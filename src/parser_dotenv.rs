//! `.env` parser: `export ` prefix, comments, quoting, escapes and `${}` expansion.

use crate::parser::{Entry, Env, Finding, ParsedFile, Severity};

pub fn parse(text: &str, ambient: &Env) -> ParsedFile {
    let mut out = ParsedFile::default();
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    let mut defined: std::collections::BTreeMap<String, usize> = Default::default();

    for (idx, raw_line) in text.split('\n').enumerate() {
        let line_no = idx + 1;
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let body = trimmed.strip_prefix("export ").unwrap_or(trimmed);
        let Some(eq) = find_eq(body) else {
            if body.contains('=') {
                let name = &body[..body.find('=').unwrap_or(0)];
                out.findings.push(Finding {
                    line: Some(line_no),
                    severity: Severity::Error,
                    message: format!("invalid variable name '{name}'"),
                });
            } else {
                out.findings.push(Finding {
                    line: Some(line_no),
                    severity: Severity::Error,
                    message: "missing '='".to_string(),
                });
            }
            continue;
        };
        let name = &body[..eq];
        if !crate::util::is_env_name(name) {
            out.findings.push(Finding {
                line: Some(line_no),
                severity: Severity::Error,
                message: format!("invalid variable name '{name}'"),
            });
            continue;
        }
        let rest = &body[eq + 1..];
        let value = parse_value(rest, line_no, &out.entries, ambient, &mut out.findings);

        if defined.insert(name.to_string(), line_no).is_some() {
            out.findings.push(Finding {
                line: Some(line_no),
                severity: Severity::Warning,
                message: format!("duplicate definition of '{name}'; last wins"),
            });
            out.entries.retain(|e| e.name != name);
        }
        out.entries.push(Entry {
            name: name.to_string(),
            value,
        });
    }
    out
}

/// Position of the `=` that ends the name. A name ends at the first char that
/// is not `[A-Za-z0-9_]`, which must be `=`.
fn find_eq(body: &str) -> Option<usize> {
    for (i, c) in body.char_indices() {
        if c == '=' {
            return Some(i);
        }
        if !(c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
    }
    None
}

fn parse_value(
    rest: &str,
    line_no: usize,
    prior: &[Entry],
    ambient: &Env,
    findings: &mut Vec<Finding>,
) -> String {
    match rest.chars().next() {
        Some('"') => match closing_double(rest) {
            Some(end) => {
                let value = expand_double(&rest[1..end], prior, ambient);
                check_trailing(&rest[end + 1..], line_no, findings);
                value
            }
            None => {
                findings.push(Finding {
                    line: Some(line_no),
                    severity: Severity::Error,
                    message: "unterminated double quote".to_string(),
                });
                expand_double(&rest[1..], prior, ambient)
            }
        },
        Some('\'') => match rest[1..].find('\'') {
            Some(end) => {
                let value = rest[1..1 + end].to_string();
                check_trailing(&rest[end + 2..], line_no, findings);
                value
            }
            None => {
                findings.push(Finding {
                    line: Some(line_no),
                    severity: Severity::Error,
                    message: "unterminated single quote".to_string(),
                });
                rest[1..].to_string()
            }
        },
        _ => {
            let mut end = rest.len();
            let bytes = rest.as_bytes();
            for i in 0..bytes.len().saturating_sub(1) {
                if bytes[i] == b' ' && bytes[i + 1] == b'#' {
                    end = i;
                    break;
                }
            }
            rest[..end].trim_end().to_string()
        }
    }
}

/// Index (relative to `rest`, which starts with the opening `"`) of the closing
/// `"`, honouring backslash escapes.
fn closing_double(rest: &str) -> Option<usize> {
    let bytes = rest.as_bytes();
    let mut i = 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return Some(i),
            _ => {
                // Advance one full UTF-8 character.
                let ch = rest[i..].chars().next().expect("in bounds");
                i += ch.len_utf8();
            }
        }
    }
    None
}

fn expand_double(content: &str, prior: &[Entry], ambient: &Env) -> String {
    let mut value = String::new();
    let mut it = content.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\\' => match it.next() {
                Some('"') => value.push('"'),
                Some('\\') => value.push('\\'),
                Some('$') => value.push('$'),
                Some('n') => value.push('\n'),
                Some('t') => value.push('\t'),
                Some('r') => value.push('\r'),
                Some(other) => {
                    value.push('\\');
                    value.push(other);
                }
                None => value.push('\\'),
            },
            '$' => {
                if it.peek() == Some(&'{') {
                    it.next();
                    let mut name = String::new();
                    let mut closed = false;
                    for c2 in it.by_ref() {
                        if c2 == '}' {
                            closed = true;
                            break;
                        }
                        name.push(c2);
                    }
                    if closed {
                        if let Some(v) = lookup(&name, prior, ambient) {
                            value.push_str(&v);
                        }
                    } else {
                        value.push_str("${");
                        value.push_str(&name);
                    }
                } else {
                    let mut name = String::new();
                    while let Some(&c2) = it.peek() {
                        if c2.is_ascii_alphanumeric() || c2 == '_' {
                            name.push(c2);
                            it.next();
                        } else {
                            break;
                        }
                    }
                    if !name.is_empty()
                        && crate::util::is_env_name(&name)
                        && let Some(v) = lookup(&name, prior, ambient)
                    {
                        value.push_str(&v);
                    } else {
                        value.push('$');
                        value.push_str(&name);
                    }
                }
            }
            c => value.push(c),
        }
    }
    value
}

fn lookup(name: &str, prior: &[Entry], ambient: &Env) -> Option<String> {
    if name.is_empty() {
        return None;
    }
    if let Some(e) = prior.iter().rev().find(|e| e.name == name) {
        return Some(e.value.clone());
    }
    ambient.get(name).map(|s| s.to_string())
}

fn check_trailing(tail: &str, line_no: usize, findings: &mut Vec<Finding>) {
    let t = tail.trim_start();
    if t.is_empty() || t.starts_with('#') {
        return;
    }
    findings.push(Finding {
        line: Some(line_no),
        severity: Severity::Warning,
        message: "trailing characters after quoted value ignored".to_string(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Severity;

    fn p(s: &str) -> ParsedFile {
        parse(s, &Env::default())
    }

    fn one(s: &str) -> String {
        let r = p(s);
        assert!(r.findings.is_empty(), "{:?}", r.findings);
        assert_eq!(r.entries.len(), 1, "{:?}", r.entries);
        r.entries[0].value.clone()
    }

    #[test]
    fn plain() {
        assert_eq!(one("A=1\n"), "1");
        assert_eq!(one("A=hello world  \n"), "hello world");
        assert_eq!(one("A=\n"), "");
    }

    #[test]
    fn export_prefix() {
        assert_eq!(one("export A=1\n"), "1");
    }

    #[test]
    fn single_quotes_keep_dollar() {
        assert_eq!(one("A='$HOME ${X} \\n'\n"), "$HOME ${X} \\n");
    }

    #[test]
    fn double_quote_escapes() {
        assert_eq!(one("A=\"a\\nb\\tc\\\"d\\\\e\\$f\"\n"), "a\nb\tc\"d\\e$f");
        assert_eq!(one("A=\"\\q\"\n"), "\\q");
    }

    #[test]
    fn expansion_from_earlier_line() {
        let r = p("B=bee\nA=\"x${B}y\"\n");
        assert_eq!(r.entries[1].value, "xbeey");
    }

    #[test]
    fn expansion_bare_name() {
        let r = p("B=bee\nA=\"$B!\"\n");
        assert_eq!(r.entries[1].value, "bee!");
    }

    #[test]
    fn expansion_from_ambient() {
        let mut amb = Env::default();
        amb.set("HOME", "/root");
        let r = parse("A=\"${HOME}/x\"\n", &amb);
        assert_eq!(r.entries[0].value, "/root/x");
    }

    #[test]
    fn escaped_dollar_is_literal() {
        let mut amb = Env::default();
        amb.set("A", "nope");
        let r = parse("A=\"\\${A}\"\n", &amb);
        assert_eq!(r.entries[0].value, "${A}");
    }

    #[test]
    fn hash_comment_only_after_space() {
        let r = p("A=1 # c\nB=x#y\n");
        assert_eq!(r.entries[0].value, "1");
        assert_eq!(r.entries[1].value, "x#y");
    }

    #[test]
    fn url_fragment_kept() {
        assert_eq!(one("URL=http://x/#frag\n"), "http://x/#frag");
    }

    #[test]
    fn unterminated_double() {
        let r = p("A=\"abc\n");
        assert_eq!(r.findings.len(), 1);
        assert_eq!(r.findings[0].severity, Severity::Error);
        assert!(r.findings[0].message.contains("unterminated double quote"));
        assert_eq!(r.entries[0].value, "abc");
    }

    #[test]
    fn unterminated_single() {
        let r = p("A='abc\n");
        assert_eq!(r.findings[0].severity, Severity::Error);
        assert!(r.findings[0].message.contains("unterminated single quote"));
        assert_eq!(r.entries[0].value, "abc");
    }

    #[test]
    fn duplicate_last_wins() {
        let r = p("A=1\nA=2\n");
        assert_eq!(r.entries.len(), 1);
        assert_eq!(r.entries[0].value, "2");
        assert!(
            r.findings
                .iter()
                .any(|f| f.severity == Severity::Warning
                    && f.message.contains("duplicate definition"))
        );
    }

    #[test]
    fn invalid_name() {
        let r = p("1A=1\n");
        assert_eq!(r.findings.len(), 1);
        assert_eq!(r.findings[0].severity, Severity::Error);
        assert!(r.findings[0].message.contains("invalid variable name"));
    }

    #[test]
    fn crlf() {
        assert_eq!(one("A=1\r\n"), "1");
        let r = p("A=1\r\nB=2\r\n");
        assert_eq!(r.entries.len(), 2);
        assert_eq!(r.entries[1].value, "2");
    }

    #[test]
    fn bom_stripped() {
        assert_eq!(one("\u{FEFF}A=1\n"), "1");
    }

    #[test]
    fn comments_and_blanks_skipped() {
        let r = p("# c\n\n   \n  # indented\nA=1\n");
        assert_eq!(r.entries.len(), 1);
        assert!(r.findings.is_empty());
    }

    #[test]
    fn trailing_chars_warning() {
        let r = p("A=\"x\" junk\n");
        assert_eq!(r.entries[0].value, "x");
        assert!(
            r.findings
                .iter()
                .any(|f| f.severity == Severity::Warning
                    && f.message.contains("trailing characters"))
        );
        assert!(p("A=\"x\" # ok\n").findings.is_empty());
    }

    #[test]
    fn missing_eq() {
        let r = p("A\n");
        assert_eq!(r.findings[0].message, "missing '='");
    }

    #[test]
    fn parse_continues_after_error() {
        let r = p("bad line\nA=1\n");
        assert_eq!(r.entries.len(), 1);
        assert_eq!(r.findings.len(), 1);
    }
}
