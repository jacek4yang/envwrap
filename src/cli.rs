//! Argument parsing and subcommand dispatch.
//!
//! `dispatch` receives argv without the program name.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::compose::{BuildInput, ExportMode, Plan, SyncMode, build, validate_names};
use crate::exec::{self, VarClass, classify};
use crate::output::{PolicyReport, report};
use crate::parser::{self, Env, FileFormat, ParsedFile, Severity};
use crate::render::{self, Format, RenderOpts};
use crate::util::{EXIT_OK, EXIT_USAGE, read_input, usage};

const HELP: &str = "\
envwrap — run commands under a declared, sanitized environment

USAGE:
  envwrap export [OPTIONS]            print the environment as raw/redacted shell code
  envwrap import [OPTIONS]            load a file into this process
  envwrap check  [OPTIONS]            validate a file (names, mirrors, proxy reachability)
  envwrap run    [OPTIONS] -- PATTERN...   run a command with exactly the declared env
  envwrap wrap   [OPTIONS] -- CMD ARG...   run a command layered over the ambient env
  envwrap --help | --version

OPTIONS (export):
  --from-file F       read F instead of stdin
  --format F          shell (default) | env | json
  --redact            replace secret values with '<redacted>'
  --names a,b,c       only these names
  --names-file F      only names listed in F
  --export-mode M     full (default) | diff
  --strict            treat warnings as errors

OPTIONS (import):
  --from-file F       read F instead of stdin
  --format F          auto (default) | dotenv | env | json
  --overwrite         replace variables that are already set
  --strict            treat warnings as errors

OPTIONS (check):
  --from-file F       read F instead of stdin
  --format F          auto (default) | dotenv | env | json
  --allow-unknown N   do not warn about variable N (repeatable)
  --allow-unset-mirror N
                      do not flag a mirror conflict for N (repeatable)
  --no-proxy-check    skip proxy reachability
  --proxy-timeout-ms N  total connect budget per proxy (default 300)
  --redact            redact hosts in findings
  --json              emit a JSON findings document
  --strict            treat warnings as errors

OPTIONS (run, wrap):
  --from-file F       layered plan file
  --inherit N         take N from the ambient environment (repeatable)
  --unset N           remove N (repeatable)
  --format F          input format: dotenv (default) | env | json
  --strict            treat warnings as errors
  wrap only:
  --allow-unknown N   do not warn about variable N (repeatable)
  --allow-unset-mirror N
                      do not flag a mirror conflict for N (repeatable)

PATTERNS:
  ${NAME} expands from the child environment; $$ is a literal '$'.
  {envwrap} as a whole argv word expands to this executable's path.
";

/// CLI-level options after parsing.
#[derive(Debug, Default, Clone)]
struct Opts {
    from_file: Option<PathBuf>,
    format: Option<String>,
    redact: bool,
    strict: bool,
    json: bool,
    overwrite: bool,
    export_mode: Option<String>,
    names: Vec<String>,
    names_file: Option<PathBuf>,
    inherit: Vec<String>,
    unset: Vec<String>,
    allow_unknown: Vec<String>,
    allow_unset_mirror: Vec<String>,
    no_proxy_check: bool,
    proxy_timeout_ms: Option<u64>,
    argv: Vec<String>,
    saw_dashdash: bool,
}

fn strip_global(args: Vec<OsString>) -> (Vec<String>, Option<&'static str>) {
    let mut out = Vec::new();
    let mut global = None;
    let mut seen_subcommand = false;
    for a in args {
        let s = a.to_string_lossy().into_owned();
        if !seen_subcommand && (s == "--help" || s == "-h") {
            global = Some("help");
        } else if !seen_subcommand && (s == "--version" || s == "-V") {
            global = Some("version");
        } else {
            if !seen_subcommand && !s.starts_with('-') {
                seen_subcommand = true;
            }
            out.push(s);
        }
    }
    (out, global)
}

pub fn dispatch(args: impl Iterator<Item = OsString>) -> i32 {
    let (args, global) = strip_global(args.collect());
    if let Some(g) = global {
        return match g {
            "version" => {
                println!("envwrap {}", crate::tool_version());
                EXIT_OK
            }
            _ => {
                print!("{HELP}");
                EXIT_OK
            }
        };
    }
    let Some(sub) = args.first().cloned() else {
        eprintln!("envwrap: error: missing command");
        eprint!("{HELP}");
        return EXIT_USAGE;
    };
    let rest = &args[1..];
    let (ambient, skipped_non_utf8) = Env::from_process();

    let result = match sub.as_str() {
        "export" => cmd_export(rest, &ambient, skipped_non_utf8),
        "import" => cmd_import(rest),
        "check" => cmd_check(rest, &ambient),
        "run" => cmd_run(rest, &ambient),
        "wrap" => cmd_wrap(rest, &ambient),
        other => {
            return usage(&format!(
                "unknown command '{other}' (expected export, import, check, run or wrap)"
            ));
        }
    };
    match result {
        Ok(code) | Err(code) => code,
    }
}

struct Builder {
    opts: Opts,
}

impl Builder {
    fn value(
        &mut self,
        opt: &str,
        inline: Option<String>,
        it: &mut std::iter::Cloned<std::slice::Iter<'_, String>>,
    ) -> Result<String, String> {
        if let Some(v) = inline {
            return Ok(v);
        }
        it.next()
            .ok_or_else(|| format!("option '{opt}' requires a value"))
    }

    fn is_repeatable(opt: &str) -> bool {
        matches!(
            opt,
            "--inherit" | "--unset" | "--allow-unknown" | "--allow-unset-mirror"
        )
    }
}

fn parse_opts(sub: &str, tokens: &[String], allow_dashdash: bool) -> Result<Opts, String> {
    let mut b = Builder {
        opts: Opts::default(),
    };
    let mut seen: std::collections::BTreeSet<String> = Default::default();
    let mut it = tokens.iter().cloned();
    while let Some(tok) = it.next() {
        if tok == "--" {
            if !allow_dashdash {
                return Err(format!("unexpected '--' for '{sub}'"));
            }
            b.opts.saw_dashdash = true;
            b.opts.argv = it.by_ref().collect::<Vec<String>>();
            break;
        }
        if !tok.starts_with("--") || tok == "--" {
            return Err(format!("unexpected argument '{tok}' for '{sub}'"));
        }
        let (name, inline) = match tok.find('=') {
            Some(i) => (tok[..i].to_string(), Some(tok[i + 1..].to_string())),
            None => (tok.clone(), None),
        };
        let value_opt = matches!(
            name.as_str(),
            "--from-file"
                | "--format"
                | "--names"
                | "--names-file"
                | "--export-mode"
                | "--inherit"
                | "--unset"
                | "--allow-unknown"
                | "--allow-unset-mirror"
                | "--proxy-timeout-ms"
        );
        let flag_opt = matches!(
            name.as_str(),
            "--redact" | "--strict" | "--json" | "--overwrite" | "--no-proxy-check"
        );
        if !value_opt && !flag_opt {
            return Err(format!("unknown option '{name}' for '{sub}'"));
        }
        if inline.is_some() && !value_opt {
            return Err(format!("option '{name}' does not take a value"));
        }
        if value_opt && !Builder::is_repeatable(&name) && !seen.insert(name.clone()) {
            return Err(format!("duplicate option '{name}'"));
        }
        match name.as_str() {
            "--from-file" => {
                b.opts.from_file = Some(PathBuf::from(b.value(&name, inline, &mut it)?))
            }
            "--format" => b.opts.format = Some(b.value(&name, inline, &mut it)?),
            "--names" => {
                let v = b.value(&name, inline, &mut it)?;
                b.opts
                    .names
                    .extend(v.split(',').map(|s| s.trim().to_string()));
            }
            "--names-file" => {
                b.opts.names_file = Some(PathBuf::from(b.value(&name, inline, &mut it)?))
            }
            "--export-mode" => b.opts.export_mode = Some(b.value(&name, inline, &mut it)?),
            "--inherit" => {
                let v = b.value(&name, inline, &mut it)?;
                b.opts.inherit.push(v)
            }
            "--unset" => {
                let v = b.value(&name, inline, &mut it)?;
                b.opts.unset.push(v)
            }
            "--allow-unknown" => {
                let v = b.value(&name, inline, &mut it)?;
                b.opts.allow_unknown.push(v)
            }
            "--allow-unset-mirror" => {
                let v = b.value(&name, inline, &mut it)?;
                b.opts.allow_unset_mirror.push(v)
            }
            "--proxy-timeout-ms" => {
                let v = b.value(&name, inline, &mut it)?;
                b.opts.proxy_timeout_ms = Some(
                    v.parse::<u64>()
                        .map_err(|_| format!("invalid value '{v}' for '--proxy-timeout-ms'"))?,
                );
            }
            "--redact" => b.opts.redact = true,
            "--strict" => b.opts.strict = true,
            "--json" => b.opts.json = true,
            "--overwrite" => b.opts.overwrite = true,
            "--no-proxy-check" => b.opts.no_proxy_check = true,
            _ => unreachable!(),
        }
    }
    if allow_dashdash && !b.opts.saw_dashdash {
        return Err("missing '--' before command arguments".to_string());
    }
    Ok(b.opts)
}

/// Human-readable labels used in parse diagnostics.
fn label(path: Option<&Path>) -> String {
    match path {
        Some(p) => p.display().to_string(),
        None => "<stdin>".to_string(),
    }
}

fn decode(bytes: Vec<u8>, path: Option<&Path>) -> Result<String, i32> {
    match String::from_utf8(bytes) {
        Ok(s) => Ok(s),
        Err(e) => {
            let n = e.utf8_error().valid_up_to();
            Err(crate::util::die(
                crate::util::EXIT_PARSE_FAILED,
                &format!("{}: invalid UTF-8 at byte {n}", label(path)),
            ))
        }
    }
}

fn upgrade(findings: &mut [crate::parser::Finding], strict: bool) {
    if !strict {
        return;
    }
    for f in findings.iter_mut() {
        if f.severity == Severity::Warning {
            f.severity = Severity::Error;
        }
    }
}

fn parse_format(s: &str, default: FileFormat) -> Result<FileFormat, String> {
    match s {
        "auto" => Ok(default),
        "dotenv" => Ok(FileFormat::Dotenv),
        "env" => Ok(FileFormat::Env),
        "json" => Ok(FileFormat::Json),
        other => Err(format!("unknown format '{other}'")),
    }
}

fn plan_from(parsed: &ParsedFile, mode: ExportMode) -> Plan {
    Plan {
        entries: parsed.entries.clone(),
        unsets: parsed.unsets.clone(),
        export_mode: mode,
    }
}

// ---------------------------------------------------------------------------
// export
// ---------------------------------------------------------------------------

fn cmd_export(tokens: &[String], ambient: &Env, skipped_non_utf8: usize) -> Result<i32, i32> {
    let opts = parse_opts("export", tokens, false).map_err(|m| usage(&m))?;
    let format = match opts.format.as_deref() {
        None | Some("shell") => Format::Shell,
        Some("env") => Format::Env,
        Some("json") => Format::Json,
        Some(other) => return Err(usage(&format!("unknown format '{other}'"))),
    };
    let mode = match opts.export_mode.as_deref() {
        None | Some("full") => ExportMode::Full,
        Some("diff") => ExportMode::Diff,
        Some(other) => return Err(usage(&format!("unknown export mode '{other}'"))),
    };
    let mut names: Option<Vec<String>> = None;
    if !opts.names.is_empty() || opts.names_file.is_some() {
        let mut list = opts.names.clone();
        if let Some(f) = &opts.names_file {
            let bytes = read_input(Some(f))?;
            let text = decode(bytes, Some(f))?;
            for line in text.lines() {
                let l = line.trim();
                if !l.is_empty() {
                    list.push(l.to_string());
                }
            }
        }
        validate_names(&list).map_err(|m| usage(&m))?;
        names = Some(list);
    }

    let bytes = read_input(opts.from_file.as_deref())?;
    let text = decode(bytes, opts.from_file.as_deref())?;
    let mut parsed = parser::parse(&text, FileFormat::Dotenv, ambient);
    let lbl = label(opts.from_file.as_deref());
    upgrade(&mut parsed.findings, opts.strict);
    let code = report(&parsed.findings, &lbl);
    if code != EXIT_OK {
        return Err(code);
    }

    let plan = plan_from(&parsed, mode);
    let ropts = RenderOpts {
        format,
        redact: opts.redact,
    };
    let out = render::render_export(&plan, ambient, &ropts, names.as_deref(), skipped_non_utf8);
    print!("{out}");
    Ok(EXIT_OK)
}

// ---------------------------------------------------------------------------
// import
// ---------------------------------------------------------------------------

fn cmd_import(tokens: &[String]) -> Result<i32, i32> {
    let opts = parse_opts("import", tokens, false).map_err(|m| usage(&m))?;
    let ambient = Env::from_process().0;
    let bytes = read_input(opts.from_file.as_deref())?;
    let text = decode(bytes, opts.from_file.as_deref())?;
    let format = match opts.format.as_deref() {
        None | Some("auto") => parser::detect(&text),
        Some(other) => parse_format(other, FileFormat::Dotenv).map_err(|m| usage(&m))?,
    };
    let parsed = parser::parse(&text, format, &ambient);
    let lbl = label(opts.from_file.as_deref());
    let mut findings = parsed.findings.clone();
    upgrade(&mut findings, opts.strict);

    // Decide the outcome for every entry before mutating anything.
    let mut applied = 0usize;
    let mut already: Vec<String> = Vec::new();
    for e in &parsed.entries {
        if !opts.overwrite && std::env::var_os(&e.name).is_some() {
            already.push(e.name.clone());
        } else {
            applied += 1;
        }
    }
    let code = report(&findings, &lbl);
    if code != EXIT_OK {
        return Err(code);
    }
    for name in &already {
        eprintln!("envwrap: warning: {name} is already set; use --overwrite to replace");
    }

    // SAFETY: the CLI path is single-threaded and spawns no threads.
    for e in &parsed.entries {
        if already.iter().any(|n| n == &e.name) {
            continue;
        }
        unsafe { std::env::set_var(&e.name, &e.value) };
    }
    for u in &parsed.unsets {
        // SAFETY: see above.
        unsafe { std::env::remove_var(u) };
    }
    eprintln!(
        "envwrap: imported {applied} variable(s) into this process only; use `eval \"$(envwrap export)\"` to affect a shell"
    );
    Ok(EXIT_OK)
}

// ---------------------------------------------------------------------------
// check
// ---------------------------------------------------------------------------

fn cmd_check(tokens: &[String], ambient: &Env) -> Result<i32, i32> {
    let opts = parse_opts("check", tokens, false).map_err(|m| usage(&m))?;
    let bytes = read_input(opts.from_file.as_deref())?;
    let text = decode(bytes, opts.from_file.as_deref())?;
    let format = match opts.format.as_deref() {
        None | Some("auto") => parser::detect(&text),
        Some(other) => parse_format(other, FileFormat::Dotenv).map_err(|m| usage(&m))?,
    };
    let parsed = parser::parse(&text, format, ambient);
    let lbl = label(opts.from_file.as_deref());
    let mut findings = parsed.findings.clone();
    upgrade(&mut findings, opts.strict);
    let code = report(&findings, &lbl);
    if code != EXIT_OK {
        return Err(code);
    }

    let mut pr = PolicyReport::default();
    pr.counts.variables = parsed.entries.len();

    let get = |n: &str| {
        parsed
            .entries
            .iter()
            .rev()
            .find(|e| e.name == n)
            .map(|e| e.value.clone())
    };

    // Unknown-variable policing on the declared names.
    for e in &parsed.entries {
        if opts.allow_unknown.iter().any(|a| a == &e.name) {
            continue;
        }
        if classify(&e.name) == VarClass::Unknown {
            pr.push(
                "unknown_variable",
                Severity::Warning,
                e.name.clone(),
                format!(
                    "{} is not a known environment variable; add --allow-unknown {} if intended",
                    e.name, e.name
                ),
            );
        }
    }

    // Mirror checks over the file as written.
    for (lower, upper) in crate::compose::MIRRORS {
        let lo = get(lower).filter(|v| !v.is_empty());
        let up = get(upper).filter(|v| !v.is_empty());
        let allowed = opts
            .allow_unset_mirror
            .iter()
            .any(|a| a == lower || a == upper);
        match (lo, up) {
            (None, None) => {}
            (Some(_), None) | (None, Some(_)) => pr.counts.mirrors += 1,
            (Some(a), Some(b)) => {
                pr.counts.mirrors += 1;
                if allowed {
                    continue;
                }
                if trim_slash(&a) != trim_slash(&b) {
                    pr.push(
                        "mirror_conflict",
                        Severity::Warning,
                        *lower,
                        format!("{lower} and {upper} differ; tools may pick either"),
                    );
                }
            }
        }
        // An explicitly-set empty side next to a set side.
        let lo_raw = get(lower);
        let up_raw = get(upper);
        if allowed {
            continue;
        }
        if let (Some(a), Some(b)) = (&lo_raw, &up_raw)
            && a.is_empty()
            && !b.is_empty()
        {
            pr.push(
                "mirror_conflict_empty",
                Severity::Warning,
                *lower,
                format!("{lower} is set to the empty string while {upper} is set"),
            );
        }
    }

    // NO_PROXY semantics: a wildcard disables every configured proxy.
    let has_proxy = [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "http_proxy",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
    ]
    .iter()
    .any(|n| get(n).is_some_and(|v| !v.is_empty()));
    // Lowercase wins over uppercase, matching the reference checker.
    let no_proxy_raw = get("no_proxy")
        .or_else(|| get("NO_PROXY"))
        .filter(|v| !v.trim().is_empty());
    if let (Some(raw), true) = (&no_proxy_raw, has_proxy) {
        let rules = crate::proxy::parse_no_proxy(raw);
        if rules.contains(&crate::proxy::NoProxyRule::All) {
            pr.push(
                "no_proxy_bypasses_all",
                Severity::Warning,
                "NO_PROXY",
                "NO_PROXY is '*' while a proxy is configured; every request bypasses it",
            );
        }
        let kept = rules.len();
        let listed = raw
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .count();
        if listed > kept {
            pr.push(
                "no_proxy_invalid_rule",
                Severity::Warning,
                "NO_PROXY",
                format!(
                    "{} entr(ies) in NO_PROXY were not understood",
                    listed - kept
                ),
            );
        }
    }

    // Proxy preflight. Only the *effective* variable per scheme is probed:
    // lowercase wins over uppercase, and ALL_PROXY applies only where no
    // scheme-specific proxy exists. Probing shadowed variables would report
    // failures no client would ever hit.
    let timeout_ms = opts.proxy_timeout_ms.unwrap_or(300);
    if !opts.no_proxy_check {
        let pick = |lower: &str, upper: &str| -> Option<(String, String)> {
            match (
                get(lower).filter(|v| !v.trim().is_empty()),
                get(upper).filter(|v| !v.trim().is_empty()),
            ) {
                // A conflict is already reported by the mirror pass above.
                (Some(l), Some(_)) => Some((lower.to_string(), l)),
                (Some(l), None) => Some((lower.to_string(), l)),
                (None, Some(u)) => Some((upper.to_string(), u)),
                (None, None) => None,
            }
        };
        let http = pick("http_proxy", "HTTP_PROXY");
        let https = pick("https_proxy", "HTTPS_PROXY");
        let all = pick("all_proxy", "ALL_PROXY");
        // ALL_PROXY only takes effect when no scheme-specific proxy is set.
        let fallback = if http.is_none() && https.is_none() {
            all.clone()
        } else {
            None
        };

        let mut targets: Vec<(String, String)> = Vec::new();
        targets.extend(http);
        targets.extend(https);
        targets.extend(fallback);

        for (name, value) in targets {
            match crate::proxy::parse_proxy_url(&value) {
                Err(reason) => pr.push(
                    "bad_proxy_url",
                    Severity::Error,
                    name.clone(),
                    format!("invalid proxy URL: {reason}"),
                ),
                Ok(parts) => {
                    if parts.scheme == "socks4" || parts.scheme == "socks4a" {
                        // SOCKS4 needs a handshake; a TCP connect proves nothing.
                        pr.counts.proxy_checks += 1;
                        continue;
                    }
                    let port = parts
                        .port
                        .unwrap_or_else(|| crate::proxy::default_port(&parts.scheme));
                    let display = if opts.redact {
                        crate::proxy::redact_proxy(&value)
                    } else {
                        format!("{}:{port}", crate::proxy::display_host(&parts.host))
                    };
                    match probe(&parts.host, port, timeout_ms) {
                        Ok(()) => pr.counts.proxy_checks += 1,
                        Err(kind) => pr.push(
                            "unreachable_proxy",
                            Severity::Error,
                            name,
                            format!("cannot reach {display} within {timeout_ms}ms ({kind})"),
                        ),
                    }
                }
            }
        }
    }

    if opts.strict {
        for f in pr.findings.iter_mut() {
            if f.severity == Severity::Warning {
                f.severity = Severity::Error;
            }
        }
    }
    Ok(crate::output::policy(&pr, opts.json))
}

fn trim_slash(s: &str) -> &str {
    s.strip_suffix('/').unwrap_or(s)
}

/// TCP-connect to `host:port` under one overall deadline.
fn probe(host: &str, port: u16, timeout_ms: u64) -> Result<(), String> {
    use std::net::{TcpStream, ToSocketAddrs};
    let addrs = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("{:?}", e.kind()))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let mut last = "no addresses".to_string();
    for addr in addrs {
        let now = std::time::Instant::now();
        if now >= deadline {
            break;
        }
        match TcpStream::connect_timeout(&addr, deadline - now) {
            Ok(_) => return Ok(()),
            Err(e) => last = format!("{:?}", e.kind()),
        }
    }
    Err(last)
}

// ---------------------------------------------------------------------------
// run / wrap
// ---------------------------------------------------------------------------

fn run_common(sub: &str, tokens: &[String], ambient: &Env) -> Result<(Opts, Plan), i32> {
    let opts = parse_opts(sub, tokens, true).map_err(|m| usage(&m))?;
    if sub == "run" && !opts.allow_unknown.is_empty() {
        return Err(usage("unknown option '--allow-unknown' for 'run'"));
    }
    if sub == "run" && !opts.allow_unset_mirror.is_empty() {
        return Err(usage("unknown option '--allow-unset-mirror' for 'run'"));
    }
    let bytes = read_input(opts.from_file.as_deref())?;
    let text = decode(bytes, opts.from_file.as_deref())?;
    let format = match opts.format.as_deref() {
        None => FileFormat::Dotenv,
        Some(other) => parse_format(other, FileFormat::Dotenv).map_err(|m| usage(&m))?,
    };
    let parsed = parser::parse(&text, format, ambient);
    let lbl = label(opts.from_file.as_deref());
    let mut findings = parsed.findings.clone();
    upgrade(&mut findings, opts.strict);
    let code = report(&findings, &lbl);
    if code != EXIT_OK {
        return Err(code);
    }
    validate_names(&opts.inherit).map_err(|m| usage(&m))?;
    validate_names(&opts.unset).map_err(|m| usage(&m))?;
    let plan = plan_from(&parsed, ExportMode::Full);
    Ok((opts, plan))
}

/// `wrap`: start from the ambient snapshot, drop unsets, overlay declared
/// entries, re-apply `--inherit`, then sync mirrors.
fn assemble_wrap(opts: &Opts, plan: &Plan, ambient: &Env) -> Result<Env, i32> {
    for name in opts.inherit.iter() {
        if opts.unset.iter().any(|u| u == name) {
            return Err(usage(&format!("{name} is both inherited and unset")));
        }
    }
    let mut env = ambient.clone();
    for n in &plan.unsets {
        env.remove(n);
    }
    for e in &plan.entries {
        env.set(&e.name, e.value.clone());
    }
    for n in &opts.unset {
        env.remove(n);
    }
    for n in &opts.inherit {
        match ambient.get(n) {
            Some(v) => env.set(n, v.to_string()),
            None => eprintln!("envwrap: warning: '{n}' is not set in the ambient environment"),
        }
    }
    for n in &opts.unset {
        if plan.entries.iter().any(|e| &e.name == n) {
            eprintln!("envwrap: warning: unset_overrides_declared: {n}");
        }
    }
    crate::compose::sync_mirrors(&mut env);
    Ok(env)
}

fn assemble(opts: &Opts, plan: &Plan, ambient: &Env) -> Result<Env, i32> {
    let built = build(BuildInput {
        base: ambient,
        declared: plan,
        inherited: &opts.inherit,
        unset: &opts.unset,
        mode: SyncMode::Synced,
    })
    .map_err(|m| usage(&m))?;
    for s in &built.skipped_inherit {
        eprintln!("envwrap: warning: '{s}' is not set in the ambient environment");
    }
    for f in &built.findings {
        eprintln!("envwrap: warning: {}", f.message);
    }
    Ok(built.env)
}

fn expand_patterns(argv: &[String], env: &Env, strict: bool) -> Result<Vec<String>, i32> {
    let mut out = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    for a in argv {
        if crate::wrap::is_envwrap_token(a) {
            match std::env::current_exe() {
                Ok(p) => out.push(p.to_string_lossy().into_owned()),
                Err(e) => {
                    return Err(crate::util::die(
                        EXIT_USAGE,
                        &format!("cannot resolve current executable: {e}"),
                    ));
                }
            }
            continue;
        }
        let x = crate::wrap::expand(&crate::wrap::parse_pattern(a), env);
        out.push(x.value);
        for m in x.missing {
            if !missing.contains(&m) {
                missing.push(m);
            }
        }
    }
    if !missing.is_empty() {
        if strict {
            for m in &missing {
                eprintln!("envwrap: error: undefined variable '{m}' in pattern");
            }
            return Err(EXIT_USAGE);
        }
        for m in &missing {
            eprintln!("envwrap: warning: undefined variable '{m}' in pattern");
        }
    }
    Ok(out)
}

fn cmd_run(tokens: &[String], ambient: &Env) -> Result<i32, i32> {
    let (opts, plan) = run_common("run", tokens, ambient)?;
    let env = assemble(&opts, &plan, ambient)?;
    for e in &plan.entries {
        if classify(&e.name) == VarClass::Unknown {
            eprintln!(
                "envwrap: warning: {} is not a known environment variable; add --allow-unknown {} if intended",
                e.name, e.name
            );
        }
    }
    let cmd = exec::parse_command(&opts.argv).map_err(|m| usage(&m))?;
    if !cmd.program.contains('/') && env.get("PATH").is_none() {
        return Err(usage(
            "PATH is not set by the plan; use --inherit PATH or an absolute program path",
        ));
    }
    let expanded = expand_patterns(&opts.argv, &env, opts.strict)?;
    let cmd = exec::parse_command(&expanded).map_err(|m| usage(&m))?;
    match exec::spawn(&env, &cmd) {
        Ok(status) => Ok(exec::exit_code(&status)),
        Err(e) => Err(crate::util::die(e.code, &e.message)),
    }
}

fn cmd_wrap(tokens: &[String], ambient: &Env) -> Result<i32, i32> {
    let (opts, plan) = run_common("wrap", tokens, ambient)?;
    let env = assemble_wrap(&opts, &plan, ambient)?;

    let mut unknown: Vec<&str> = Vec::new();
    for e in &plan.entries {
        if opts.allow_unknown.iter().any(|a| a == &e.name) {
            continue;
        }
        if classify(&e.name) == VarClass::Unknown {
            unknown.push(&e.name);
        }
    }
    if !unknown.is_empty() {
        if opts.strict {
            for n in &unknown {
                eprintln!(
                    "envwrap: error: {n} is not a known environment variable; add --allow-unknown {n} if intended"
                );
            }
            return Err(crate::util::EXIT_PARSE_FAILED);
        }
        for n in &unknown {
            eprintln!(
                "envwrap: warning: {n} is not a known environment variable; add --allow-unknown {n} if intended"
            );
        }
    }

    let cmd = exec::parse_command(&opts.argv).map_err(|m| usage(&m))?;
    if !cmd.program.contains('/') && env.get("PATH").is_none() {
        return Err(usage(
            "PATH is not set by the plan; use --inherit PATH or an absolute program path",
        ));
    }
    let expanded = expand_patterns(&opts.argv, &env, opts.strict)?;
    let cmd = exec::parse_command(&expanded).map_err(|m| usage(&m))?;
    match exec::spawn(&env, &cmd) {
        Ok(status) => Ok(exec::exit_code(&status)),
        Err(e) => Err(crate::util::die(e.code, &e.message)),
    }
}
