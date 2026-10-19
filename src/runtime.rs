//! One supervised execution path for generic commands and the Claude shortcut.

use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::{
    broker::Broker, identity, network, profile::Profile, transport::Cancel, wire::Endpoint,
};

const START_TIMEOUT: Duration = Duration::from_secs(30);
static SIGNALS: AtomicU32 = AtomicU32::new(0);

#[derive(Serialize, Deserialize)]
struct Bootstrap {
    socket: PathBuf,
    token: String,
    ip: PathBuf,
    nft: PathBuf,
    dmi: PathBuf,
    mac: [u8; 6],
    environment: Vec<(Vec<u8>, Vec<u8>)>,
    arguments: Vec<Vec<u8>>,
}

pub struct Dependencies {
    bwrap: PathBuf,
    ip: PathBuf,
    nft: PathBuf,
}
impl Dependencies {
    pub fn find() -> Result<Self, String> {
        Ok(Self {
            bwrap: locate("bwrap")?,
            ip: locate("ip")?,
            nft: locate("nft")?,
        })
    }
}
fn locate(name: &str) -> Result<PathBuf, String> {
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::var_os("PATH").unwrap_or_default();
    for directory in
        std::env::split_paths(&path).chain([PathBuf::from("/usr/sbin"), PathBuf::from("/usr/bin")])
    {
        let candidate = directory.join(name);
        if candidate
            .metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        {
            return candidate
                .canonicalize()
                .map_err(|error| format!("cannot resolve {name}: {error}"));
        }
    }
    Err(format!(
        "required helper {name} is missing; install bubblewrap, iproute2 and nftables"
    ))
}

pub fn run(profile: &Profile, arguments: &[OsString]) -> Result<i32, String> {
    if arguments.is_empty() || arguments[0].is_empty() {
        return Err("a program is required".into());
    }
    identity::preflight(profile)?;
    let dependencies = Dependencies::find()?;
    let mut settings = profile.settings()?;
    let check = Cancel::new().map_err(display)?;
    settings.upstream.check(profile.network.dns, Arc::clone(&check)).map_err(|_| {
        "configured proxy preflight failed (SOCKS5h authentication or proxied DNS TCP path); application was not started".to_owned()
    })?;
    settings.dns.prepare(&settings.upstream, check)?;
    let temporary = tempfile::Builder::new()
        .prefix("envwrap-")
        .tempdir_in("/tmp")
        .map_err(display)?;
    std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o700))
        .map_err(display)?;
    let files = identity::materialize(profile, temporary.path()).map_err(display)?;
    let signals = Signals::install().map_err(display)?;
    let (broker, endpoint) =
        Broker::start(temporary.path(), Arc::new(settings)).map_err(display)?;
    let control_path = temporary.path().join("control.sock");
    let listener = UnixListener::bind(&control_path).map_err(display)?;
    listener.set_nonblocking(true).map_err(display)?;
    let executable = std::env::current_exe().map_err(display)?;
    let mut command = Command::new(&dependencies.bwrap);
    command
        .env_clear()
        .env("PATH", "/usr/bin:/usr/sbin:/bin")
        .env("LC_ALL", "C")
        .args([
            "--bind",
            "/",
            "/",
            "--dev-bind",
            "/dev",
            "/dev",
            "--proc",
            "/proc",
            "--unshare-user",
            "--unshare-net",
            "--unshare-pid",
            "--unshare-uts",
            "--die-with-parent",
            "--cap-add",
            "CAP_NET_ADMIN",
            "--cap-add",
            "CAP_NET_RAW",
            "--cap-add",
            "CAP_SYS_ADMIN",
            "--hostname",
        ])
        .arg(&profile.identity.hostname)
        .arg("--chdir")
        .arg(std::env::current_dir().map_err(display)?);
    files.mounts(&mut command);
    command
        .arg("--")
        .arg(executable)
        .arg("--internal-runtime")
        .arg(&control_path);
    // SAFETY: only async-signal-safe syscalls and stack storage after fork.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) < 0 {
                return Err(io::Error::last_os_error());
            }
            // The bwrap reaper shares the foreground terminal group. It must
            // not die on Ctrl-C before the application can handle it and report
            // its own exit status. The namespace helper installs caught signal
            // handlers again; exec then restores defaults for the application.
            let mut ignored: libc::sigaction = std::mem::zeroed();
            ignored.sa_sigaction = libc::SIG_IGN;
            libc::sigemptyset(&mut ignored.sa_mask);
            for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT] {
                if libc::sigaction(signal, &ignored, std::ptr::null_mut()) < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let mut child = Process(
        command
            .spawn()
            .map_err(|error| format!("cannot start rootless runtime: {error}"))?,
    );
    let deadline = Instant::now() + START_TIMEOUT;
    let mut parent = loop {
        match listener.accept() {
            Ok((socket, _)) => break socket,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(display(error)),
        }
        if let Some(status) = child.0.try_wait().map_err(display)? {
            return Err(format!(
                "rootless namespace setup failed ({}); check bubblewrap/user namespace support",
                exit_code(status)
            ));
        }
        if Instant::now() >= deadline {
            return Err("rootless runtime connection timed out".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    drop(listener);
    std::fs::remove_file(&control_path).map_err(display)?;
    parent
        .set_write_timeout(Some(START_TIMEOUT))
        .map_err(display)?;
    let bootstrap = Bootstrap {
        socket: endpoint.socket,
        token: endpoint.token,
        ip: dependencies.ip,
        nft: dependencies.nft,
        dmi: files.dmi.clone(),
        mac: profile.identity.mac,
        environment: identity::environment(profile)
            .into_iter()
            .map(|(k, v)| (k.into_vec(), v.into_vec()))
            .collect(),
        arguments: arguments
            .iter()
            .map(|value| value.as_bytes().to_vec())
            .collect(),
    };
    let payload = serde_json::to_vec(&bootstrap).map_err(display)?;
    parent
        .write_all(&(payload.len() as u32).to_be_bytes())
        .and_then(|()| parent.write_all(&payload))
        .map_err(|_| "runtime bootstrap channel closed".to_owned())?;
    parent.set_nonblocking(true).map_err(display)?;
    let deadline = Instant::now() + START_TIMEOUT;
    let mut ready = false;
    let mut callbacks: Option<crate::callback::Running> = None;
    let status = loop {
        if let Some(status) = child.0.try_wait().map_err(display)? {
            break status;
        }
        if broker.finished() {
            return Err("host network broker exited; stopping protected application".into());
        }
        let mut byte = [0];
        match parent.read(&mut byte) {
            Ok(1) if byte[0] == b'R' && !ready => {
                ready = true;
                callbacks = Some(
                    crate::callback::host(Endpoint {
                        socket: temporary.path().join("callbacks.sock"),
                        token: bootstrap.token.clone(),
                    })
                    .map_err(display)?,
                );
            }
            Ok(1) if byte[0] == b'E' => {
                drop(callbacks.take());
            }
            Ok(0) => {} // waitpid supplies the precise application/runtime status.
            Ok(_) => return Err("invalid runtime control response".into()),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(display(error)),
        }
        if callbacks
            .as_ref()
            .is_some_and(crate::callback::Running::finished)
        {
            callbacks
                .take()
                .expect("checked callback worker")
                .shutdown()
                .map_err(display)?;
            return Err("host callback worker exited; stopping application".into());
        }
        for signal in signals.pending() {
            match parent.write(&[signal as u8]) {
                Ok(1) => {}
                _ => {
                    return Err("cannot forward termination signal to protected application".into());
                }
            }
        }
        if !ready && Instant::now() >= deadline {
            return Err("rootless runtime setup timed out; application was not started".into());
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    drop(callbacks);
    broker.shutdown().map_err(display)?;
    Ok(exit_code(status))
}

pub fn helper(path: &Path) -> Result<i32, String> {
    // Rust UnixStream descriptors are close-on-exec. Bootstrap configuration
    // travels through this private directory socket, never argv or helper env.
    let mut control = UnixStream::connect(path).map_err(display)?;
    control
        .set_read_timeout(Some(START_TIMEOUT))
        .map_err(display)?;
    let mut length = [0; 4];
    control.read_exact(&mut length).map_err(display)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > 16 * 1024 * 1024 {
        return Err("runtime bootstrap exceeds 16 MiB".into());
    }
    let mut payload = vec![0; length];
    control.read_exact(&mut payload).map_err(display)?;
    let config: Bootstrap =
        serde_json::from_slice(&payload).map_err(|_| "invalid runtime bootstrap".to_owned())?;
    drop(payload);
    if config.arguments.is_empty() {
        return Err("missing runtime program".into());
    }
    let signals = Signals::install().map_err(display)?;
    let endpoint = Arc::new(Endpoint {
        socket: config.socket,
        token: config.token,
    });
    let network =
        network::Running::start(Arc::clone(&endpoint), &config.ip, &config.nft, config.mac)
            .map_err(display)?;
    let callbacks = crate::callback::namespace(
        &endpoint.socket.with_file_name("callbacks.sock"),
        endpoint.token.clone(),
        network.listener_addresses(),
    )
    .map_err(display)?;
    identity::namespace_sysfs(&config.dmi).map_err(display)?;
    let mut command = Command::new(OsStr::from_bytes(&config.arguments[0]));
    command
        .args(
            config.arguments[1..]
                .iter()
                .map(|arg| OsStr::from_bytes(arg)),
        )
        .env_clear()
        .envs(
            config
                .environment
                .iter()
                .map(|(key, value)| (OsStr::from_bytes(key), OsStr::from_bytes(value))),
        );
    // SAFETY: prctl and capset use stack-only data and are safe after fork. No
    // namespace capability, control channel or network socket reaches the app.
    unsafe {
        command.pre_exec(drop_capabilities);
    }
    let mut child = Process(
        command
            .spawn()
            .map_err(|error| format!("cannot launch requested program: {error}"))?,
    );
    control.write_all(b"R").map_err(display)?;
    control.set_nonblocking(true).map_err(display)?;
    let status = loop {
        if let Some(status) = child.0.try_wait().map_err(display)? {
            break status;
        }
        if network.finished() {
            return Err("transparent network worker exited; stopping application".into());
        }
        if callbacks.finished() {
            callbacks.shutdown().map_err(display)?;
            return Err("namespace callback worker exited; stopping application".into());
        }
        let mut signal = [0];
        match control.read(&mut signal) {
            Ok(0) => return Err("launcher exited; stopping application".into()),
            Ok(1)
                if [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT]
                    .contains(&(signal[0] as i32)) =>
            {
                // SAFETY: child.id is the live child held by Process; no reaping
                // or pid reuse occurs between try_wait and kill on this thread.
                unsafe {
                    libc::kill(child.0.id() as i32, signal[0] as i32);
                }
            }
            Ok(_) => return Err("invalid launcher control message".into()),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(display(error)),
        }
        for signal in signals.pending() {
            // SAFETY: same live, unreaped child as the control-channel path.
            unsafe {
                libc::kill(child.0.id() as i32, signal);
            }
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    control.write_all(b"E").map_err(display)?;
    callbacks.shutdown().map_err(display)?;
    network.shutdown().map_err(display)?;
    Ok(exit_code(status))
}

fn drop_capabilities() -> io::Result<()> {
    #[repr(C)]
    struct Header {
        version: u32,
        pid: i32,
    }
    #[repr(C)]
    struct Data {
        effective: u32,
        permitted: u32,
        inheritable: u32,
    }
    let header = Header {
        version: 0x20080522,
        pid: 0,
    };
    let data = [
        Data {
            effective: 0,
            permitted: 0,
            inheritable: 0,
        },
        Data {
            effective: 0,
            permitted: 0,
            inheritable: 0,
        },
    ];
    // SAFETY: Linux capset ABI v3 takes one header and two initialized records.
    unsafe {
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) < 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::syscall(libc::SYS_capset, &header as *const Header, data.as_ptr()) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}
fn exit_code(status: ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(libc::SIGKILL))
}
fn display(error: impl std::fmt::Display) -> String {
    error.to_string()
}

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

extern "C" fn signal_handler(signal: i32, info: *mut libc::siginfo_t, _context: *mut libc::c_void) {
    // Terminal-generated signals already reach every process in the foreground
    // process group. Forward only targeted signals, avoiding a second Ctrl-C.
    if !info.is_null() && unsafe { (*info).si_code } != libc::SI_KERNEL {
        SIGNALS.fetch_or(1u32 << signal, Ordering::Relaxed);
    }
}
struct Signals(Vec<(i32, libc::sigaction)>);
impl Signals {
    fn install() -> io::Result<Self> {
        SIGNALS.store(0, Ordering::Relaxed);
        let mut guard = Self(Vec::new());
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT] {
            // SAFETY: zeroed sigaction storage is initialized before installation.
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = signal_handler as *const () as usize;
            action.sa_flags = libc::SA_SIGINFO;
            unsafe {
                libc::sigemptyset(&mut action.sa_mask);
            }
            if unsafe { libc::sigaction(signal, &action, &mut old) } < 0 {
                return Err(io::Error::last_os_error());
            }
            guard.0.push((signal, old));
        }
        Ok(guard)
    }
    fn pending(&self) -> impl Iterator<Item = i32> {
        let signals = SIGNALS.swap(0, Ordering::Relaxed);
        [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT]
            .into_iter()
            .filter(move |signal| signals & (1u32 << signal) != 0)
    }
}
impl Drop for Signals {
    fn drop(&mut self) {
        for (signal, old) in &self.0 {
            unsafe {
                libc::sigaction(*signal, old, std::ptr::null_mut());
            }
        }
    }
}
