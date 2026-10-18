//! Environment model and file-parser dispatcher.

use std::collections::BTreeMap;

/// A single `NAME=value` pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub value: String,
}

/// A deterministic snapshot of an environment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Env {
    pub vars: BTreeMap<String, String>,
}

impl Env {
    /// Snapshot the current process environment. Entries whose name or value is
    /// not valid UTF-8 are dropped and counted.
    pub fn from_process() -> (Env, usize) {
        let mut env = Env::default();
        let mut skipped = 0;
        for (k, v) in std::env::vars_os() {
            match (k.into_string(), v.into_string()) {
                (Ok(k), Ok(v)) => {
                    env.vars.insert(k, v);
                }
                _ => skipped += 1,
            }
        }
        (env, skipped)
    }

    /// Parse a NUL-separated `environ` blob. Last definition wins.
    pub fn from_bytes(environ: &[u8]) -> (Env, usize) {
        let mut env = Env::default();
        let mut skipped = 0;
        for seg in environ.split(|b| *b == 0) {
            if seg.is_empty() {
                continue;
            }
            let Some(eq) = seg.iter().position(|b| *b == b'=') else {
                skipped += 1;
                continue;
            };
            let (name, value) = (&seg[..eq], &seg[eq + 1..]);
            match (std::str::from_utf8(name), std::str::from_utf8(value)) {
                (Ok(n), Ok(v)) if crate::util::is_env_name(n) => {
                    env.vars.insert(n.to_string(), v.to_string());
                }
                _ => skipped += 1,
            }
        }
        (env, skipped)
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.vars.get(name).map(|s| s.as_str())
    }

    pub fn set(&mut self, name: &str, value: impl Into<String>) {
        self.vars.insert(name.to_string(), value.into());
    }

    pub fn remove(&mut self, name: &str) -> Option<String> {
        self.vars.remove(name)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.vars.contains_key(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &String)> {
        self.vars.iter()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub line: Option<usize>,
    pub severity: Severity,
    pub message: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedFile {
    pub entries: Vec<Entry>,
    pub unsets: Vec<String>,
    pub findings: Vec<Finding>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileFormat {
    Dotenv,
    Env,
    Json,
}

/// Parse `text` in the given format. Never stops early.
pub fn parse(text: &str, format: FileFormat, ambient: &Env) -> ParsedFile {
    match format {
        FileFormat::Dotenv => crate::parser_dotenv::parse(text, ambient),
        FileFormat::Env => crate::parser_env::parse(text, ambient),
        FileFormat::Json => crate::parser_json::parse(text, ambient),
    }
}

/// Auto-detect: JSON when the first non-whitespace char (after a BOM) is `{`,
/// otherwise dotenv. `Env` is never auto-detected.
pub fn detect(text: &str) -> FileFormat {
    let t = text.strip_prefix('\u{FEFF}').unwrap_or(text).trim_start();
    if t.starts_with('{') {
        FileFormat::Json
    } else {
        FileFormat::Dotenv
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_json() {
        assert_eq!(detect("{\"A\":\"1\"}"), FileFormat::Json);
        assert_eq!(detect("A=1\n"), FileFormat::Dotenv);
        assert_eq!(detect("\u{FEFF}{\"A\":1}"), FileFormat::Json);
        assert_eq!(detect("\n\n  {\"A\":1}"), FileFormat::Json);
    }

    #[test]
    fn from_bytes_last_wins() {
        let (env, skipped) = Env::from_bytes(b"A=1\0B=2\0A=3\0");
        assert_eq!(skipped, 0);
        assert_eq!(env.get("A"), Some("3"));
        assert_eq!(env.get("B"), Some("2"));
    }

    #[test]
    fn from_bytes_skips_bad_segments() {
        let (env, skipped) = Env::from_bytes(b"NOEQ\0=A\0A=1\0");
        assert_eq!(skipped, 2);
        assert_eq!(env.get("A"), Some("1"));
    }

    #[test]
    fn from_bytes_counts_invalid_utf8() {
        let (env, skipped) = Env::from_bytes(b"X=\xff\0Y=ok\0");
        assert_eq!(skipped, 1);
        assert_eq!(env.get("X"), None);
        assert_eq!(env.get("Y"), Some("ok"));
    }
}
