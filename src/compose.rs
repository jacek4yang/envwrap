//! Composition of a declared plan with ambient inheritance and unset rules.

use crate::parser::{Entry, Env, Finding, Severity};

/// Every proxy-related variable name recognized by `envwrap`.
pub const PROXY_VARS: [&str; 10] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
    "FTP_PROXY",
    "ftp_proxy",
];

/// `(lowercase, uppercase)` proxy variable pairs.
pub const MIRRORS: &[(&str, &str)] = &[
    ("http_proxy", "HTTP_PROXY"),
    ("https_proxy", "HTTPS_PROXY"),
    ("all_proxy", "ALL_PROXY"),
    ("no_proxy", "NO_PROXY"),
    ("ftp_proxy", "FTP_PROXY"),
];

/// Variables identifying the host or user rather than configuration.
pub const HOST_ID_VARS: &[&str] = &["HOSTNAME", "COMPUTERNAME", "USER", "LOGNAME"];

/// How `export` renders the plan.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExportMode {
    #[default]
    Full,
    Diff,
}

/// A declared environment: ordered entries plus names to unset.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub entries: Vec<Entry>,
    pub unsets: Vec<String>,
    pub export_mode: ExportMode,
}

/// Whether lowercase/uppercase mirror pairs are filled in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncMode {
    Synced,
    Verbatim,
}

pub struct BuildInput<'a> {
    pub base: &'a Env,
    pub declared: &'a Plan,
    pub inherited: &'a [String],
    pub unset: &'a [String],
    pub mode: SyncMode,
}

#[derive(Debug, Default)]
pub struct BuildOutput {
    pub env: Env,
    pub skipped_inherit: Vec<String>,
    pub findings: Vec<Finding>,
}

/// Apply the winner order: declared → unsets → inherited → `--unset` → mirrors.
pub fn build(input: BuildInput<'_>) -> Result<BuildOutput, String> {
    let mut out = BuildOutput::default();

    for e in &input.declared.entries {
        out.env.set(&e.name, e.value.clone());
    }
    for name in &input.declared.unsets {
        out.env.remove(name);
    }
    for name in input.unset {
        out.env.remove(name);
    }

    for name in input.inherited {
        if input.unset.iter().any(|u| u == name) {
            return Err(format!("{name} is both inherited and unset"));
        }
        match input.base.get(name) {
            Some(v) => out.env.set(name, v.to_string()),
            None => out.skipped_inherit.push(name.clone()),
        }
    }

    if input.mode == SyncMode::Synced {
        sync_mirrors(&mut out.env);
    }

    for name in input.unset {
        if input.declared.entries.iter().any(|e| &e.name == name) {
            out.findings.push(Finding {
                line: None,
                severity: Severity::Warning,
                message: format!("unset_overrides_declared: {name}"),
            });
        }
    }

    Ok(out)
}

/// Reject names that are not valid environment variable names.
pub fn validate_names(names: &[String]) -> Result<(), String> {
    for n in names {
        if !crate::util::is_env_name(n) {
            return Err(format!("invalid variable name '{n}'"));
        }
    }
    Ok(())
}

pub struct MirrorFix {
    pub filled: String,
    pub from: String,
}

/// Fill an empty or absent mirror partner from the set side. Conflicting
/// non-empty values are left untouched.
pub fn sync_mirrors(env: &mut Env) -> Vec<MirrorFix> {
    let mut fixes = Vec::new();
    for (lower, upper) in MIRRORS {
        let lo = env.get(lower).map(|s| s.to_string());
        let up = env.get(upper).map(|s| s.to_string());
        match (lo, up) {
            (Some(l), None) if !l.is_empty() => {
                env.set(upper, l.clone());
                fixes.push(MirrorFix {
                    filled: (*upper).to_string(),
                    from: (*lower).to_string(),
                });
            }
            (Some(l), Some(u)) if u.is_empty() && !l.is_empty() => {
                env.set(upper, l.clone());
                fixes.push(MirrorFix {
                    filled: (*upper).to_string(),
                    from: (*lower).to_string(),
                });
            }
            (_, Some(u)) if !u.is_empty() => {
                let lower_missing = match env.get(lower) {
                    None => true,
                    Some(l) => l.is_empty(),
                };
                if lower_missing {
                    env.set(lower, u.clone());
                    fixes.push(MirrorFix {
                        filled: (*lower).to_string(),
                        from: (*upper).to_string(),
                    });
                }
            }
            _ => {}
        }
    }
    fixes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(pairs: &[(&str, &str)], unsets: &[&str]) -> Plan {
        Plan {
            entries: pairs
                .iter()
                .map(|(n, v)| Entry {
                    name: n.to_string(),
                    value: v.to_string(),
                })
                .collect(),
            unsets: unsets.iter().map(|s| s.to_string()).collect(),
            export_mode: ExportMode::Full,
        }
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn inherit_overrides_declared() {
        let mut base = Env::default();
        base.set("FOO", "ambient");
        let p = plan(&[("FOO", "declared")], &[]);
        let out = build(BuildInput {
            base: &base,
            declared: &p,
            inherited: &s(&["FOO"]),
            unset: &[],
            mode: SyncMode::Verbatim,
        })
        .unwrap();
        assert_eq!(out.env.get("FOO"), Some("ambient"));
    }

    #[test]
    fn unsets_removed_declared_then_cli() {
        let mut base = Env::default();
        base.set("B", "1");
        let p = plan(&[("A", "1"), ("B", "2"), ("C", "3")], &["B"]);
        let out = build(BuildInput {
            base: &base,
            declared: &p,
            inherited: &[],
            unset: &s(&["C"]),
            mode: SyncMode::Verbatim,
        })
        .unwrap();
        assert_eq!(out.env.get("A"), Some("1"));
        assert_eq!(out.env.get("B"), None);
        assert_eq!(out.env.get("C"), None);
    }

    #[test]
    fn unset_vs_declared_warns() {
        let p = plan(&[("A", "1")], &[]);
        let out = build(BuildInput {
            base: &Env::default(),
            declared: &p,
            inherited: &[],
            unset: &s(&["A"]),
            mode: SyncMode::Verbatim,
        })
        .unwrap();
        assert_eq!(out.env.get("A"), None);
        assert_eq!(out.findings.len(), 1);
        assert_eq!(out.findings[0].message, "unset_overrides_declared: A");
    }

    #[test]
    fn inherit_and_unset_conflict() {
        let err = build(BuildInput {
            base: &Env::default(),
            declared: &Plan::default(),
            inherited: &s(&["A"]),
            unset: &s(&["A"]),
            mode: SyncMode::Verbatim,
        })
        .unwrap_err();
        assert_eq!(err, "A is both inherited and unset");
    }

    #[test]
    fn inherit_missing_from_base_reported() {
        let out = build(BuildInput {
            base: &Env::default(),
            declared: &Plan::default(),
            inherited: &s(&["NOPE"]),
            unset: &[],
            mode: SyncMode::Verbatim,
        })
        .unwrap();
        assert_eq!(out.skipped_inherit, ["NOPE"]);
    }

    #[test]
    fn mirror_filled_from_absent() {
        let p = plan(&[("http_proxy", "http://a:1")], &[]);
        let out = build(BuildInput {
            base: &Env::default(),
            declared: &p,
            inherited: &[],
            unset: &[],
            mode: SyncMode::Synced,
        })
        .unwrap();
        assert_eq!(out.env.get("HTTP_PROXY"), Some("http://a:1"));
    }

    #[test]
    fn mirror_filled_from_empty() {
        let mut env = Env::default();
        env.set("https_proxy", "http://b:2");
        env.set("HTTPS_PROXY", "");
        let fixes = sync_mirrors(&mut env);
        assert_eq!(env.get("HTTPS_PROXY"), Some("http://b:2"));
        assert_eq!(fixes[0].filled, "HTTPS_PROXY");
        assert_eq!(fixes[0].from, "https_proxy");
    }

    #[test]
    fn mirror_upper_only() {
        let mut env = Env::default();
        env.set("ALL_PROXY", "socks5://h:1080");
        sync_mirrors(&mut env);
        assert_eq!(env.get("all_proxy"), Some("socks5://h:1080"));
    }

    #[test]
    fn mirror_conflict_untouched() {
        let mut env = Env::default();
        env.set("http_proxy", "http://a:1");
        env.set("HTTP_PROXY", "http://b:2");
        assert!(sync_mirrors(&mut env).is_empty());
        assert_eq!(env.get("http_proxy"), Some("http://a:1"));
        assert_eq!(env.get("HTTP_PROXY"), Some("http://b:2"));
    }

    #[test]
    fn validate_names_cases() {
        assert!(validate_names(&s(&["1A"])).is_err());
        assert_eq!(
            validate_names(&s(&["1A"])).unwrap_err(),
            "invalid variable name '1A'"
        );
        assert!(validate_names(&s(&["A_1"])).is_ok());
    }
}
