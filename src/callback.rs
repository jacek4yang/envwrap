//! Host-loopback publication of application TCP listeners (OAuth/dev callbacks).
//!
//! Port discovery is bounded polling, not interception of bind/listen syscalls.
//! A newly opened port can take up to one discovery interval to appear on the
//! host. Host port conflicts fail the session instead of routing to the wrong app.

use std::collections::{HashMap, HashSet};
use std::io::{self, BufReader, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::{
    kernel, relay,
    transport::{self, Cancel},
    wire::{self, Endpoint},
};

pub struct Running {
    cancel: Arc<Cancel>,
    thread: Option<JoinHandle<io::Result<()>>>,
}
impl Running {
    fn spawn(
        work: impl FnOnce(Arc<Cancel>) -> io::Result<()> + Send + 'static,
    ) -> io::Result<Self> {
        let cancel = Cancel::new()?;
        let worker = Arc::clone(&cancel);
        let thread = thread::Builder::new()
            .name("envwrap-callbacks".into())
            .spawn(move || work(worker))?;
        Ok(Self {
            cancel,
            thread: Some(thread),
        })
    }
    pub fn finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }
    pub fn shutdown(mut self) -> io::Result<()> {
        self.stop()
    }
    fn stop(&mut self) -> io::Result<()> {
        self.cancel.trigger();
        let result = self.thread.take().map_or(Ok(()), |worker| {
            worker
                .join()
                .map_err(|_| io::Error::other("callback worker panicked"))?
        });
        match result {
            Err(e) if e.kind() == io::ErrorKind::ConnectionAborted => Ok(()),
            result => result,
        }
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

pub fn namespace(path: &Path, token: String, excluded: [SocketAddr; 2]) -> io::Result<Running> {
    let listener = UnixListener::bind(path)?;
    listener.set_nonblocking(true)?;
    Running::spawn(move |cancel| {
        let mut workers: Vec<JoinHandle<()>> = Vec::new();
        let result = (|| {
            loop {
                cancel.wait(listener.as_raw_fd(), libc::POLLIN, None)?;
                let (socket, _) = match listener.accept() {
                    Ok(value) => value,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                    Err(e) => return Err(e),
                };
                workers.retain(|worker| !worker.is_finished());
                if workers.len() >= 128 {
                    continue;
                }
                let cancel = Arc::clone(&cancel);
                let expected = format!("AUTH {token}");
                workers.push(thread::spawn(move || {
                    let _ = (|| -> io::Result<()> {
                        let mut stream =
                            BufReader::new(transport::unix(socket, Arc::clone(&cancel))?);
                        if wire::line(&mut stream, 128)? != expected {
                            return Err(io::ErrorKind::PermissionDenied.into());
                        }
                        let command = wire::line(&mut stream, 128)?;
                        if command == "LIST" {
                            let listeners = listeners(excluded)?;
                            stream.get_mut().write_all(b"OK\n")?;
                            for address in listeners {
                                writeln!(stream.get_mut(), "{address}")?;
                            }
                            stream.get_mut().write_all(b".\n")?;
                            return Ok(());
                        }
                        let target: SocketAddr = command
                            .strip_prefix("TCP ")
                            .ok_or(io::ErrorKind::InvalidInput)?
                            .parse()
                            .map_err(|_| io::ErrorKind::InvalidInput)?;
                        if !target.ip().is_loopback()
                            || excluded.contains(&target)
                            || target.port() == 53
                        {
                            return Err(io::ErrorKind::PermissionDenied.into());
                        }
                        let mut destination = BufReader::new(transport::connect(
                            target,
                            Arc::clone(&cancel),
                            wire::SETUP_TIMEOUT,
                            Some(kernel::LOCAL_MARK),
                        )?);
                        stream.get_mut().write_all(b"OK\n")?;
                        relay::streams(&mut stream, &mut destination, &cancel)
                    })();
                }));
            }
        })();
        cancel.trigger();
        for worker in workers {
            let _ = worker.join();
        }
        result
    })
}

fn listeners(excluded: [SocketAddr; 2]) -> io::Result<HashSet<SocketAddr>> {
    let mut addresses = HashSet::new();
    for (path, ipv6) in [("/proc/net/tcp", false), ("/proc/net/tcp6", true)] {
        let text = std::fs::read_to_string(path)?;
        for line in text.lines().skip(1) {
            let mut fields = line.split_whitespace();
            fields.next();
            let Some(local) = fields.next() else {
                continue;
            };
            fields.next();
            if fields.next() != Some("0A") {
                continue;
            }
            let (ip, port) = local.split_once(':').ok_or(io::ErrorKind::InvalidData)?;
            let port = u16::from_str_radix(port, 16).map_err(|_| io::ErrorKind::InvalidData)?;
            if port == 53 {
                continue;
            }
            let address = if ipv6 {
                if ip.len() != 32 {
                    return Err(io::ErrorKind::InvalidData.into());
                }
                let mut bytes = [0; 16];
                for (index, word) in ip.as_bytes().as_chunks::<8>().0.iter().enumerate() {
                    let value = u32::from_str_radix(
                        std::str::from_utf8(word).map_err(|_| io::ErrorKind::InvalidData)?,
                        16,
                    )
                    .map_err(|_| io::ErrorKind::InvalidData)?;
                    bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_ne_bytes());
                }
                IpAddr::V6(Ipv6Addr::from(bytes))
            } else {
                IpAddr::V4(Ipv4Addr::from(
                    u32::from_str_radix(ip, 16)
                        .map_err(|_| io::ErrorKind::InvalidData)?
                        .to_ne_bytes(),
                ))
            };
            let address = if address.is_unspecified() {
                if ipv6 {
                    IpAddr::V6(Ipv6Addr::LOCALHOST)
                } else {
                    IpAddr::V4(Ipv4Addr::LOCALHOST)
                }
            } else {
                address
            };
            if !address.is_loopback() {
                continue;
            }
            let address = SocketAddr::new(address, port);
            if !excluded.contains(&address) {
                addresses.insert(address);
            }
            if addresses.len() > 128 {
                return Err(io::Error::other(
                    "more than 128 application loopback listeners",
                ));
            }
        }
    }
    Ok(addresses)
}

fn request(
    endpoint: &Endpoint,
    command: &str,
    cancel: Arc<Cancel>,
) -> io::Result<BufReader<transport::Channel<std::os::unix::net::UnixStream>>> {
    let mut socket = transport::unix_connect(&endpoint.socket, cancel)?;
    write!(socket, "AUTH {}\n{command}\n", endpoint.token)?;
    let mut reader = BufReader::new(socket);
    if wire::line(&mut reader, 128)? != "OK" {
        return Err(io::ErrorKind::ConnectionRefused.into());
    }
    Ok(reader)
}
fn bind(address: SocketAddr) -> io::Result<TcpListener> {
    let socket = socket2::Socket::new(
        socket2::Domain::for_address(address),
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )?;
    socket.set_reuse_address(true)?;
    if address.is_ipv6() {
        socket.set_only_v6(true)?;
    }
    socket.bind(&address.into()).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("host callback port {address} unavailable: {error}"),
        )
    })?;
    socket.listen(128)?;
    socket.set_nonblocking(true)?;
    Ok(socket.into())
}

pub fn host(endpoint: Endpoint) -> io::Result<Running> {
    Running::spawn(move |cancel| {
        let mut ports: HashMap<SocketAddr, TcpListener> = HashMap::new();
        let mut workers: Vec<JoinHandle<()>> = Vec::new();
        let result = (|| {
            while !cancel.stopped() {
                let mut response = request(&endpoint, "LIST", Arc::clone(&cancel))?;
                let mut desired = HashSet::new();
                loop {
                    let line = wire::line(&mut response, 128)?;
                    if line == "." {
                        break;
                    }
                    let address: SocketAddr =
                        line.parse().map_err(|_| io::ErrorKind::InvalidData)?;
                    if !address.ip().is_loopback() || desired.len() >= 128 {
                        return Err(io::ErrorKind::InvalidData.into());
                    }
                    desired.insert(address);
                }
                ports.retain(|address, _| desired.contains(address));
                for address in desired {
                    if let std::collections::hash_map::Entry::Vacant(entry) = ports.entry(address) {
                        entry.insert(bind(address)?);
                    }
                }
                workers.retain(|worker| !worker.is_finished());
                for (address, listener) in &ports {
                    loop {
                        let (mut socket, _) = match listener.accept() {
                            Ok(value) => value,
                            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                            Err(e) => return Err(e),
                        };
                        if workers.len() >= 128 {
                            continue;
                        }
                        let endpoint = Endpoint {
                            socket: endpoint.socket.clone(),
                            token: endpoint.token.clone(),
                        };
                        let target = *address;
                        let cancel = Arc::clone(&cancel);
                        workers.push(thread::spawn(move || {
                            if let Ok(mut destination) =
                                request(&endpoint, &format!("TCP {target}"), Arc::clone(&cancel))
                            {
                                let _ = relay::streams(&mut socket, &mut destination, &cancel);
                            }
                        }));
                    }
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(())
        })();
        cancel.trigger();
        for worker in workers {
            let _ = worker.join();
        }
        result
    })
}
