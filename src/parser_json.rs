//! Hand-written RFC 8259 JSON reader for `envwrap` input files.

use crate::parser::{Entry, Env, Finding, ParsedFile, Severity};

const MAX_DEPTH: usize = 128;

pub fn parse(text: &str, _ambient: &Env) -> ParsedFile {
    let mut out = ParsedFile::default();
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    let mut p = Parser {
        s: text.as_bytes(),
        i: 0,
    };
    p.ws();
    if p.peek() != Some(b'{') {
        let kind = p.type_name();
        out.findings.push(Finding {
            line: None,
            severity: Severity::Error,
            message: format!("invalid JSON: top level is {kind}, expected object"),
        });
        return out;
    }
    match p.object(1) {
        Ok(members) => {
            p.ws();
            if p.peek().is_some() {
                out.findings.push(Finding {
                    line: None,
                    severity: Severity::Error,
                    message: format!(
                        "invalid JSON at byte {}: trailing characters after top-level object",
                        p.i
                    ),
                });
                return out;
            }
            for (key, val) in members {
                match val {
                    Value::Str(v) => {
                        if key.contains('\0') || !crate::util::is_env_name(&key) {
                            out.findings.push(Finding {
                                line: None,
                                severity: Severity::Error,
                                message: format!("invalid variable name '{key}'"),
                            });
                            continue;
                        }
                        out.entries.retain(|e| e.name != key);
                        out.entries.push(Entry {
                            name: key,
                            value: v,
                        });
                    }
                    Value::Null => out.unsets.push(key),
                    _ => out.findings.push(Finding {
                        line: None,
                        severity: Severity::Error,
                        message: format!("key '{key}': non-string value ignored"),
                    }),
                }
            }
        }
        Err((off, reason)) => out.findings.push(Finding {
            line: None,
            severity: Severity::Error,
            message: format!("invalid JSON at byte {off}: {reason}"),
        }),
    }
    out
}

enum Value {
    Str(String),
    Null,
    Other,
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

type PErr = (usize, String);

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn ws(&mut self) {
        while let Some(c) = self.peek() {
            if matches!(c, b' ' | b'\t' | b'\n' | b'\r') {
                self.i += 1;
            } else {
                break;
            }
        }
    }

    fn type_name(&self) -> &'static str {
        match self.peek() {
            None => "end of input",
            Some(b'{') => "object",
            Some(b'[') => "array",
            Some(b'"') => "string",
            Some(b't') | Some(b'f') => "boolean",
            Some(b'n') => "null",
            Some(_) => "number",
        }
    }

    fn object(&mut self, depth: usize) -> Result<Vec<(String, Value)>, PErr> {
        if depth > MAX_DEPTH {
            return Err((self.i, "nesting too deep".to_string()));
        }
        self.i += 1; // '{'
        let mut members: Vec<(String, Value)> = Vec::new();
        self.ws();
        if self.peek() == Some(b'}') {
            self.i += 1;
            return Ok(members);
        }
        loop {
            self.ws();
            if self.peek() != Some(b'"') {
                return Err((self.i, "expected object key".to_string()));
            }
            let key = self.string()?;
            self.ws();
            if self.peek() != Some(b':') {
                return Err((self.i, "expected ':'".to_string()));
            }
            self.i += 1;
            self.ws();
            let val = self.value(depth)?;
            members.retain(|(k, _)| *k != key);
            members.push((key, val));
            self.ws();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    return Ok(members);
                }
                _ => return Err((self.i, "expected ',' or '}'".to_string())),
            }
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, PErr> {
        match self.peek() {
            Some(b'"') => Ok(Value::Str(self.string()?)),
            Some(b'{') => {
                self.object(depth + 1)?;
                Ok(Value::Other)
            }
            Some(b'[') => {
                self.array(depth + 1)?;
                Ok(Value::Other)
            }
            Some(b'n') => {
                self.literal("null")?;
                Ok(Value::Null)
            }
            Some(b't') => {
                self.literal("true")?;
                Ok(Value::Other)
            }
            Some(b'f') => {
                self.literal("false")?;
                Ok(Value::Other)
            }
            Some(b'-') | Some(b'0'..=b'9') => {
                self.number()?;
                Ok(Value::Other)
            }
            Some(_) => Err((self.i, "unexpected character".to_string())),
            None => Err((self.i, "unexpected end of input".to_string())),
        }
    }

    fn array(&mut self, depth: usize) -> Result<(), PErr> {
        if depth > MAX_DEPTH {
            return Err((self.i, "nesting too deep".to_string()));
        }
        self.i += 1; // '['
        self.ws();
        if self.peek() == Some(b']') {
            self.i += 1;
            return Ok(());
        }
        loop {
            self.ws();
            self.value(depth)?;
            self.ws();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b']') => {
                    self.i += 1;
                    return Ok(());
                }
                _ => return Err((self.i, "expected ',' or ']'".to_string())),
            }
        }
    }

    fn literal(&mut self, word: &str) -> Result<(), PErr> {
        if self.s[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(())
        } else {
            Err((self.i, "invalid literal".to_string()))
        }
    }

    fn number(&mut self) -> Result<(), PErr> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        let digits_start = self.i;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.i += 1;
        }
        if self.i == digits_start {
            return Err((start, "invalid number".to_string()));
        }
        if self.peek() == Some(b'.') {
            self.i += 1;
            let f = self.i;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.i += 1;
            }
            if self.i == f {
                return Err((start, "invalid number".to_string()));
            }
        }
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            self.i += 1;
            if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                self.i += 1;
            }
            let e = self.i;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.i += 1;
            }
            if self.i == e {
                return Err((start, "invalid number".to_string()));
            }
        }
        Ok(())
    }

    fn string(&mut self) -> Result<String, PErr> {
        let start = self.i;
        self.i += 1; // opening quote
        let mut out: Vec<u8> = Vec::new();
        loop {
            let Some(c) = self.peek() else {
                return Err((start, "unterminated string".to_string()));
            };
            match c {
                b'"' => {
                    self.i += 1;
                    return String::from_utf8(out)
                        .map_err(|_| (start, "invalid UTF-8 in string".to_string()));
                }
                b'\\' => {
                    self.i += 1;
                    let Some(esc) = self.peek() else {
                        return Err((start, "unterminated escape".to_string()));
                    };
                    self.i += 1;
                    match esc {
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        b'/' => out.push(b'/'),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0c),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let hi = self.hex4()?;
                            let cp = if (0xD800..0xDC00).contains(&hi) {
                                if self.s[self.i..].starts_with(b"\\u") {
                                    self.i += 2;
                                    let lo = self.hex4()?;
                                    if (0xDC00..0xE000).contains(&lo) {
                                        0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                                    } else {
                                        return Err((self.i, "invalid surrogate pair".to_string()));
                                    }
                                } else {
                                    return Err((self.i, "unpaired surrogate".to_string()));
                                }
                            } else if (0xDC00..0xE000).contains(&hi) {
                                return Err((self.i, "unpaired surrogate".to_string()));
                            } else {
                                hi
                            };
                            let ch = char::from_u32(cp)
                                .ok_or_else(|| (self.i, "invalid code point".to_string()))?;
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        }
                        _ => return Err((self.i - 1, "invalid escape".to_string())),
                    }
                }
                0x00..=0x1f => {
                    return Err((self.i, "control character in string".to_string()));
                }
                _ => {
                    out.push(c);
                    self.i += 1;
                }
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, PErr> {
        if self.i + 4 > self.s.len() {
            return Err((self.i, "truncated \\u escape".to_string()));
        }
        let mut v = 0u32;
        for k in 0..4 {
            let c = self.s[self.i + k];
            let d = match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                b'A'..=b'F' => c - b'A' + 10,
                _ => return Err((self.i + k, "invalid hex digit".to_string())),
            };
            v = v * 16 + d as u32;
        }
        self.i += 4;
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> ParsedFile {
        parse(s, &Env::default())
    }

    #[test]
    fn strings() {
        let r = p(r#"{"A":"1","B":"x y"}"#);
        assert!(r.findings.is_empty());
        assert_eq!(r.entries[0].value, "1");
        assert_eq!(r.entries[1].value, "x y");
    }

    #[test]
    fn null_is_unset() {
        let r = p(r#"{"A":null}"#);
        assert_eq!(r.unsets, ["A"]);
    }

    #[test]
    fn number_is_error() {
        let r = p(r#"{"A":1}"#);
        assert_eq!(r.findings.len(), 1);
        assert!(r.entries.is_empty());
    }

    #[test]
    fn nested_object_is_error() {
        let r = p(r#"{"A":{"B":"1"}}"#);
        assert_eq!(r.findings.len(), 1);
    }

    #[test]
    fn trailing_garbage_offset() {
        let r = p(r#"{"A":"1"} x"#);
        assert_eq!(r.findings.len(), 1);
        assert!(
            r.findings[0].message.contains("byte 10"),
            "{}",
            r.findings[0].message
        );
    }

    #[test]
    fn surrogate_pair() {
        let r = p(r#"{"A":"\uD83D\uDE00"}"#);
        assert_eq!(r.entries[0].value, "😀");
    }

    #[test]
    fn too_deep() {
        let mut deep = String::new();
        for _ in 0..129 {
            deep.push('[');
        }
        deep.push('0');
        for _ in 0..129 {
            deep.push(']');
        }
        let r = p(&format!(r#"{{"A":{deep}}}"#));
        assert_eq!(r.findings.len(), 1);
        assert!(r.findings[0].message.contains("nesting too deep"));
    }

    #[test]
    fn duplicate_last_wins() {
        let r = p(r#"{"A":"1","A":"2"}"#);
        assert!(r.findings.is_empty());
        assert_eq!(r.entries.len(), 1);
        assert_eq!(r.entries[0].value, "2");
    }

    #[test]
    fn top_level_array() {
        let r = p("[]");
        assert_eq!(r.findings.len(), 1);
        assert!(r.findings[0].message.contains("top level is array"));
    }

    #[test]
    fn invalid_name() {
        let r = p(r#"{"1A":"1"}"#);
        assert_eq!(r.findings.len(), 1);
        assert!(r.entries.is_empty());
    }
}
