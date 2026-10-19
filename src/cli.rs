//! Small command surface; application arguments are never reinterpreted.

use std::ffi::OsString;
use std::path::Path;

use crate::{
    identity,
    profile::{InternalDns, Network, Profile, ProxySource, Store},
    runtime,
};

const HELP: &str = "envwrap — Linux transparent proxy and declared container identity

USAGE:
  envwrap [--profile NAME] claude [CLAUDE_ARGS...]
  envwrap [--profile NAME] run -- PROGRAM [ARGS...]
  envwrap [--profile NAME] doctor
  envwrap profile create NAME --proxy-env VAR --dns IP:PORT --timezone ZONE --locale LOCALE
  envwrap profile show NAME
  envwrap profile list
  envwrap profile delete NAME

Default profile selection: default. Missing profiles are never created on launch.
Put --profile before the command. All arguments after claude belong to Claude,
including --help, --version and --resume. HOME, login and application settings
remain your own. No TLS interception or control of arbitrary telemetry payloads.

Requirements: Linux, rootless user namespaces, bubblewrap with read-only overlays,
iproute2 and nftables. External TCP and DNS use the configured proxy; external
UDP is blocked. Host loopback and explicit trusted LAN ranges are direct.
";
const CREATE_HELP: &str = "envwrap profile create NAME [OPTIONS]

Required:
  --proxy-env VAR       explicit environment variable holding an HTTP CONNECT or SOCKS5 URL
  --dns IP:PORT         DNS server reached by TCP through that proxy
  --timezone ZONE       installed IANA zone, e.g. Europe/Berlin
  --locale LOCALE       installed locale; inspect locale -a
Optional:
  --hostname NAME       stable Linux hostname (otherwise generated at creation)
  --direct CIDR         trusted direct LAN range; repeatable, no implicit private ranges
  --internal-dns SUFFIX=IP:PORT   explicit direct resolver for a domain suffix; repeatable
  --search DOMAIN       DNS search domain; repeatable

Proxy endpoints currently require a literal IP, including [IPv6]:port.
Profiles are private TOML files. To store a URL instead, edit network.proxy to
url = \"...\" and remove env. Never put a proxy password in command arguments.
Creation never overwrites an existing profile. Runtime never generates identity.
";

pub fn dispatch(arguments: impl Iterator<Item = OsString>) -> i32 {
    match execute(arguments.collect()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("envwrap: {error}");
            2
        }
    }
}
fn text(value: &OsString) -> Result<&str, String> {
    value
        .to_str()
        .ok_or_else(|| "envwrap options must be UTF-8; application arguments need not be".into())
}
fn execute(mut arguments: Vec<OsString>) -> Result<i32, String> {
    if arguments
        .first()
        .is_some_and(|arg| arg == "--internal-runtime")
        && arguments.len() == 2
    {
        return runtime::helper(Path::new(&arguments[1]));
    }
    if arguments
        .first()
        .is_some_and(|arg| arg == "--internal-probe")
        && arguments.len() == 1
    {
        return probe();
    }
    let mut selected = "default".to_owned();
    if arguments.first().is_some_and(|arg| arg == "--profile") {
        if arguments.len() < 3 {
            return Err("--profile requires a name followed by a command".into());
        }
        selected = text(&arguments[1])?.to_owned();
        arguments.drain(..2);
    }
    let Some(command) = arguments.first() else {
        print!("{HELP}");
        return Ok(0);
    };
    match text(command)? {
        "--help"|"-h" if arguments.len()==1 => { print!("{HELP}"); Ok(0) },
        "--version"|"-V" if arguments.len()==1 => { println!("envwrap {}",crate::tool_version()); Ok(0) },
        "profile" => profile(&arguments[1..]),
        "claude" => {
            arguments[0]="claude".into();
            runtime::run(&Store::from_env()?.load(&selected)?,&arguments)
        },
        "run" if arguments.get(1).is_some_and(|arg|arg=="--") && arguments.len()>2 => runtime::run(&Store::from_env()?.load(&selected)?,&arguments[2..]),
        "doctor" if arguments.len()==1 => {
            let profile=Store::from_env()?.load(&selected)?;
            identity::preflight(&profile)?;
            runtime::Dependencies::find()?;
            println!("profile: {selected} (schema {})",profile.version);
            println!("direct exceptions: host loopback{}",if profile.network.direct.is_empty(){String::new()}else{format!(", {}",profile.network.direct.iter().map(ToString::to_string).collect::<Vec<_>>().join(", "))});
            for rule in &profile.network.internal_dns {println!("direct DNS: {} -> {}",rule.suffix,rule.resolver);}
            println!("external UDP: blocked; inherited HOME/config/Unix sockets: preserved");
            let status=runtime::run(&profile,&[std::env::current_exe().map_err(|e|e.to_string())?.into(),"--internal-probe".into()])?;
            if status!=0 {return Err(format!("runtime diagnostics failed ({status})"));}
            println!("Claude authenticated conversation/MCP/hooks: not exercised by doctor");
            Ok(0)
        },
        "run" => Err("usage: envwrap [--profile NAME] run -- PROGRAM [ARGS...]".into()),
        _ => Err("unknown command or unsupported option; run envwrap --help (old environment/sandbox commands were removed)".into()),
    }
}
fn profile(arguments: &[OsString]) -> Result<i32, String> {
    let store = Store::from_env()?;
    let Some(operation) = arguments.first() else {
        return Err("profile requires create, show, list or delete".into());
    };
    match text(operation)? {
        "list" if arguments.len() == 1 => {
            for name in store.list()? {
                println!("{name}");
            }
        }
        "show" if arguments.len() == 2 => {
            let mut value = store.load(text(&arguments[1])?)?;
            value.identity.seed = "<redacted>".into();
            if value.network.proxy.url.is_some() {
                value.network.proxy.url = Some("<redacted>".into());
            }
            print!(
                "{}",
                toml::to_string_pretty(&value).map_err(|_| "cannot render profile")?
            );
        }
        "delete" if arguments.len() == 2 => store.delete(text(&arguments[1])?)?,
        "create" => {
            if arguments[1..].iter().any(|arg| arg == "--help") {
                print!("{CREATE_HELP}");
                return Ok(0);
            }
            let name = arguments.get(1).ok_or("profile create requires a name")?;
            let mut timezone = None;
            let mut locale = None;
            let mut proxy = None;
            let mut dns = None;
            let mut hostname = None;
            let mut direct = Vec::new();
            let mut internal_dns = Vec::new();
            let mut search = Vec::new();
            let mut options = arguments[2..].iter();
            while let Some(key) = options.next() {
                let key = text(key)?;
                // Reject unknown tokens before formatting diagnostics: a user
                // may accidentally supply a credential-bearing URL as an option.
                let target = match key {
                    "--timezone" => Some(&mut timezone),
                    "--locale" => Some(&mut locale),
                    "--proxy-env" => Some(&mut proxy),
                    "--dns" => Some(&mut dns),
                    "--hostname" => Some(&mut hostname),
                    "--direct" | "--search" | "--internal-dns" => None,
                    _ => return Err("unknown profile create option; use --help".into()),
                };
                let value = text(
                    options
                        .next()
                        .ok_or_else(|| format!("{key} requires a value"))?,
                )?;
                if let Some(target) = target {
                    if target.replace(value.to_owned()).is_some() {
                        return Err(format!("duplicate {key}"));
                    }
                    continue;
                }
                match key {
                    "--direct" => {
                        direct.push(value.parse().map_err(|_| "--direct requires a CIDR")?)
                    }
                    "--search" => search.push(value.to_owned()),
                    "--internal-dns" => {
                        let (suffix, address) = value
                            .split_once('=')
                            .ok_or("--internal-dns requires SUFFIX=IP:PORT")?;
                        internal_dns.push(InternalDns {
                            suffix: suffix.into(),
                            resolver: address
                                .parse()
                                .map_err(|_| "invalid internal DNS address")?,
                        });
                    }
                    _ => unreachable!("validated repeatable option"),
                }
            }
            let network = Network {
                proxy: ProxySource {
                    env: Some(proxy.ok_or("missing --proxy-env")?),
                    url: None,
                },
                dns: dns
                    .ok_or("missing --dns")?
                    .parse()
                    .map_err(|_| "--dns requires IP:PORT")?,
                direct,
                internal_dns,
                search,
            };
            let profile = Profile::generate(
                text(name)?.to_owned(),
                timezone.ok_or("missing --timezone")?,
                locale.ok_or("missing --locale")?,
                network,
                hostname,
            )?;
            identity::preflight(&profile)?;
            let path = store.create(&profile)?;
            println!("created {}", path.display());
        }
        _ => {
            return Err(concat!(
                "usage: envwrap profile create|show|list|delete; ",
                "create NAME --help lists required configuration"
            )
            .into());
        }
    }
    Ok(0)
}

fn probe() -> Result<i32, String> {
    let read = |path: &str| std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"));
    let hostname = read("/etc/hostname")?;
    if read("/proc/sys/kernel/hostname")?.trim() != hostname.trim()
        || std::env::var("HOSTNAME").unwrap_or_default() != hostname.trim()
    {
        return Err("hostname views disagree".into());
    }
    let machine = read("/etc/machine-id")?;
    if Path::new("/var/lib/dbus/machine-id").exists()
        && read("/var/lib/dbus/machine-id")? != machine
    {
        return Err("machine-id aliases disagree".into());
    }
    let timezone = std::env::var("TZ").map_err(|_| "TZ is absent")?;
    if read("/etc/timezone")?.trim() != timezone {
        return Err("timezone names disagree".into());
    }
    let zone = Path::new("/usr/share/zoneinfo").join(&timezone);
    if std::fs::read_link("/etc/localtime").map_err(|e| e.to_string())? != zone
        || std::fs::read("/etc/localtime").map_err(|e| e.to_string())?
            != std::fs::read(zone).map_err(|e| e.to_string())?
    {
        return Err("timezone content/path disagree".into());
    }
    for interface in ["lo", "eth0"] {
        if !Path::new("/sys/class/net").join(interface).exists() {
            return Err("namespace sysfs interface missing".into());
        }
    }
    let status = read("/proc/self/status")?;
    for field in ["CapInh:", "CapPrm:", "CapEff:", "CapAmb:"] {
        if !status.lines().any(|line| {
            line.starts_with(field) && line.split_whitespace().nth(1) == Some("0000000000000000")
        }) {
            return Err("application retained helper capabilities".into());
        }
    }
    println!(
        "PASS rootless startup, hostname/machine-id/timezone views, namespace sysfs, application capabilities"
    );
    let query = [0x45, 0x57, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 1];
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
    socket
        .set_read_timeout(Some(std::time::Duration::from_secs(12)))
        .map_err(|e| e.to_string())?;
    socket.connect("198.18.0.1:53").map_err(|e| e.to_string())?;
    socket.send(&query).map_err(|e| e.to_string())?;
    let mut response = [0; 512];
    let count = socket
        .recv(&mut response)
        .map_err(|_| "configured DNS query failed")?;
    if count < 12 || response[..2] != query[..2] || response[2] & 0x80 == 0 {
        return Err("invalid DNS response".into());
    }
    if response[3] & 0x0f != 0 {
        return Err("configured DNS resolver returned an error".into());
    }
    println!("PASS application UDP DNS using configured DNS policy");
    Ok(0)
}
