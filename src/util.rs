//! Exit codes, diagnostics, IO and small formatting helpers.

use std::path::Path;

pub const EXIT_OK: i32 = 0;
pub const EXIT_RUN_FAILED: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_POLICY_FAILED: i32 = 3;
pub const EXIT_PARSE_FAILED: i32 = 4;
pub const EXIT_UNKNOWN_FATAL: i32 = 5; // reserved: never returned in v0.1.0

/// Print a usage error and return [`EXIT_USAGE`].
pub fn usage(msg: &str) -> i32 {
    eprintln!("envwrap: error: {msg}");
    EXIT_USAGE
}

/// Print an error and return `code`.
pub fn die(code: i32, msg: &str) -> i32 {
    eprintln!("envwrap: error: {msg}");
    code
}

/// Read a file or stdin to EOF.
pub fn read_input(from_file: Option<&Path>) -> Result<Vec<u8>, i32> {
    match from_file {
        None => {
            use std::io::Read;
            let mut buf = Vec::new();
            match std::io::stdin().read_to_end(&mut buf) {
                Ok(_) => Ok(buf),
                Err(e) => {
                    eprintln!("envwrap: error: cannot read stdin: {e}");
                    Err(EXIT_USAGE)
                }
            }
        }
        Some(path) => match std::fs::read(path) {
            Ok(b) => Ok(b),
            Err(e) => {
                eprintln!("envwrap: error: cannot read {}: {e}", path.display());
                Err(EXIT_USAGE)
            }
        },
    }
}

/// `[A-Za-z_][A-Za-z0-9_]*`
pub fn is_env_name(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Convert Unix seconds to UTC civil date/time (Hinnant's algorithm).
pub fn civil_from_unix(secs: i64) -> (i32, u32, u32, u32, u32, u32) {
    let secs = secs.max(0);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (hh, mm, ss) = (
        (rem / 3600) as u32,
        ((rem % 3600) / 60) as u32,
        (rem % 60) as u32,
    );
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32, hh, mm, ss)
}

/// Current UTC time formatted as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (y, mo, d, hh, mm, ss) = civil_from_unix(secs);
    format!("{y:04}-{mo:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Escape a string for use inside a JSON string literal (no surrounding quotes).
pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// Truncate to at most `max` characters (never splits a char).
pub fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_epoch() {
        assert_eq!(civil_from_unix(0), (1970, 1, 1, 0, 0, 0));
    }

    #[test]
    fn civil_known() {
        assert_eq!(civil_from_unix(1_729_180_800), (2024, 10, 17, 16, 0, 0));
    }

    #[test]
    fn civil_negative_clamps() {
        assert_eq!(civil_from_unix(-5), (1970, 1, 1, 0, 0, 0));
    }

    #[test]
    fn names() {
        assert!(is_env_name("A_1"));
        assert!(!is_env_name("1A"));
        assert!(!is_env_name(""));
        assert!(!is_env_name("A-B"));
    }

    #[test]
    fn escapes() {
        assert_eq!(json_escape("a\"b\\c\nd"), "a\\\"b\\\\c\\nd");
    }
}
