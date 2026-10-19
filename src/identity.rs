//! Declared Linux container identity; real kernel, CPU, memory and disks remain inherited.

use std::ffi::{CString, OsString};
use std::fs;
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::profile::Profile;

pub struct Files {
    pub etc: PathBuf,
    pub dmi: PathBuf,
    pub boot: PathBuf,
    pub machine_alias: Option<PathBuf>,
}

pub fn preflight(profile: &Profile) -> Result<(), String> {
    profile.validate()?;
    let zone = Path::new("/usr/share/zoneinfo").join(&profile.identity.timezone);
    let mut magic = [0; 4];
    fs::File::open(zone)
        .and_then(|mut file| file.read_exact(&mut magic))
        .map_err(|_| "identity.timezone is not an installed timezone")?;
    if &magic != b"TZif" {
        return Err("identity.timezone does not name a TZif timezone file".into());
    }
    let locale = CString::new(profile.identity.locale.as_str()).map_err(|_| "invalid locale")?;
    // SAFETY: newlocale receives a valid C string and allocates an independent
    // locale object. It does not mutate the process-global locale.
    let value =
        unsafe { libc::newlocale(libc::LC_ALL_MASK, locale.as_ptr(), std::ptr::null_mut()) };
    if value.is_null() {
        return Err("identity.locale is not installed; install it or choose an installed locale (locale -a)".into());
    }
    unsafe {
        libc::freelocale(value);
    }
    Ok(())
}

pub fn materialize(profile: &Profile, directory: &Path) -> io::Result<Files> {
    let identity = &profile.identity;
    let etc = directory.join("etc");
    let dmi = directory.join("dmi");
    fs::create_dir(&etc)?;
    fs::create_dir(&dmi)?;
    fs::write(etc.join("hostname"), format!("{}\n", identity.hostname))?;
    fs::write(etc.join("machine-id"), format!("{}\n", identity.machine_id))?;
    fs::write(etc.join("timezone"), format!("{}\n", identity.timezone))?;
    symlink(
        Path::new("/usr/share/zoneinfo").join(&identity.timezone),
        etc.join("localtime"),
    )?;

    let old_hostname = fs::read_to_string("/proc/sys/kernel/hostname")?;
    let hosts = fs::read_to_string("/etc/hosts")?;
    let mut rewritten = String::with_capacity(hosts.len() + 100);
    for line in hosts.lines() {
        let (record, comment) = line.split_once('#').unwrap_or((line, ""));
        for (i, word) in record.split_whitespace().enumerate() {
            if i != 0 {
                rewritten.push('\t');
            }
            rewritten.push_str(if word == old_hostname.trim() {
                &identity.hostname
            } else {
                word
            });
        }
        if !comment.is_empty() {
            rewritten.push_str(" #");
            rewritten.push_str(comment);
        }
        rewritten.push('\n');
    }
    rewritten.push_str(&format!("127.0.1.1\t{}\n", identity.hostname));
    fs::write(etc.join("hosts"), rewritten)?;
    let nss = fs::read_to_string("/etc/nsswitch.conf")?;
    let mut rewritten = String::with_capacity(nss.len() + 20);
    for line in nss.lines() {
        if line.trim_start().starts_with("hosts:") {
            continue;
        }
        rewritten.push_str(line);
        rewritten.push('\n');
    }
    // systemd-resolved/nscd Unix sockets otherwise bypass the DNS packet path.
    rewritten.push_str("hosts: files dns\n");
    fs::write(etc.join("nsswitch.conf"), rewritten)?;
    let mut resolv = String::from("nameserver 198.18.0.1\noptions timeout:2 attempts:2\n");
    if !profile.network.search.is_empty() {
        resolv.push_str("search ");
        resolv.push_str(&profile.network.search.join(" "));
        resolv.push('\n');
    }
    fs::write(etc.join("resolv.conf"), resolv)?;
    for (name, value) in [
        ("sys_vendor", "envwrap"),
        ("product_name", "Linux container"),
        ("product_version", "1"),
        ("product_uuid", &identity.product_uuid),
        ("product_serial", &identity.machine_id),
        ("board_vendor", "envwrap"),
        ("board_name", "Virtual board"),
        ("board_version", "1"),
        ("board_serial", &identity.machine_id),
        ("chassis_vendor", "envwrap"),
        ("chassis_type", "1"),
        ("chassis_serial", &identity.machine_id),
    ] {
        fs::write(dmi.join(name), format!("{value}\n"))?;
    }
    let host_boot = fs::read("/proc/sys/kernel/random/boot_id")?;
    let mut hmac = Hmac::<Sha256>::new_from_slice(identity.seed.as_bytes())
        .expect("HMAC accepts arbitrary key lengths");
    hmac.update(&host_boot);
    let digest = hmac.finalize().into_bytes();
    let mut boot: [u8; 16] = digest[..16].try_into().expect("digest prefix");
    boot[6] = (boot[6] & 15) | 0x40;
    boot[8] = (boot[8] & 63) | 0x80;
    let boot_path = directory.join("boot-id");
    fs::write(&boot_path, format!("{}\n", crate::profile::uuid(&boot)))?;
    let machine_alias = match fs::canonicalize("/var/lib/dbus/machine-id") {
        Ok(path) if path != Path::new("/etc/machine-id") => Some(path),
        Ok(_) => None,
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    Ok(Files {
        etc,
        dmi,
        boot: boot_path,
        machine_alias,
    })
}

impl Files {
    pub fn mounts(&self, command: &mut Command) {
        command
            .arg("--overlay-src")
            .arg("/etc")
            .arg("--overlay-src")
            .arg(&self.etc)
            .args(["--ro-overlay", "/etc"]);
        command
            .arg("--ro-bind")
            .arg(&self.boot)
            .arg("/proc/sys/kernel/random/boot_id");
        if let Some(target) = &self.machine_alias {
            command
                .arg("--ro-bind")
                .arg(self.etc.join("machine-id"))
                .arg(target);
        }
        // glibc's nscd hosts cache uses a Unix socket before consulting NSS.
        // Mask only its dedicated cache directory, never all of /run.
        for directory in ["/run/nscd", "/var/run/nscd"] {
            if Path::new(directory).is_dir() {
                command.args(["--tmpfs", directory]);
            }
        }
    }
}

pub fn environment(profile: &Profile) -> Vec<(OsString, OsString)> {
    let proxy_source = profile.network.proxy.env.as_deref();
    let mut result: Vec<_> = std::env::vars_os()
        .filter(|(key, _)| {
            let name = key.as_bytes();
            ![
                b"http_proxy".as_slice(),
                b"https_proxy",
                b"all_proxy",
                b"no_proxy",
                b"ftp_proxy",
            ]
            .iter()
            .any(|candidate| name.eq_ignore_ascii_case(candidate))
                && proxy_source.is_none_or(|source| name != source.as_bytes())
                && !matches!(name, b"TZ" | b"TZDIR" | b"LANG" | b"LANGUAGE" | b"HOSTNAME")
                && !name.starts_with(b"LC_")
        })
        .collect();
    for (key, value) in [
        ("TZ", &profile.identity.timezone),
        ("LANG", &profile.identity.locale),
        ("LC_ALL", &profile.identity.locale),
        ("HOSTNAME", &profile.identity.hostname),
    ] {
        result.push((key.into(), value.into()));
    }
    result
}

pub fn namespace_sysfs(dmi: &Path) -> io::Result<()> {
    // SAFETY: all mount arguments are valid NUL-terminated strings, and this
    // function runs only after bwrap has created private mount/user/net namespaces.
    let flags = libc::MS_RDONLY | libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC;
    if unsafe {
        libc::mount(
            c"sysfs".as_ptr(),
            c"/sys".as_ptr(),
            c"sysfs".as_ptr(),
            flags,
            std::ptr::null(),
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    if Path::new("/sys/devices/virtual/dmi/id").exists() {
        let source =
            CString::new(dmi.as_os_str().as_bytes()).map_err(|_| io::ErrorKind::InvalidInput)?;
        if unsafe {
            libc::mount(
                source.as_ptr(),
                c"/sys/devices/virtual/dmi/id".as_ptr(),
                std::ptr::null(),
                libc::MS_BIND,
                std::ptr::null(),
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        if unsafe {
            libc::mount(
                std::ptr::null(),
                c"/sys/devices/virtual/dmi/id".as_ptr(),
                std::ptr::null(),
                libc::MS_BIND | libc::MS_REMOUNT | flags,
                std::ptr::null(),
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}
