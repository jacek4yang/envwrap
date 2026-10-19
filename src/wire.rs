//! Private, authenticated host/namespace transport. No upstream credential crosses this boundary.

use crate::transport::{self, Cancel, Channel};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub const SETUP_TIMEOUT: Duration = Duration::from_secs(10);
pub const MAX_DATAGRAM: usize = 65_527;

pub struct Endpoint {
    pub socket: PathBuf,
    pub token: String,
}

pub enum Stream {
    Host(BufReader<Channel<UnixStream>>),
    Local(Channel<TcpStream>),
}

impl AsRawFd for Stream {
    fn as_raw_fd(&self) -> RawFd {
        match self {
            Self::Host(s) => s.get_ref().as_raw_fd(),
            Self::Local(s) => s.as_raw_fd(),
        }
    }
}
impl Read for Stream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Host(s) => s.read(bytes),
            Self::Local(s) => s.read(bytes),
        }
    }
}
impl Write for Stream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self {
            Self::Host(s) => s.get_mut().write(bytes),
            Self::Local(s) => s.write(bytes),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Host(s) => s.get_mut().flush(),
            Self::Local(s) => s.flush(),
        }
    }
}
impl Stream {
    pub fn nonblocking(&mut self) -> io::Result<()> {
        match self {
            Self::Host(s) => s.get_mut().nonblocking(),
            Self::Local(s) => s.nonblocking(),
        }
        Ok(())
    }
    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        match self {
            Self::Host(s) => s.get_ref().shutdown(how),
            Self::Local(s) => s.shutdown(how),
        }
    }
    pub fn buffered(&self) -> bool {
        matches!(self, Self::Host(s) if !s.buffer().is_empty())
    }
}

/// Read bounded protocol metadata, never unbounded application payload.
pub fn line(reader: &mut impl BufRead, cap: usize) -> io::Result<String> {
    let mut value = Vec::new();
    loop {
        let bytes = reader.fill_buf()?;
        if bytes.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "control connection closed",
            ));
        }
        let count = bytes
            .iter()
            .position(|&b| b == b'\n')
            .map_or(bytes.len(), |n| n + 1);
        if count > cap.saturating_sub(value.len()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "control line too large",
            ));
        }
        value.extend_from_slice(&bytes[..count]);
        reader.consume(count);
        if value.last() == Some(&b'\n') {
            value.pop();
            if value.last() == Some(&b'\r') {
                value.pop();
            }
            return String::from_utf8(value).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid control encoding")
            });
        }
    }
}

fn request(endpoint: &Endpoint, command: &str, cancel: Arc<Cancel>) -> io::Result<Stream> {
    let mut socket = transport::unix_connect(&endpoint.socket, cancel)?;
    write!(socket, "AUTH {}\n{command}\n", endpoint.token)?;
    let mut reader = BufReader::new(socket);
    if line(&mut reader, 256)? != "OK" {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            "host gateway refused the request",
        ));
    }
    Ok(Stream::Host(reader))
}

pub fn tcp(endpoint: &Endpoint, target: SocketAddr, cancel: Arc<Cancel>) -> io::Result<Stream> {
    // Marked local sockets bypass capture. Their conntrack mark also lets a
    // local server's reply use the real kernel connection, not the IP stack.
    if crate::kernel::namespace_local(target.ip()) && target.port() != 53 {
        match transport::connect(
            target,
            Arc::clone(&cancel),
            Duration::from_millis(500),
            Some(crate::kernel::LOCAL_MARK),
        ) {
            Ok(stream) => return Ok(Stream::Local(stream)),
            Err(error)
                if target.ip().is_loopback()
                    && error.kind() == io::ErrorKind::ConnectionRefused => {}
            Err(error) => return Err(error),
        }
    }
    request(endpoint, &format!("TCP {target}"), cancel)
}

pub fn udp(endpoint: &Endpoint, target: SocketAddr, cancel: Arc<Cancel>) -> io::Result<Stream> {
    request(endpoint, &format!("UDP {target}"), cancel)
}

pub fn dns(endpoint: &Endpoint, query: &[u8], cancel: Arc<Cancel>) -> io::Result<Vec<u8>> {
    let mut stream = request(endpoint, "DNS", cancel)?;
    write_datagram(&mut stream, query)?;
    read_datagram(&mut stream)
}

pub fn write_datagram(output: &mut impl Write, data: &[u8]) -> io::Result<()> {
    if data.len() > MAX_DATAGRAM {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "datagram too large",
        ));
    }
    output.write_all(&(data.len() as u16).to_be_bytes())?;
    output.write_all(data)
}

pub fn read_datagram(input: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut length = [0; 2];
    input.read_exact(&mut length)?;
    let length = u16::from_be_bytes(length) as usize;
    if length > MAX_DATAGRAM {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "datagram too large",
        ));
    }
    let mut data = vec![0; length];
    input.read_exact(&mut data)?;
    Ok(data)
}
pub struct FrameReader {
    pub bytes: Vec<u8>,
    used: usize,
    target: usize,
}
impl FrameReader {
    pub fn new() -> Self {
        Self {
            bytes: vec![0; MAX_DATAGRAM + 2],
            used: 0,
            target: 2,
        }
    }
    pub fn read<'a>(&'a mut self, stream: &mut impl Read) -> io::Result<Option<&'a [u8]>> {
        loop {
            match stream.read(&mut self.bytes[self.used..self.target]) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "datagram relay closed",
                    ));
                }
                Ok(count) => self.used += count,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) =>
                {
                    return Ok(None);
                }
                Err(error) => return Err(error),
            }
            if self.used != self.target {
                continue;
            }
            if self.target == 2 {
                let length = u16::from_be_bytes([self.bytes[0], self.bytes[1]]) as usize;
                if length > MAX_DATAGRAM {
                    return Err(io::Error::other("invalid datagram length"));
                }
                self.target = length + 2;
                if length != 0 {
                    continue;
                }
            }
            let length = self.target;
            self.used = 0;
            self.target = 2;
            return Ok(Some(&self.bytes[2..length]));
        }
    }
}

/// Bounded UDP framing queue. Retain one drained allocation for steady traffic.
pub struct FrameWriter {
    queue: std::collections::VecDeque<Vec<u8>>,
    spare: Vec<u8>,
    queued: usize,
    offset: usize,
}
impl FrameWriter {
    pub fn new() -> Self {
        Self {
            queue: Default::default(),
            spare: Vec::new(),
            queued: 0,
            offset: 0,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
    pub fn push(&mut self, data: &[u8]) -> bool {
        if data.len() > MAX_DATAGRAM || data.len() + 2 > 262_144usize.saturating_sub(self.queued) {
            return false;
        }
        let mut frame = std::mem::take(&mut self.spare);
        frame.clear();
        frame.reserve(data.len() + 2);
        frame.extend_from_slice(&(data.len() as u16).to_be_bytes());
        frame.extend_from_slice(data);
        self.queued += frame.len();
        self.queue.push_back(frame);
        true
    }
    pub fn write(&mut self, target: &mut impl Write) -> io::Result<bool> {
        self.flush(|bytes| target.write(bytes), true)
    }
    pub fn send(&mut self, target: &std::net::UdpSocket) -> io::Result<bool> {
        self.flush(|bytes| target.send(bytes), false)
    }
    fn flush(
        &mut self,
        mut send: impl FnMut(&[u8]) -> io::Result<usize>,
        framed: bool,
    ) -> io::Result<bool> {
        let mut progress = false;
        for _ in 0..32 {
            let Some(frame) = self.queue.front() else {
                break;
            };
            let bytes = if framed {
                &frame[self.offset..]
            } else {
                &frame[2..]
            };
            match send(bytes) {
                Ok(0) if framed => return Err(io::ErrorKind::WriteZero.into()),
                Ok(count) => {
                    if framed {
                        self.offset += count;
                    } else if count == bytes.len() {
                        self.offset = frame.len();
                    } else {
                        return Err(io::Error::other("partial UDP datagram"));
                    }
                    progress = true;
                    if self.offset == frame.len() {
                        let mut frame = self.queue.pop_front().expect("queued frame");
                        self.queued -= frame.len();
                        self.offset = 0;
                        frame.clear();
                        if frame.capacity() > self.spare.capacity() {
                            self.spare = frame;
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        Ok(progress)
    }
}
