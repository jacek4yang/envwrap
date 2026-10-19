//! Rootless transparent networking using the real Linux TCP/UDP stack.
//!
//! All interfaces and firewall rules live in a fresh network namespace. TCP
//! REDIRECT preserves the original destination; UDP TPROXY delivers complete
//! native datagrams. The host broker alone owns upstream credentials/egress.

use std::collections::HashMap;
use std::io::{self, Write};
use std::net::{SocketAddr, TcpListener};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::kernel::{self, Key, UdpCapture, UdpReplies};
use crate::transport::Cancel;
use crate::wire::{self, Endpoint, FrameReader, FrameWriter, Stream};

const MAX_TCP: usize = 128;
const MAX_UDP: usize = 128;
const MAX_PENDING: usize = 64;
const UDP_IDLE: Duration = Duration::from_secs(120);

fn ip(program: &Path, arguments: &[&str]) -> io::Result<()> {
    let result = Command::new(program)
        .args(arguments)
        .env("LC_ALL", "C")
        .output()?;
    if result.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "namespace network setup: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        )))
    }
}
fn interfaces(program: &Path, mac: [u8; 6]) -> io::Result<()> {
    let mac = format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    );
    ip(program, &["link", "set", "lo", "up"])?;
    // Largest non-jumbo IPv6 UDP datagram: 40 + 65535 bytes. Larger values
    // enable kernel TCP jumbograms; smaller values require raw fragmentation.
    ip(program, &["link", "set", "lo", "mtu", "65575"])?;
    ip(
        program,
        &["link", "add", "eth0", "address", &mac, "type", "dummy"],
    )?;
    ip(program, &["addr", "add", "198.18.0.2/30", "dev", "eth0"])?;
    ip(
        program,
        &[
            "-6",
            "addr",
            "add",
            "fd23:656e:7677::2/64",
            "dev",
            "eth0",
            "nodad",
        ],
    )?;
    ip(program, &["link", "set", "eth0", "up"])?;
    ip(
        program,
        &[
            "route",
            "add",
            "default",
            "via",
            kernel::GATEWAY_V4,
            "dev",
            "eth0",
        ],
    )?;
    ip(
        program,
        &[
            "-6",
            "route",
            "add",
            "default",
            "via",
            kernel::GATEWAY_V6,
            "dev",
            "eth0",
        ],
    )?;
    ip(program, &["rule", "add", "fwmark", "3", "table", "100"])?;
    ip(
        program,
        &[
            "route",
            "add",
            "local",
            "0.0.0.0/0",
            "dev",
            "lo",
            "table",
            "100",
        ],
    )?;
    ip(
        program,
        &["-6", "rule", "add", "fwmark", "3", "table", "100"],
    )?;
    ip(
        program,
        &[
            "-6", "route", "add", "local", "::/0", "dev", "lo", "table", "100",
        ],
    )?;
    Ok(())
}
fn tcp_listener(ipv6: bool) -> io::Result<TcpListener> {
    let address: SocketAddr = if ipv6 { "[::1]:0" } else { "127.0.0.1:0" }
        .parse()
        .expect("constant TCP bind address");
    let socket = socket2::Socket::new(
        socket2::Domain::for_address(address),
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )?;
    if ipv6 {
        socket.set_only_v6(true)?;
    }
    socket.set_nonblocking(true)?;
    socket.bind(&address.into())?;
    socket.listen(128)?;
    Ok(socket.into())
}
fn firewall(program: &Path, tcp: &[SocketAddr; 2], udp: &[UdpCapture; 2]) -> io::Result<()> {
    let rules = format!(
        "table inet envwrap {{\n chain ew_output {{ type route hook output priority mangle; policy accept; meta mark 1 ct mark set 1; ct mark 1 meta mark set 1; meta l4proto udp meta mark 0 meta mark set 3; }}\n chain ew_nat {{ type nat hook output priority dstnat; policy accept; meta l4proto tcp meta mark 0 meta nfproto ipv4 redirect to :{}; meta l4proto tcp meta mark 0 meta nfproto ipv6 redirect to :{}; }}\n chain ew_udp {{ type filter hook prerouting priority mangle; policy accept; meta mark 3 meta l4proto udp udp dport != 53 fib daddr type local socket transparent 0 return; meta mark 3 meta l4proto udp meta nfproto ipv4 tproxy ip to :{} accept; meta mark 3 meta l4proto udp meta nfproto ipv6 tproxy ip6 to :{} accept; }}\n}}\n",
        tcp[0].port(),
        tcp[1].port(),
        udp[0].port()?,
        udp[1].port()?
    );
    let mut child = Command::new(program)
        .args(["-f", "-"])
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .expect("piped nft stdin")
        .write_all(rules.as_bytes())?;
    let result = child.wait_with_output()?;
    if result.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "namespace transparent proxy rules: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        )))
    }
}

pub struct Running {
    cancel: Arc<Cancel>,
    listeners: [SocketAddr; 2],
    thread: Option<JoinHandle<io::Result<()>>>,
}
impl Running {
    pub fn start(endpoint: Arc<Endpoint>, ip: &Path, nft: &Path, mac: [u8; 6]) -> io::Result<Self> {
        interfaces(ip, mac)?;
        let tcp = [tcp_listener(false)?, tcp_listener(true)?];
        let listeners = [tcp[0].local_addr()?, tcp[1].local_addr()?];
        let udp = [UdpCapture::new(false)?, UdpCapture::new(true)?];
        let replies = UdpReplies::new()?;
        let cancel = Cancel::new()?;
        let wake = Wake::new()?;
        firewall(nft, &listeners, &udp)?;
        let thread_cancel = Arc::clone(&cancel);
        let thread = thread::Builder::new()
            .name("envwrap-network".into())
            .spawn(move || {
                let mut engine = Engine::new(endpoint, tcp, udp, replies, thread_cancel, wake);
                let result = engine.run();
                let stopped = engine.cancel.stopped();
                engine.cancel.trigger();
                for worker in engine.workers {
                    let _ = worker.join();
                }
                if stopped
                    && result
                        .as_ref()
                        .is_err_and(|e| e.kind() == io::ErrorKind::ConnectionAborted)
                {
                    Ok(())
                } else {
                    result
                }
            })?;
        Ok(Self {
            cancel,
            listeners,
            thread: Some(thread),
        })
    }
    pub fn listener_addresses(&self) -> [SocketAddr; 2] {
        self.listeners
    }
    pub fn finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }
    fn stop(&mut self) -> io::Result<()> {
        self.cancel.trigger();
        self.thread.take().map_or(Ok(()), |thread| {
            thread
                .join()
                .map_err(|_| io::Error::other("network worker panicked"))?
        })
    }
    pub fn shutdown(mut self) -> io::Result<()> {
        self.stop()
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        if self.thread.is_some() {
            let _ = self.stop();
        }
    }
}

struct Wake(OwnedFd);
impl Wake {
    fn new() -> io::Result<Arc<Self>> {
        // SAFETY: a successful eventfd call returns one newly owned descriptor.
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Arc::new(Self(unsafe { OwnedFd::from_raw_fd(fd) })))
    }
    fn notify(&self) {
        let one = 1u64;
        // SAFETY: eventfd consumes exactly one initialized native u64.
        unsafe {
            libc::write(self.0.as_raw_fd(), (&one as *const u64).cast(), 8);
        }
    }
    fn drain(&self) {
        let mut count = 0u64;
        // SAFETY: count is writable for the eventfd's fixed eight-byte result.
        unsafe {
            libc::read(self.0.as_raw_fd(), (&mut count as *mut u64).cast(), 8);
        }
    }
}
struct UdpFlow {
    id: u64,
    stream: Option<Stream>,
    reader: FrameReader,
    writer: FrameWriter,
    used: Instant,
}
enum Event {
    Open {
        key: Key,
        id: u64,
        result: io::Result<Stream>,
    },
    Dns {
        key: Key,
        result: io::Result<Vec<u8>>,
    },
}
struct Engine {
    endpoint: Arc<Endpoint>,
    tcp: [TcpListener; 2],
    udp: [UdpCapture; 2],
    replies: UdpReplies,
    cancel: Arc<Cancel>,
    wake: Arc<Wake>,
    flows: HashMap<Key, UdpFlow>,
    serial: u64,
    pending: Arc<AtomicUsize>,
    tx: mpsc::Sender<Event>,
    rx: mpsc::Receiver<Event>,
    workers: Vec<JoinHandle<()>>,
}
impl Engine {
    fn new(
        endpoint: Arc<Endpoint>,
        tcp: [TcpListener; 2],
        udp: [UdpCapture; 2],
        replies: UdpReplies,
        cancel: Arc<Cancel>,
        wake: Arc<Wake>,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            endpoint,
            tcp,
            udp,
            replies,
            cancel,
            wake,
            flows: HashMap::new(),
            serial: 0,
            pending: Arc::new(AtomicUsize::new(0)),
            tx,
            rx,
            workers: Vec::new(),
        }
    }
    fn asynchronous(&mut self, work: impl FnOnce() -> Event + Send + 'static) -> bool {
        if self
            .pending
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < MAX_PENDING).then_some(n + 1)
            })
            .is_err()
        {
            return false;
        }
        let pending = Arc::clone(&self.pending);
        let tx = self.tx.clone();
        let wake = Arc::clone(&self.wake);
        self.workers.retain(|worker| !worker.is_finished());
        match thread::Builder::new()
            .name("envwrap-datagram".into())
            .spawn(move || {
                let result = work();
                pending.fetch_sub(1, Ordering::Release);
                if tx.send(result).is_ok() {
                    wake.notify();
                }
            }) {
            Ok(worker) => {
                self.workers.push(worker);
                true
            }
            Err(_) => {
                self.pending.fetch_sub(1, Ordering::Release);
                false
            }
        }
    }
    fn accept(&mut self, index: usize) -> io::Result<()> {
        for _ in 0..32 {
            let (mut socket, _) = match self.tcp[index].accept() {
                Ok(connection) => connection,
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) =>
                {
                    break;
                }
                Err(e) => return Err(e),
            };
            self.workers.retain(|worker| !worker.is_finished());
            if self.workers.len() >= MAX_TCP + MAX_PENDING {
                continue;
            }
            let target = kernel::original_destination(&socket)?;
            if self
                .tcp
                .iter()
                .any(|listener| listener.local_addr().ok() == Some(target))
            {
                continue;
            }
            socket.set_nodelay(true)?;
            let endpoint = Arc::clone(&self.endpoint);
            let cancel = Arc::clone(&self.cancel);
            self.workers
                .push(
                    thread::Builder::new()
                        .name("envwrap-tcp".into())
                        .spawn(move || {
                            if let Ok(mut stream) =
                                wire::tcp(&endpoint, target, Arc::clone(&cancel))
                            {
                                let _ = crate::relay::streams(&mut socket, &mut stream, &cancel);
                            }
                        })?,
                );
        }
        Ok(())
    }
    fn packet(&mut self, key: Key, data: &[u8]) {
        if key.target.port() == 53 {
            let endpoint = Arc::clone(&self.endpoint);
            let cancel = Arc::clone(&self.cancel);
            let data = data.to_vec();
            self.asynchronous(move || Event::Dns {
                key,
                result: wire::dns(&endpoint, &data, cancel),
            });
            return;
        }
        if !self.flows.contains_key(&key) {
            if self.flows.len() >= MAX_UDP {
                return;
            }
            self.serial = self.serial.wrapping_add(1);
            let id = self.serial;
            self.flows.insert(
                key,
                UdpFlow {
                    id,
                    stream: None,
                    reader: FrameReader::new(),
                    writer: FrameWriter::new(),
                    used: Instant::now(),
                },
            );
            let endpoint = Arc::clone(&self.endpoint);
            let cancel = Arc::clone(&self.cancel);
            if !self.asynchronous(move || {
                let result = wire::udp(&endpoint, key.target, cancel).and_then(|mut stream| {
                    stream.nonblocking()?;
                    Ok(stream)
                });
                Event::Open { key, id, result }
            }) {
                self.flows.remove(&key);
                return;
            }
        }
        let flow = self.flows.get_mut(&key).expect("UDP flow");
        if flow.writer.push(data) {
            flow.used = Instant::now();
        }
    }
    fn progress(&mut self) -> io::Result<()> {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::Dns {
                    key,
                    result: Ok(data),
                } => self.replies.send(key, &data)?,
                Event::Dns { result: Err(_), .. } => {}
                Event::Open { key, id, result } => {
                    if self.flows.get(&key).is_some_and(|flow| flow.id == id) {
                        match result {
                            Ok(stream) => {
                                self.flows.get_mut(&key).expect("UDP flow").stream = Some(stream)
                            }
                            Err(_) => {
                                self.flows.remove(&key);
                            }
                        }
                    }
                }
            }
        }
        let mut failure = None;
        self.flows.retain(|key, flow| {
            if flow.used.elapsed() >= UDP_IDLE {
                return false;
            }
            let Some(stream) = &mut flow.stream else {
                return true;
            };
            if flow.writer.write(stream).is_err() {
                return false;
            }
            for _ in 0..32 {
                match flow.reader.read(stream) {
                    Ok(Some(data)) => {
                        if let Err(error) = self.replies.send(*key, data) {
                            failure = Some(error);
                            return false;
                        }
                        flow.used = Instant::now();
                    }
                    Ok(None) => break,
                    Err(_) => return false,
                }
            }
            true
        });
        failure.map_or(Ok(()), Err)
    }
    fn run(&mut self) -> io::Result<()> {
        let mut descriptors = Vec::with_capacity(MAX_UDP + 6);
        let mut bytes = vec![0; wire::MAX_DATAGRAM];
        loop {
            self.cancel.check()?;
            self.progress()?;
            descriptors.clear();
            descriptors.push(libc::pollfd {
                fd: self.cancel.fd(),
                events: libc::POLLIN,
                revents: 0,
            });
            descriptors.push(libc::pollfd {
                fd: self.wake.0.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
            for listener in &self.tcp {
                descriptors.push(libc::pollfd {
                    fd: listener.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                });
            }
            for socket in &self.udp {
                descriptors.push(libc::pollfd {
                    fd: socket.fd(),
                    events: libc::POLLIN,
                    revents: 0,
                });
            }
            let mut timeout = -1;
            for flow in self.flows.values() {
                let remaining = UDP_IDLE
                    .saturating_sub(flow.used.elapsed())
                    .as_millis()
                    .saturating_add(1)
                    .min(i32::MAX as u128) as i32;
                timeout = if timeout < 0 {
                    remaining
                } else {
                    timeout.min(remaining)
                };
                if let Some(stream) = &flow.stream {
                    if stream.buffered() {
                        timeout = 0;
                    }
                    descriptors.push(libc::pollfd {
                        fd: stream.as_raw_fd(),
                        events: libc::POLLIN
                            | if flow.writer.is_empty() {
                                0
                            } else {
                                libc::POLLOUT
                            },
                        revents: 0,
                    });
                }
            }
            // SAFETY: descriptors is initialized contiguous pollfd storage.
            let result =
                unsafe { libc::poll(descriptors.as_mut_ptr(), descriptors.len() as _, timeout) };
            if result < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                return Err(io::Error::last_os_error());
            }
            self.cancel.check()?;
            if descriptors[1].revents & libc::POLLIN != 0 {
                self.wake.drain();
            }
            for index in 0..2 {
                if descriptors[index + 2].revents & libc::POLLIN != 0 {
                    self.accept(index)?;
                }
                if descriptors[index + 4].revents & libc::POLLIN != 0 {
                    for _ in 0..32 {
                        let Some((key, data)) = self.udp[index].receive(&mut bytes)? else {
                            break;
                        };
                        self.packet(key, data);
                    }
                }
            }
        }
    }
}
