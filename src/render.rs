//! Rendering a plan/environment as shell code, `env` text or JSON.

use crate::compose::{ExportMode, HOST_ID_VARS, PROXY_VARS, Plan};
use crate::parser::Env;
use crate::util::{json_escape, now_rfc3339};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Shell,
    Env,
    Json,
}

pub struct RenderOpts {
    pub format: Format,
    pub redact: bool,
}

const SECRET_SUFFIXES: &[&str] = &[
    "_TOKEN",
    "_SECRET",
    "_SECRETS",
    "_PASSWORD",
    "_PASSWD",
    "_PWD",
    "_KEY",
    "_KEYS",
    "_APIKEY",
    "_API_KEY",
    "_CREDENTIAL",
    "_CREDENTIALS",
    "_AUTH",
    "_AUTHORIZATION",
    "_SESSION",
];

const SECRET_NAMES: &[&str] = &["SECRET", "TOKEN", "PASSWORD", "PASSWD", "APIKEY"];

const SECRET_EXTRA: &[&str] = &[
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_API_KEY",
    "GITHUB_TOKEN",
    "GH_TOKEN",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
];

/// Heuristic: does this variable name denote a secret?
pub fn is_secret_name(name: &str) -> bool {
    let up = name.to_ascii_uppercase();
    if SECRET_NAMES.contains(&up.as_str()) || SECRET_EXTRA.contains(&up.as_str()) {
        return true;
    }
    if up.contains("COOKIE") {
        return true;
    }
    SECRET_SUFFIXES.iter().any(|s| up.ends_with(s))
}

/// Redact a value if `--redact` applies to this name.
pub fn redact_value(name: &str, value: &str) -> String {
    if PROXY_VARS.contains(&name) {
        return match crate::proxy::parse_proxy_url(value) {
            Ok(_) => crate::proxy::redact_proxy(value),
            Err(_) => "<redacted>".to_string(),
        };
    }
    if HOST_ID_VARS.contains(&name) {
        return "<redacted>".to_string();
    }
    if is_secret_name(name) {
        return "<redacted>".to_string();
    }
    value.to_string()
}

/// POSIX-safe single-quoted shell word.
pub fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    let safe = value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
    if safe {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for c in value.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

fn rendered_env(env: &Env, opts: &RenderOpts) -> Vec<(String, String)> {
    env.iter()
        .map(|(k, v)| {
            let v = if opts.redact {
                redact_value(k, v)
            } else {
                v.clone()
            };
            (k.clone(), v)
        })
        .collect()
}

/// Render a whole environment.
pub fn render(env: &Env, opts: &RenderOpts) -> String {
    let pairs = rendered_env(env, opts);
    match opts.format {
        Format::Shell => {
            let mut out = String::new();
            for (k, v) in pairs {
                out.push_str(&k);
                out.push('=');
                out.push_str(&shell_quote(&v));
                out.push('\n');
            }
            out
        }
        Format::Env => {
            let mut out = String::new();
            for (k, v) in pairs {
                out.push_str(&k);
                out.push('=');
                out.push_str(&v);
                out.push('\n');
            }
            out
        }
        Format::Json => json_document(&pairs, &[], opts.redact, 0),
    }
}

/// Render `unset` statements.
pub fn render_unsets(names: &[String], format: Format) -> String {
    match format {
        Format::Json => String::new(),
        _ => {
            let mut sorted: Vec<&String> = names.iter().collect();
            sorted.sort();
            sorted.dedup();
            if sorted.is_empty() {
                return String::new();
            }
            let joined: Vec<&str> = sorted.iter().map(|s| s.as_str()).collect();
            format!("unset {}\n", joined.join(" "))
        }
    }
}

fn json_document(
    pairs: &[(String, String)],
    unsets: &[String],
    redacted: bool,
    skipped_non_utf8: usize,
) -> String {
    let mut out = String::new();
    out.push_str("{\"schema_version\":\"1.0\",");
    out.push_str(&format!(
        "\"tool\":{{\"name\":\"envwrap\",\"version\":\"{}\"}},",
        crate::tool_version()
    ));
    out.push_str(&format!("\"generated_at\":\"{}\",", now_rfc3339()));
    out.push_str("\"format\":\"json\",");
    out.push_str(&format!("\"redacted\":{redacted},"));
    out.push_str(&format!("\"skipped_non_utf8\":{skipped_non_utf8},"));
    out.push_str("\"variables\":{");
    let mut sorted: Vec<&(String, String)> = pairs.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    sorted.dedup_by(|a, b| a.0 == b.0);
    for (i, (k, v)) in sorted.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!("\"{}\":\"{}\"", json_escape(k), json_escape(v)));
    }
    out.push_str("},\"unset\":[");
    let mut us: Vec<&String> = unsets.iter().collect();
    us.sort();
    us.dedup();
    for (i, u) in us.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!("\"{}\"", json_escape(u)));
    }
    out.push_str("]}\n");
    out
}

/// Render an `export` of a plan, honouring mode and `--names` filtering.
pub fn render_export(
    plan: &Plan,
    base: &Env,
    opts: &RenderOpts,
    names: Option<&[String]>,
    skipped_non_utf8: usize,
) -> String {
    let mut entries: Vec<&crate::parser::Entry> = match plan.export_mode {
        ExportMode::Full => plan.entries.iter().collect(),
        ExportMode::Diff => plan
            .entries
            .iter()
            .filter(|e| base.get(&e.name) != Some(e.value.as_str()))
            .collect(),
    };
    let mut unsets: Vec<String> = match plan.export_mode {
        ExportMode::Full => plan.unsets.clone(),
        ExportMode::Diff => plan
            .unsets
            .iter()
            .filter(|n| base.contains(n))
            .cloned()
            .collect(),
    };

    if let Some(list) = names {
        for n in list {
            if !plan.entries.iter().any(|e| &e.name == n) {
                eprintln!("envwrap: warning: name '{n}' is not defined by the selected source");
            }
        }
        entries.retain(|e| list.contains(&e.name));
        unsets.retain(|u| list.contains(u));
    }

    let mut out = String::new();
    let pairs: Vec<(String, String)> = entries
        .iter()
        .map(|e| {
            let v = if opts.redact {
                redact_value(&e.name, &e.value)
            } else {
                e.value.clone()
            };
            (e.name.clone(), v)
        })
        .collect();

    match opts.format {
        Format::Shell => {
            for (k, v) in &pairs {
                out.push_str(k);
                out.push('=');
                out.push_str(&shell_quote(v));
                out.push('\n');
            }
            out.push_str(&render_unsets(&unsets, Format::Shell));
        }
        Format::Env => {
            for (k, v) in &pairs {
                if v.contains('\n') {
                    eprintln!(
                        "envwrap: warning: {k} contains a newline; 'env' format cannot round-trip it"
                    );
                }
                out.push_str(k);
                out.push('=');
                out.push_str(v);
                out.push('\n');
            }
            out.push_str(&render_unsets(&unsets, Format::Env));
        }
        Format::Json => {
            out.push_str(&json_document(
                &pairs,
                &unsets,
                opts.redact,
                skipped_non_utf8,
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Entry;

    fn opts(format: Format, redact: bool) -> RenderOpts {
        RenderOpts { format, redact }
    }

    #[test]
    fn quoting() {
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote("$HOME"), "'$HOME'");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("a\nb"), "'a\nb'");
        assert_eq!(shell_quote("a/b:c,d"), "a/b:c,d");
    }

    #[test]
    fn env_format_verbatim() {
        let mut env = Env::default();
        env.set("A", " x y ");
        assert_eq!(render(&env, &opts(Format::Env, false)), "A= x y \n");
    }

    #[test]
    fn json_escaping() {
        let mut env = Env::default();
        env.set("A", "q\"b\\c\nd");
        let s = render(&env, &opts(Format::Json, false));
        assert!(s.contains(r#""A":"q\"b\\c\nd""#), "{s}");
        assert!(s.ends_with("]}\n"));
    }

    #[test]
    fn secrets() {
        assert!(is_secret_name("MY_API_KEY"));
        assert!(!is_secret_name("TOKENIZER"));
        assert!(is_secret_name("HF_TOKEN"));
        assert!(is_secret_name("token"));
        assert!(is_secret_name("MY_COOKIE_JAR"));
        assert!(!is_secret_name("PATH"));
    }

    #[test]
    fn redaction_keeps_proxy_port() {
        assert_eq!(
            redact_value("https_proxy", "http://u:p@h:3128"),
            "http://<redacted>@h:3128"
        );
        assert_eq!(redact_value("HOSTNAME", "box"), "<redacted>");
        assert_eq!(redact_value("PATH", "/bin"), "/bin");
    }

    fn plan(pairs: &[(&str, &str)], unsets: &[&str], mode: ExportMode) -> Plan {
        Plan {
            entries: pairs
                .iter()
                .map(|(n, v)| Entry {
                    name: n.to_string(),
                    value: v.to_string(),
                })
                .collect(),
            unsets: unsets.iter().map(|s| s.to_string()).collect(),
            export_mode: mode,
        }
    }

    #[test]
    fn full_vs_diff() {
        let mut base = Env::default();
        base.set("SAME", "1");
        base.set("GONE", "x");
        let p = plan(
            &[("SAME", "1"), ("NEW", "2")],
            &["GONE", "ABSENT"],
            ExportMode::Full,
        );
        let full = render_export(&p, &base, &opts(Format::Shell, false), None, 0);
        assert_eq!(full, "SAME=1\nNEW=2\nunset ABSENT GONE\n");

        let p = plan(
            &[("SAME", "1"), ("NEW", "2")],
            &["GONE", "ABSENT"],
            ExportMode::Diff,
        );
        let diff = render_export(&p, &base, &opts(Format::Shell, false), None, 0);
        assert_eq!(diff, "NEW=2\nunset GONE\n");
    }

    #[test]
    fn names_filter() {
        let p = plan(&[("A", "1"), ("B", "2")], &[], ExportMode::Full);
        let s = render_export(
            &p,
            &Env::default(),
            &opts(Format::Shell, false),
            Some(&["A".to_string()]),
            0,
        );
        assert_eq!(s, "A=1\n");
    }

    #[test]
    fn redacted_export() {
        let p = plan(
            &[("API_TOKEN", "abc"), ("PLAIN", "hello world")],
            &[],
            ExportMode::Full,
        );
        let s = render_export(&p, &Env::default(), &opts(Format::Shell, true), None, 0);
        assert_eq!(s, "API_TOKEN='<redacted>'\nPLAIN='hello world'\n");
    }
}
