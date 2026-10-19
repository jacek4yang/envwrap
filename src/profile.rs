//! Explicit, versioned profiles. Loading never creates or repairs a profile.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use ipnet::IpNet;
use serde::{Deserialize, Serialize};

use crate::{broker, dns, kernel, upstream::Upstream};

pub const SCHEMA_VERSION: u32 = 2;
const MAX_PROFILE: u64 = 1_048_576;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    #[serde(deserialize_with = "version")]
    pub version: u32,
    pub name: String,
    pub identity: Identity,
    pub network: Network,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub hostname: String,
    pub machine_id: String,
    pub product_uuid: String,
    #[serde(with = "mac")]
    pub mac: [u8; 6],
    pub timezone: String,
    pub locale: String,
    /// Secret salt for a profile-scoped boot identity. Never sent to the child.
    pub seed: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Network {
    pub proxy: ProxySource,
    pub dns: std::net::SocketAddr,
    #[serde(default)]
    pub dns_preference: dns::Preference,
    #[serde(default)]
    pub direct: Vec<IpNet>,
    #[serde(default)]
    pub internal_dns: Vec<InternalDns>,
    #[serde(default)]
    pub search: Vec<String>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProxySource {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InternalDns {
    pub suffix: String,
    pub resolver: std::net::SocketAddr,
}

fn version<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
    let value = u32::deserialize(deserializer)?;
    if value != SCHEMA_VERSION {
        return Err(serde::de::Error::custom(
            "unsupported profile schema; explicitly create a version 2 profile",
        ));
    }
    Ok(value)
}
pub fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && (value.as_bytes()[0].is_ascii_lowercase() || value.as_bytes()[0].is_ascii_digit())
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
}
pub fn valid_variable(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && (value.as_bytes()[0].is_ascii_alphabetic() || value.as_bytes()[0] == b'_')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
fn hex_value(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
        && value.bytes().any(|b| b != b'0')
}
impl Profile {
    pub fn generate(
        name: String,
        timezone: String,
        locale: String,
        network: Network,
        hostname: Option<String>,
    ) -> Result<Self, String> {
        if !valid_name(&name) {
            return Err("profile name must match [a-z0-9][a-z0-9_-]{0,63}".into());
        }
        let mut random = [0u8; 70];
        File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(&mut random))
            .map_err(|_| "cannot obtain identity entropy")?;
        let machine_id = hex(&random[..16]);
        random[22] = (random[22] & 0x0f) | 0x40;
        random[24] = (random[24] & 0x3f) | 0x80;
        let product_uuid = uuid(random[16..32].try_into().expect("UUID entropy"));
        let mut mac: [u8; 6] = random[32..38].try_into().expect("MAC entropy");
        mac[0] = (mac[0] & 0xfc) | 2;
        let profile = Self {
            version: SCHEMA_VERSION,
            name,
            identity: Identity {
                hostname: hostname.unwrap_or_else(|| format!("envwrap-{}", &machine_id[..8])),
                machine_id,
                product_uuid,
                mac,
                timezone,
                locale,
                seed: hex(&random[38..]),
            },
            network,
        };
        profile.validate()?;
        Ok(profile)
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.version != SCHEMA_VERSION {
            return Err("unsupported profile schema; explicitly create a version 2 profile".into());
        }
        if !valid_name(&self.name) {
            return Err("invalid profile name".into());
        }
        let hostname = &self.identity.hostname;
        if hostname.is_empty()
            || hostname.len() > 63
            || !hostname.as_bytes()[0].is_ascii_alphanumeric()
            || !hostname.as_bytes()[hostname.len() - 1].is_ascii_alphanumeric()
            || !hostname
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(
                "identity.hostname must be a lowercase Linux hostname of 1–63 characters".into(),
            );
        }
        if !hex_value(&self.identity.machine_id, 32) {
            return Err("identity.machine_id must contain 32 nonzero lowercase hex digits".into());
        }
        let uuid = &self.identity.product_uuid;
        if uuid.len() != 36
            || uuid.bytes().enumerate().any(|(i, b)| {
                if matches!(i, 8 | 13 | 18 | 23) {
                    b != b'-'
                } else {
                    !(b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
                }
            })
            || uuid.bytes().all(|b| matches!(b, b'0' | b'-'))
        {
            return Err("identity.product_uuid must be a nonzero canonical lowercase UUID".into());
        }
        if self.identity.mac[0] & 3 != 2 {
            return Err("identity.mac must be a locally administered unicast MAC".into());
        }
        if !hex_value(&self.identity.seed, 64) {
            return Err("identity.seed must contain 64 nonzero lowercase hex digits".into());
        }
        let timezone = &self.identity.timezone;
        if timezone.is_empty()
            || timezone.len() > 128
            || !timezone.is_ascii()
            || Path::new(timezone)
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
            || !timezone
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-' | b'+'))
        {
            return Err(
                "identity.timezone must name an IANA zone below /usr/share/zoneinfo".into(),
            );
        }
        let locale = &self.identity.locale;
        if locale.is_empty()
            || locale.len() > 64
            || !locale
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'@' | b'-'))
        {
            return Err("identity.locale is not a valid locale name".into());
        }
        match (&self.network.proxy.env, &self.network.proxy.url) {
            (Some(name), None) if valid_variable(name) => {}
            (None, Some(url)) => {
                Upstream::parse(url)?;
            }
            _ => {
                return Err(
                    "network.proxy requires exactly one valid env variable name or URL".into(),
                );
            }
        }
        dns::validate_address(self.network.dns)?;
        for range in &self.network.direct {
            if range.prefix_len() == 0 {
                return Err("network.direct cannot contain a default route; declare trusted host/LAN prefixes".into());
            }
            if crate::domain::overlaps(range) {
                return Err("network.direct overlaps reserved domain routing ranges 198.19.0.0/16 or fd23:656e:7677:1::/64".into());
            }
            if range.contains(&std::net::IpAddr::V4(kernel::GUEST_V4))
                || range.contains(&std::net::IpAddr::V6(kernel::GUEST_V6))
            {
                return Err(
                    "network.direct overlaps envwrap's reserved virtual interface addresses".into(),
                );
            }
            if range.network().is_multicast()
                || range.contains(
                    &"224.0.0.1"
                        .parse::<std::net::IpAddr>()
                        .expect("multicast constant"),
                )
                || range.contains(
                    &"ff02::1"
                        .parse::<std::net::IpAddr>()
                        .expect("multicast constant"),
                )
            {
                return Err(
                    "network.direct supports unicast host/LAN routes, not multicast bridging"
                        .into(),
                );
            }
            if let IpNet::V6(range) = range
                && (range.network().is_unicast_link_local()
                    || range.contains(
                        &"fe80::1"
                            .parse::<std::net::Ipv6Addr>()
                            .expect("link-local constant"),
                    ))
            {
                return Err("network.direct requires routable IPv6 prefixes; link-local interface scopes are not bridged".into());
            }
        }
        for rule in &self.network.internal_dns {
            dns::suffix(&rule.suffix)?;
            dns::validate_address(rule.resolver)?;
            if !rule.resolver.ip().is_loopback()
                && !self
                    .network
                    .direct
                    .iter()
                    .any(|range| range.contains(&rule.resolver.ip()))
            {
                return Err(
                    "internal DNS resolver must be loopback or in an explicit network.direct range"
                        .into(),
                );
            }
        }
        if self.network.search.len() > 6 {
            return Err("network.search supports at most six search domains".into());
        }
        let mut search_length = 0;
        for domain in &self.network.search {
            dns::suffix(domain)?;
            search_length += domain.len() + 1;
        }
        if search_length > 256 {
            return Err("network.search exceeds the Linux resolver search-list limit".into());
        }
        Ok(())
    }
    pub fn settings(&self) -> Result<broker::Settings, String> {
        self.validate()?;
        let upstream = if let Some(name) = &self.network.proxy.env {
            let value = std::env::var(name)
                .map_err(|_| format!("proxy source variable {name} is missing or is not UTF-8"))?;
            if value.trim().is_empty() {
                return Err(format!("proxy source variable {name} is empty"));
            }
            Upstream::parse(&value)?
        } else {
            Upstream::parse(
                self.network
                    .proxy
                    .url
                    .as_deref()
                    .expect("validated proxy source"),
            )?
        };
        let dns = dns::Resolver::new(
            self.network.dns,
            self.network
                .internal_dns
                .iter()
                .map(|rule| dns::LocalResolver {
                    suffix: rule.suffix.clone(),
                    address: rule.resolver,
                })
                .collect(),
            self.network.dns_preference,
        )?;
        let settings = broker::Settings {
            upstream,
            dns,
            direct: self.network.direct.clone(),
        };
        settings.validate()?;
        Ok(settings)
    }
}

pub struct Store {
    directory: PathBuf,
}
impl Store {
    pub fn at(directory: PathBuf) -> Self {
        Self { directory }
    }
    pub fn from_env() -> Result<Self, String> {
        let base =
            if let Some(value) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
                PathBuf::from(value)
            } else {
                PathBuf::from(
                    std::env::var_os("HOME")
                        .ok_or("set HOME or XDG_CONFIG_HOME for profile storage")?,
                )
                .join(".config")
            };
        if !base.is_absolute() {
            return Err("profile configuration directory must be absolute".into());
        }
        Ok(Self::at(base.join("envwrap/profiles")))
    }
    pub fn path(&self, name: &str) -> Result<PathBuf, String> {
        if !valid_name(name) {
            return Err("profile name must match [a-z0-9][a-z0-9_-]{0,63}".into());
        }
        Ok(self.directory.join(format!("{name}.toml")))
    }
    pub fn load(&self, name: &str) -> Result<Profile, String> {
        let path = self.path(name)?;
        let file = File::open(&path).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                format!(
                    "profile '{name}' is not configured; run envwrap profile create {name} --help"
                )
            } else {
                format!("cannot open profile: {error}")
            }
        })?;
        let metadata = file
            .metadata()
            .map_err(|error| format!("cannot inspect profile: {error}"))?;
        // SAFETY: getuid has no pointer arguments or process-global mutation.
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::getuid() }
            || metadata.mode() & 0o077 != 0
        {
            return Err(
                "profile must be a private regular file owned by this user (chmod 600)".into(),
            );
        }
        let mut text = String::with_capacity(metadata.len().min(MAX_PROFILE) as usize);
        file.take(MAX_PROFILE + 1)
            .read_to_string(&mut text)
            .map_err(|_| "cannot read profile as UTF-8")?;
        if text.len() as u64 > MAX_PROFILE {
            return Err("profile exceeds 1 MiB".into());
        }
        let profile:Profile=toml::from_str(&text).map_err(|error:toml::de::Error| {
            let line=error.span().map_or(1,|span|text.as_bytes()[..span.start.min(text.len())].iter().filter(|&&b|b==b'\n').count()+1);
            let message=error.message();
            let detail = if message == "unsupported profile schema; explicitly create a version 2 profile"
                || message.starts_with("missing field `")
            {
                message
            } else {
                "check field names, TOML syntax and types (values omitted)"
            };
            format!("invalid profile at line {line}: {detail}; create a complete version 2 profile if this is an older configuration")
        })?;
        profile.validate()?;
        if profile.name != name {
            return Err("profile name does not match its filename".into());
        }
        Ok(profile)
    }
    pub fn create(&self, profile: &Profile) -> Result<PathBuf, String> {
        profile.validate()?;
        let target = self.path(&profile.name)?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.directory)
            .map_err(|error| format!("cannot create profile directory: {error}"))?;
        fs::set_permissions(&self.directory, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("cannot protect profile directory: {error}"))?;
        let text = toml::to_string_pretty(profile).map_err(|_| "cannot encode profile")?;
        let mut temporary = tempfile::Builder::new()
            .prefix(".create-")
            .tempfile_in(&self.directory)
            .map_err(|error| format!("cannot create private profile file: {error}"))?;
        temporary
            .write_all(text.as_bytes())
            .and_then(|()| temporary.as_file().sync_all())
            .map_err(|error| format!("cannot write profile: {error}"))?;
        temporary.persist_noclobber(&target).map_err(|error| {
            if error.error.kind() == io::ErrorKind::AlreadyExists {
                "profile already exists; it was not replaced".into()
            } else {
                format!("cannot publish profile: {}", error.error)
            }
        })?;
        File::open(&self.directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("cannot sync profile directory: {error}"))?;
        Ok(target)
    }
    pub fn list(&self) -> Result<Vec<String>, String> {
        let entries = match fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(format!("cannot list profiles: {error}")),
        };
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| format!("cannot read profile directory: {error}"))?;
            let filename = entry.file_name();
            if let Some(name) = filename
                .to_str()
                .and_then(|name| name.strip_suffix(".toml"))
                .filter(|name| valid_name(name))
            {
                names.push(name.to_owned());
            }
        }
        names.sort_unstable();
        Ok(names)
    }
    pub fn delete(&self, name: &str) -> Result<(), String> {
        fs::remove_file(self.path(name)?)
            .map_err(|error| format!("cannot delete profile: {error}"))?;
        File::open(&self.directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("cannot sync profile deletion: {error}"))
    }
}
pub fn nonce() -> io::Result<String> {
    let mut bytes = [0u8; 32];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(hex(&bytes))
}
fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        text.push(HEX[(byte >> 4) as usize] as char);
        text.push(HEX[(byte & 15) as usize] as char);
    }
    text
}
pub fn uuid(bytes: &[u8; 16]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(36);
    for (index, &byte) in bytes.iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            text.push('-');
        }
        text.push(HEX[(byte >> 4) as usize] as char);
        text.push(HEX[(byte & 15) as usize] as char);
    }
    text
}
mod mac {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(bytes: &[u8; 6], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&Display(bytes))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[u8; 6], D::Error> {
        let text = String::deserialize(deserializer)?;
        let mut result = [0; 6];
        let mut parts = text.split(':');
        for byte in &mut result {
            let part = parts
                .next()
                .filter(|part| part.len() == 2)
                .ok_or_else(|| serde::de::Error::custom("MAC requires six hex octets"))?;
            *byte = u8::from_str_radix(part, 16)
                .map_err(|_| serde::de::Error::custom("invalid MAC hex octet"))?;
        }
        if parts.next().is_some() {
            return Err(serde::de::Error::custom("MAC requires six hex octets"));
        }
        Ok(result)
    }
    struct Display<'a>(&'a [u8; 6]);
    impl std::fmt::Display for Display<'_> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(
                f,
                "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                self.0[0], self.0[1], self.0[2], self.0[3], self.0[4], self.0[5]
            )
        }
    }
}
