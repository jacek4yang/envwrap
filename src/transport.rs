//! Cancelable sockets: bounded setup, no arbitrary established-session timeout.

use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub struct Cancel {
    fd: OwnedFd,
    stopped: AtomicBool,
}
impl Cancel {
    pub fn new() -> io::Result<Arc<Self>> {
        // SAFETY: eventfd returns one new owned descriptor on success.
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Arc::new(Self {
            fd: unsafe { OwnedFd::from_raw_fd(fd) },
            stopped: AtomicBool::new(false),
        }))
    }
    pub fn fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
    pub fn stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }
    pub fn trigger(&self) {
        if !self.stopped.swap(true, Ordering::AcqRel) {
            let one: u64 = 1;
            // SAFETY: the eventfd consumes a u64. Do not drain this fd: every
            // session waiter must observe the same persistent cancellation.
            unsafe {
                libc::write(self.fd(), (&one as *const u64).cast(), 8);
            }
        }
    }
    pub fn check(&self) -> io::Result<()> {
        if self.stopped() {
            Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "session ended",
            ))
        } else {
            Ok(())
        }
    }
    pub fn wait(&self, fd: RawFd, events: i16, deadline: Option<Instant>) -> io::Result<()> {
        let mut descriptors = [
            libc::pollfd {
                fd,
                events,
                revents: 0,
            },
            libc::pollfd {
                fd: self.fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        loop {
            self.check()?;
            let timeout = if let Some(deadline) = deadline {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "socket setup timed out",
                    ));
                }
                left.as_millis().saturating_add(1).min(i32::MAX as u128) as i32
            } else {
                -1
            };
            // SAFETY: descriptors is initialized contiguous pollfd storage.
            let count = unsafe { libc::poll(descriptors.as_mut_ptr(), 2, timeout) };
            self.check()?;
            if count > 0 {
                return Ok(());
            }
            if count == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "socket setup timed out",
                ));
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Mode {
    Bounded(Instant),
    Wait,
    Nonblocking,
}

pub struct Channel<S> {
    socket: S,
    cancel: Arc<Cancel>,
    mode: Mode,
}
impl<S: AsRawFd> Channel<S> {
    pub fn new(socket: S, cancel: Arc<Cancel>) -> Self {
        Self {
            socket,
            cancel,
            mode: Mode::Wait,
        }
    }
    pub fn bounded(&mut self, duration: Duration) {
        self.mode = Mode::Bounded(Instant::now() + duration);
    }
    pub fn idle(&mut self) {
        self.mode = Mode::Wait;
    }
    pub fn nonblocking(&mut self) {
        self.mode = Mode::Nonblocking;
    }
    fn ready(&self, events: i16) -> io::Result<()> {
        match self.mode {
            Mode::Nonblocking => Err(io::ErrorKind::WouldBlock.into()),
            Mode::Wait => self.cancel.wait(self.as_raw_fd(), events, None),
            Mode::Bounded(deadline) => self.cancel.wait(self.as_raw_fd(), events, Some(deadline)),
        }
    }
    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        let how = match how {
            Shutdown::Read => libc::SHUT_RD,
            Shutdown::Write => libc::SHUT_WR,
            Shutdown::Both => libc::SHUT_RDWR,
        };
        // SAFETY: socket owns a valid stream socket descriptor.
        if unsafe { libc::shutdown(self.as_raw_fd(), how) } < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}
impl<S: AsRawFd> AsRawFd for Channel<S> {
    fn as_raw_fd(&self) -> RawFd {
        self.socket.as_raw_fd()
    }
}
impl<S: Read + AsRawFd> Read for Channel<S> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        loop {
            self.cancel.check()?;
            match self.socket.read(bytes) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    self.ready(libc::POLLIN)?
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => return result,
            }
        }
    }
}
impl<S: Write + AsRawFd> Write for Channel<S> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        loop {
            self.cancel.check()?;
            match self.socket.write(bytes) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    self.ready(libc::POLLOUT)?
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => return result,
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        self.socket.flush()
    }
}

pub fn connect(
    address: SocketAddr,
    cancel: Arc<Cancel>,
    timeout: Duration,
    mark: Option<u32>,
) -> io::Result<Channel<TcpStream>> {
    cancel.check()?;
    let socket = socket2::Socket::new(
        socket2::Domain::for_address(address),
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )?;
    socket.set_nonblocking(true)?;
    if let Some(mark) = mark {
        socket.set_mark(mark)?;
    }
    finish_connect(&socket, &address.into(), &cancel, timeout)?;
    socket.set_tcp_nodelay(true)?;
    let mut channel = Channel::new(TcpStream::from(socket), cancel);
    channel.bounded(timeout);
    Ok(channel)
}
fn finish_connect(
    socket: &socket2::Socket,
    address: &socket2::SockAddr,
    cancel: &Cancel,
    timeout: Duration,
) -> io::Result<()> {
    match socket.connect(address) {
        Ok(()) => {}
        Err(error)
            if matches!(
                error.raw_os_error(),
                Some(libc::EINPROGRESS | libc::EALREADY | libc::EINTR)
            ) =>
        {
            cancel.wait(
                socket.as_raw_fd(),
                libc::POLLOUT,
                Some(Instant::now() + timeout),
            )?;
            if let Some(error) = socket.take_error()? {
                return Err(error);
            }
        }
        Err(error) => return Err(error),
    }
    Ok(())
}

pub fn unix(socket: UnixStream, cancel: Arc<Cancel>) -> io::Result<Channel<UnixStream>> {
    socket.set_nonblocking(true)?;
    let mut channel = Channel::new(socket, cancel);
    channel.bounded(crate::wire::SETUP_TIMEOUT);
    Ok(channel)
}

pub fn unix_connect(
    path: &std::path::Path,
    cancel: Arc<Cancel>,
) -> io::Result<Channel<UnixStream>> {
    cancel.check()?;
    let socket = socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None)?;
    socket.set_nonblocking(true)?;
    finish_connect(
        &socket,
        &socket2::SockAddr::unix(path)?,
        &cancel,
        crate::wire::SETUP_TIMEOUT,
    )?;
    unix(socket.into(), cancel)
}
