//! Host egress broker. Only explicit direct exceptions bypass the upstream.

use std::io::{self, BufReader, Write};
use std::net::{SocketAddr, UdpSocket};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use ipnet::IpNet;

use crate::dns::Resolver;
use crate::transport::{self, Cancel, Channel};
use crate::upstream::Upstream;
use crate::wire::{self, Endpoint, FrameReader, FrameWriter};

pub struct Settings {
    pub upstream: Upstream,
    pub dns: Resolver,
    pub direct: Vec<IpNet>,
}
impl Settings {
    pub fn direct(&self, address: std::net::IpAddr) -> bool {
        address.is_loopback() || self.direct.iter().any(|range| range.contains(&address))
    }
    pub fn validate(&self) -> Result<(), String> {
        for address in self.dns.local_addresses() {
            if !self.direct(address.ip()) {
                return Err("internal DNS resolver must be host loopback or included in an explicit direct range".into());
            }
        }
        Ok(())
    }
}

pub struct Broker {
    socket: PathBuf,
    cancel: Arc<Cancel>,
    thread: Option<JoinHandle<io::Result<()>>>,
}
impl Broker {
    pub fn start(directory: &Path, settings: Arc<Settings>) -> io::Result<(Self, Endpoint)> {
        settings.validate().map_err(io::Error::other)?;
        let metadata = directory.symlink_metadata()?;
        // SAFETY: getuid has no pointer arguments or side effects.
        if !metadata.is_dir()
            || metadata.mode() & 0o077 != 0
            || metadata.uid() != unsafe { libc::getuid() }
        {
            return Err(io::Error::other(
                "broker requires a private, user-owned runtime directory",
            ));
        }
        let socket = directory.join("gateway.sock");
        let listener = UnixListener::bind(&socket)?;
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let token = crate::profile::nonce()?;
        let expected = format!("AUTH {token}");
        let cancel = Cancel::new()?;
        let worker_cancel = Arc::clone(&cancel);
        let thread = thread::Builder::new()
            .name("envwrap-gateway".into())
            .spawn(move || {
                let mut workers: Vec<JoinHandle<()>> = Vec::new();
                let result = (|| {
                    while !worker_cancel.stopped() {
                        worker_cancel.wait(listener.as_raw_fd(), libc::POLLIN, None)?;
                        loop {
                            let socket = match listener.accept() {
                                Ok((socket, _)) => socket,
                                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                                    continue;
                                }
                                Err(error) => return Err(error),
                            };
                            workers.retain(|worker| !worker.is_finished());
                            if workers.len() >= 256 {
                                drop(socket);
                                continue;
                            }
                            let settings = Arc::clone(&settings);
                            let cancel = Arc::clone(&worker_cancel);
                            let expected = expected.clone();
                            workers.push(
                                thread::Builder::new().name("envwrap-egress".into()).spawn(
                                    move || {
                                        let _ = serve(socket, &expected, &settings, cancel);
                                    },
                                )?,
                            );
                        }
                    }
                    Ok(())
                })();
                worker_cancel.trigger();
                for worker in workers {
                    let _ = worker.join();
                }
                if worker_cancel.stopped()
                    && result
                        .as_ref()
                        .is_err_and(|error| error.kind() == io::ErrorKind::ConnectionAborted)
                {
                    Ok(())
                } else {
                    result
                }
            })?;
        Ok((
            Self {
                socket: socket.clone(),
                cancel,
                thread: Some(thread),
            },
            Endpoint { socket, token },
        ))
    }
    pub fn finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }
    fn stop(&mut self) -> io::Result<()> {
        self.cancel.trigger();
        let result = self.thread.take().map_or(Ok(()), |thread| {
            thread
                .join()
                .map_err(|_| io::Error::other("gateway worker panicked"))?
        });
        let cleanup = std::fs::remove_file(&self.socket);
        result?;
        match cleanup {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }
    pub fn shutdown(mut self) -> io::Result<()> {
        self.stop()
    }
}
impl Drop for Broker {
    fn drop(&mut self) {
        if self.thread.is_some() {
            let _ = self.stop();
        }
    }
}

enum Request {
    Tcp(SocketAddr),
    TcpDns,
    Udp(SocketAddr),
    Dns,
}
fn metadata(
    client: &mut impl std::io::BufRead,
    expected: &str,
    settings: &Settings,
) -> io::Result<Request> {
    if wire::line(client, 128)? != expected {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let command = wire::line(client, 256)?;
    if command == "DNS" {
        return Ok(Request::Dns);
    }
    let (verb, address) = command
        .split_once(' ')
        .ok_or_else(|| io::Error::other("invalid gateway command"))?;
    let address: SocketAddr = address
        .parse()
        .map_err(|_| io::Error::other("invalid gateway target"))?;
    if address.port() == 0 || address.ip().is_unspecified() || address.ip().is_multicast() {
        return Err(io::Error::other("gateway target must be unicast"));
    }
    match verb {
        "TCP" if address.port() == 53 => Ok(Request::TcpDns),
        "TCP" => Ok(Request::Tcp(address)),
        "UDP" if address.port() != 53 && settings.direct(address.ip()) => Ok(Request::Udp(address)),
        _ => Err(io::ErrorKind::PermissionDenied.into()),
    }
}
fn refused(client: &mut Channel<UnixStream>) -> io::Result<()> {
    client.bounded(wire::SETUP_TIMEOUT);
    client.write_all(b"ERR gateway request refused\n")
}
fn serve(
    socket: UnixStream,
    expected: &str,
    settings: &Settings,
    cancel: Arc<Cancel>,
) -> io::Result<()> {
    let mut client = BufReader::new(transport::unix(socket, Arc::clone(&cancel))?);
    let request = match metadata(&mut client, expected, settings) {
        Ok(request) => request,
        Err(_) => return refused(client.get_mut()),
    };
    match request {
        Request::Tcp(target) => {
            let upstream = if crate::domain::reserved(target.ip()) {
                if !settings.upstream.remote_dns() {
                    return refused(client.get_mut());
                }
                match settings.dns.domains.lookup(target.ip()) {
                    Ok(name) => {
                        settings
                            .upstream
                            .connect_name(&name, target.port(), Arc::clone(&cancel))
                    }
                    Err(_) => return refused(client.get_mut()),
                }
            } else if settings.direct(target.ip()) {
                transport::connect(target, Arc::clone(&cancel), wire::SETUP_TIMEOUT, None)
                    .map(BufReader::new)
            } else {
                settings.upstream.connect(target, Arc::clone(&cancel))
            };
            let mut upstream = match upstream {
                Ok(upstream) => upstream,
                Err(_) => return refused(client.get_mut()),
            };
            client.get_mut().write_all(b"OK\n")?;
            crate::relay::streams(&mut client, &mut upstream, &cancel)
        }
        Request::TcpDns => {
            client.get_mut().write_all(b"OK\n")?;
            crate::dns::serve(&mut client, &settings.dns, &settings.upstream, &cancel)
        }
        Request::Dns => {
            client.get_mut().write_all(b"OK\n")?;
            client.get_mut().bounded(wire::SETUP_TIMEOUT);
            let query = wire::read_datagram(&mut client)?;
            let response = settings
                .dns
                .lookup_udp(&query, &settings.upstream, cancel)?;
            wire::write_datagram(client.get_mut(), &response)
        }
        Request::Udp(target) => {
            let bind = if target.is_ipv4() {
                "0.0.0.0:0"
            } else {
                "[::]:0"
            };
            let upstream = UdpSocket::bind(bind)?;
            upstream.set_nonblocking(true)?;
            upstream.connect(target)?;
            client.get_mut().write_all(b"OK\n")?;
            udp(&mut client, &upstream, &cancel)
        }
    }
}
fn udp(
    client: &mut BufReader<Channel<UnixStream>>,
    socket: &UdpSocket,
    cancel: &Cancel,
) -> io::Result<()> {
    client.get_mut().nonblocking();
    let mut incoming = FrameReader::new();
    let mut to_udp = FrameWriter::new();
    let mut to_unix = FrameWriter::new();
    let mut bytes = [0; wire::MAX_DATAGRAM];
    loop {
        cancel.check()?;
        let mut progress = false;
        for _ in 0..32 {
            let Some(data) = incoming.read(client)? else {
                break;
            };
            to_udp.push(data);
            progress = true;
        }
        for _ in 0..32 {
            match socket.recv(&mut bytes) {
                Ok(count) => {
                    to_unix.push(&bytes[..count]);
                    progress = true;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        progress |= to_udp.send(socket)?;
        progress |= to_unix.write(client.get_mut())?;
        if progress {
            continue;
        }
        let mut descriptors = [
            libc::pollfd {
                fd: client.get_ref().as_raw_fd(),
                events: libc::POLLIN | if to_unix.is_empty() { 0 } else { libc::POLLOUT },
                revents: 0,
            },
            libc::pollfd {
                fd: socket.as_raw_fd(),
                events: libc::POLLIN | if to_udp.is_empty() { 0 } else { libc::POLLOUT },
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
