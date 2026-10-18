//! `env` format parser: one `NAME=value` per line, no quoting or escapes.

use crate::parser::{Entry, Env, Finding, ParsedFile, Severity};

pub fn parse(text: &str, _ambient: &Env) -> ParsedFile {
    let mut out = ParsedFile::default();
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    for (idx, raw_line) in text.split('\n').enumerate() {
        let line_no = idx + 1;
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.is_empty() {
            continue;
        }
        match line.find('=') {
            Some(eq) => {
                let name = &line[..eq];
                if !crate::util::is_env_name(name) {
                    out.findings.push(Finding {
                        line: Some(line_no),
                        severity: Severity::Error,
                        message: format!("invalid variable name '{name}'"),
                    });
                    continue;
                }
                out.entries.retain(|e| e.name != name);
                out.entries.push(Entry {
                    name: name.to_string(),
                    value: line[eq + 1..].to_string(),
                });
            }
            None => {
                if crate::util::is_env_name(line) {
                    out.unsets.push(line.to_string());
                } else {
                    out.findings.push(Finding {
                        line: Some(line_no),
                        severity: Severity::Error,
                        message: format!("invalid variable name '{line}'"),
                    });
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> ParsedFile {
        parse(s, &Env::default())
    }

    #[test]
    fn keeps_spaces() {
        let r = p("A= x y \n");
        assert_eq!(r.entries[0].value, " x y ");
    }

    #[test]
    fn bare_name_is_unset() {
        let r = p("A\nB=1\n");
        assert_eq!(r.unsets, ["A"]);
        assert_eq!(r.entries.len(), 1);
        assert!(r.findings.is_empty());
    }

    #[test]
    fn empty_value() {
        let r = p("A=\n");
        assert_eq!(r.entries[0].value, "");
    }

    #[test]
    fn invalid_name() {
        let r = p("1A=1\n");
        assert_eq!(r.findings.len(), 1);
        assert_eq!(r.findings[0].severity, Severity::Error);
        assert!(r.entries.is_empty());
    }
}
