//! Proxy authority parsing shared by profile validation and the host transport.

pub struct ProxyParts {
    pub scheme: String,
    pub host: String,
    pub port: Option<u16>,
}

/// Credentials are deliberately not retained here. Split userinfo before the
/// path: a password can contain a slash, and authority selection must agree with
/// the credential parser in upstream.rs.
pub fn parse_proxy_url(raw: &str) -> Result<ProxyParts, String> {
    let raw = raw.trim();
    let (scheme, rest) = match raw.find("://") {
        Some(index) => {
            let scheme = raw[..index].to_ascii_lowercase();
            if !["http", "socks5", "socks5h"].contains(&scheme.as_str()) {
                return Err("unsupported proxy scheme".into());
            }
            (scheme, &raw[index + 3..])
        }
        None => ("http".to_owned(), raw),
    };
    let hostpath = rest.rsplit_once('@').map_or(rest, |(_, host)| host);
    let end = hostpath.find(['/', '?', '#']).unwrap_or(hostpath.len());
    let (host, port) = split_hostport(&hostpath[..end])?;
    if host.is_empty() {
        return Err("missing proxy host".into());
    }
    Ok(ProxyParts { scheme, host, port })
}

fn split_hostport(hostport: &str) -> Result<(String, Option<u16>), String> {
    if let Some(stripped) = hostport.strip_prefix('[') {
        let (host, after) = stripped
            .split_once(']')
            .ok_or("missing closing IPv6 bracket")?;
        let port = match after.strip_prefix(':') {
            None if after.is_empty() => None,
            None => return Err("invalid proxy port".into()),
            Some(port) => Some(parse_port(port)?),
        };
        Ok((host.to_owned(), port))
    } else {
        match hostport.rsplit_once(':') {
            Some((host, _)) if host.contains(':') => {
                Err("IPv6 proxy addresses require brackets".into())
            }
            Some((host, port)) => Ok((host.to_owned(), Some(parse_port(port)?))),
            None => Ok((hostport.to_owned(), None)),
        }
    }
}
fn parse_port(value: &str) -> Result<u16, String> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("invalid proxy port".into());
    }
    value.parse().map_err(|_| "invalid proxy port".into())
}
pub fn default_port(scheme: &str) -> u16 {
    if scheme == "http" { 80 } else { 1080 }
}
