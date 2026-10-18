//! Pattern expansion for `run`/`wrap`: `${NAME}`, `$$` and `{envwrap}`.

use crate::parser::Env;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tok {
    Literal(String),
    Var(String),
}

/// Split a pattern into literals and variables.
pub fn parse_pattern(s: &str) -> Vec<Tok> {
    let mut toks = Vec::new();
    let mut lit = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            lit.push(c);
            continue;
        }
        match chars.peek() {
            Some('$') => {
                chars.next();
                lit.push('$');
            }
            Some('{') => {
                chars.next();
                let mut name = String::new();
                let mut closed = false;
                for c2 in chars.by_ref() {
                    if c2 == '}' {
                        closed = true;
                        break;
                    }
                    name.push(c2);
                }
                if closed && crate::util::is_env_name(&name) {
                    if !lit.is_empty() {
                        toks.push(Tok::Literal(std::mem::take(&mut lit)));
                    }
                    toks.push(Tok::Var(name));
                } else if closed {
                    lit.push_str("${");
                    lit.push_str(&name);
                    lit.push('}');
                } else {
                    lit.push_str("${");
                    lit.push_str(&name);
                }
            }
            _ => lit.push('$'),
        }
    }
    if !lit.is_empty() {
        toks.push(Tok::Literal(lit));
    }
    toks
}

pub struct Expansion {
    pub value: String,
    pub missing: Vec<String>,
}

/// Expand tokens against `env` in a single pass.
pub fn expand(toks: &[Tok], env: &Env) -> Expansion {
    let mut value = String::new();
    let mut missing = Vec::new();
    for t in toks {
        match t {
            Tok::Literal(s) => value.push_str(s),
            Tok::Var(name) => match env.get(name) {
                Some(v) => value.push_str(v),
                None => {
                    if !missing.contains(name) {
                        missing.push(name.clone());
                    }
                }
            },
        }
    }
    Expansion { value, missing }
}

/// `{envwrap}` as a whole argv element.
pub fn is_envwrap_token(s: &str) -> bool {
    s == "{envwrap}"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Env {
        let mut e = Env::default();
        for (k, v) in pairs {
            e.set(k, *v);
        }
        e
    }

    #[test]
    fn variable_with_spaces_is_one_token() {
        let e = env(&[("A", "x y")]);
        let x = expand(&parse_pattern("${A}"), &e);
        assert_eq!(x.value, "x y");
        assert!(x.missing.is_empty());
    }

    #[test]
    fn double_dollar() {
        let x = expand(&parse_pattern("$${A}"), &env(&[("A", "nope")]));
        assert_eq!(x.value, "${A}");
    }

    #[test]
    fn missing_collected() {
        let x = expand(&parse_pattern("a${MISSING}b"), &Env::default());
        assert_eq!(x.value, "ab");
        assert_eq!(x.missing, ["MISSING"]);
    }

    #[test]
    fn unterminated_brace_is_literal() {
        let x = expand(&parse_pattern("${abc"), &Env::default());
        assert_eq!(x.value, "${abc");
        assert!(x.missing.is_empty());
    }

    #[test]
    fn lone_dollar_literal() {
        let x = expand(&parse_pattern("a$b"), &Env::default());
        assert_eq!(x.value, "a$b");
    }

    #[test]
    fn single_pass_no_rescan() {
        let e = env(&[("A", "${B}"), ("B", "bee")]);
        let x = expand(&parse_pattern("${A}"), &e);
        assert_eq!(x.value, "${B}");
    }

    #[test]
    fn envwrap_token_exact() {
        assert!(is_envwrap_token("{envwrap}"));
        assert!(!is_envwrap_token("x{envwrap}"));
        assert!(!is_envwrap_token("{envwrap}x"));
    }
}
