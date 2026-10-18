//! Child-process execution and unknown-variable classification.

use crate::parser::Env;
use crate::util::{EXIT_RUN_FAILED, is_env_name};

pub struct Cmd {
    pub program: String,
    pub args: Vec<String>,
}

/// Build a command from argv text. `Err` is usage text.
pub fn parse_command(tokens: &[String]) -> Result<Cmd, String> {
    let Some((program, args)) = tokens.split_first() else {
        return Err("missing command after '--'".to_string());
    };
    if program.is_empty() {
        return Err("empty command name".to_string());
    }
    Ok(Cmd {
        program: program.clone(),
        args: args.to_vec(),
    })
}

pub struct SpawnError {
    pub message: String,
    pub code: i32,
}

/// Spawn the child with an environment that is exactly `env`.
pub fn spawn(env: &Env, cmd: &Cmd) -> Result<std::process::ExitStatus, SpawnError> {
    std::process::Command::new(&cmd.program)
        .args(&cmd.args)
        .env_clear()
        .envs(env.iter())
        .status()
        .map_err(|e| SpawnError {
            message: format!("cannot execute '{}': {e}", cmd.program),
            code: EXIT_RUN_FAILED,
        })
}

/// Conventional exit code: the child's code, or `128 + signal`.
pub fn exit_code(status: &std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return 128 + sig;
        }
    }
    1
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VarClass {
    Known,
    Unknown,
}

pub const AMBIENT_ALLOWLIST: &[&str] = &[
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "PATH",
    "TERM",
    "COLORTERM",
    "TERM_PROGRAM",
    "LANG",
    "LANGUAGE",
    "TZ",
    "TMPDIR",
    "TMP",
    "TEMP",
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "PWD",
    "OLDPWD",
    "SHLVL",
    "PAGER",
    "EDITOR",
    "VISUAL",
    "HOSTNAME",
    "USERNAME",
];

pub const ALLOWED_PREFIXES: &[&str] = &[
    "LC_",
    "XDG_",
    "SSH_",
    "GIT_",
    "RUST_",
    "CARGO_",
    "NODE_",
    "npm_",
    "PYTHON",
    "CONDA",
    "VIRTUAL_ENV",
    "AWS_",
    "ANTHROPIC_",
];

/// Classify a variable name as a known environment variable or not.
pub fn classify(name: &str) -> VarClass {
    if !is_env_name(name) {
        return VarClass::Unknown;
    }
    if AMBIENT_ALLOWLIST.contains(&name) {
        return VarClass::Known;
    }
    if crate::compose::PROXY_VARS.contains(&name) {
        return VarClass::Known;
    }
    if ALLOWED_PREFIXES.iter().any(|p| name.starts_with(p)) {
        return VarClass::Known;
    }
    VarClass::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_cases() {
        assert_eq!(classify("PATH"), VarClass::Known);
        assert_eq!(classify("LC_ALL"), VarClass::Known);
        assert_eq!(classify("XDG_CONFIG_HOME"), VarClass::Known);
        assert_eq!(classify("HTTPS_PROXY"), VarClass::Known);
        assert_eq!(classify("ANTHROPIC_API_KEY"), VarClass::Known);
        assert_eq!(classify("FOO"), VarClass::Unknown);
    }

    #[test]
    fn parse_command_requires_token() {
        assert!(parse_command(&[]).is_err());
        let c = parse_command(&["echo".into(), "hi".into()]).unwrap();
        assert_eq!(c.program, "echo");
        assert_eq!(c.args, ["hi"]);
    }

    #[test]
    fn exit_codes() {
        let s = std::process::Command::new("/bin/false")
            .status()
            .expect("/bin/false");
        assert_eq!(exit_code(&s), 1);
    }
}
