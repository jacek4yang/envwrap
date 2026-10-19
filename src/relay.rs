//! Backpressure-aware, half-close preserving stream relay without idle expiry.

use std::io::{self, BufReader, Read, Write};
use std::net::Shutdown;
use std::os::fd::AsRawFd;

use crate::transport::{Cancel, Channel};

/// Covers parsed broker streams and unbuffered accepted kernel sockets without
/// adding another BufReader allocation or losing an already-prefetched banner.
pub trait Duplex: Read {
    fn send(&mut self, bytes: &[u8]) -> io::Result<usize>;
    fn no_block(&mut self) -> io::Result<()>;
    fn descriptor(&self) -> i32;
    fn write_eof(&self) -> io::Result<()>;
}
impl<S: Read + Write + AsRawFd> Duplex for BufReader<Channel<S>> {
    fn send(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.get_mut().write(bytes)
    }
    fn no_block(&mut self) -> io::Result<()> {
        self.get_mut().nonblocking();
        Ok(())
    }
    fn descriptor(&self) -> i32 {
        self.get_ref().as_raw_fd()
    }
    fn write_eof(&self) -> io::Result<()> {
        self.get_ref().shutdown(Shutdown::Write)
    }
}
impl Duplex for std::net::TcpStream {
    fn send(&mut self, bytes: &[u8]) -> io::Result<usize> {
        Write::write(self, bytes)
    }
    fn no_block(&mut self) -> io::Result<()> {
        self.set_nonblocking(true)
    }
    fn descriptor(&self) -> i32 {
        self.as_raw_fd()
    }
    fn write_eof(&self) -> io::Result<()> {
        self.shutdown(Shutdown::Write)
    }
}
impl Duplex for crate::wire::Stream {
    fn send(&mut self, bytes: &[u8]) -> io::Result<usize> {
        Write::write(self, bytes)
    }
    fn no_block(&mut self) -> io::Result<()> {
        self.nonblocking()
    }
    fn descriptor(&self) -> i32 {
        self.as_raw_fd()
    }
    fn write_eof(&self) -> io::Result<()> {
        self.shutdown(Shutdown::Write)
    }
}

const CAPACITY: usize = 65_536;
struct Ring {
    bytes: [u8; CAPACITY],
    start: usize,
    length: usize,
    eof: bool,
    closed: bool,
}
impl Ring {
    fn new() -> Self {
        Self {
            bytes: [0; CAPACITY],
            start: 0,
            length: 0,
            eof: false,
            closed: false,
        }
    }
    fn read(&mut self, source: &mut impl Read) -> io::Result<bool> {
        if self.eof || self.length == CAPACITY {
            return Ok(false);
        }
        let end = (self.start + self.length) % CAPACITY;
        let room = (CAPACITY - self.length).min(CAPACITY - end);
        match source.read(&mut self.bytes[end..end + room]) {
            Ok(0) => {
                self.eof = true;
                Ok(true)
            }
            Ok(count) => {
                self.length += count;
                Ok(true)
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }
    fn write(&mut self, target: &mut impl Duplex) -> io::Result<bool> {
        let mut progress = false;
        if self.length != 0 {
            let end = self.start + self.length.min(CAPACITY - self.start);
            match target.send(&self.bytes[self.start..end]) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(count) => {
                    self.start = (self.start + count) % CAPACITY;
                    self.length -= count;
                    progress = true;
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error),
            }
        }
        if self.length == 0 && self.eof && !self.closed {
            target.write_eof()?;
            self.closed = true;
            progress = true;
        }
        Ok(progress)
    }
}

pub fn streams(left: &mut impl Duplex, right: &mut impl Duplex, cancel: &Cancel) -> io::Result<()> {
    left.no_block()?;
    right.no_block()?;
    let mut forward = Ring::new();
    let mut reverse = Ring::new();
    loop {
        cancel.check()?;
        let mut progress = forward.read(left)?;
        progress |= reverse.read(right)?;
        progress |= forward.write(right)?;
        progress |= reverse.write(left)?;
        if forward.closed && reverse.closed {
            return Ok(());
        }
        if progress {
            continue;
        }
        let left_events = if !forward.eof && forward.length < CAPACITY {
            libc::POLLIN
        } else {
            0
        } | if reverse.length != 0 {
            libc::POLLOUT
        } else {
            0
        };
        let right_events = if !reverse.eof && reverse.length < CAPACITY {
            libc::POLLIN
        } else {
            0
        } | if forward.length != 0 {
            libc::POLLOUT
        } else {
            0
        };
        let mut descriptors = [
            libc::pollfd {
                fd: if left_events == 0 {
                    -1
                } else {
                    left.descriptor()
                },
                events: left_events,
                revents: 0,
            },
            libc::pollfd {
                fd: if right_events == 0 {
                    -1
                } else {
                    right.descriptor()
                },
                events: right_events,
                revents: 0,
            },
            libc::pollfd {
                fd: cancel.fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: descriptors is initialized contiguous pollfd storage.
        if unsafe { libc::poll(descriptors.as_mut_ptr(), 3, -1) } < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}
