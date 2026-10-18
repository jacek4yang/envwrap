//! End-to-end CLI tests. Every child process runs with a cleared environment
//! plus only what the test sets, so the host environment cannot leak in.

use std::process::{Command, Output, Stdio};
use std::time::Instant;

const BIN: &str = env!("CARGO_BIN_EXE_envwrap");
const FIX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn bin() -> Command {
    let mut c = Command::new(BIN);
    c.env_clear().env("PATH", "/usr/bin:/bin");
    c
}

fn run(mut cmd: Command, stdin: Option<&str>) -> Output {
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn envwrap");
    if let Some(s) = stdin {
        use std::io::Write;
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(s.as_bytes())
            .unwrap();
    }
    drop(child.stdin.take());
    child.wait_with_output().expect("wait")
}

fn fixture(name: &str) -> String {
    format!("{FIX}/{name}")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn export_plain_and_redacted() {
    let mut c = bin();
    c.args(["export", "--from-file", &fixture("secrets.env")]);
    let o = run(c, None);
    assert_eq!(stdout(&o), "API_TOKEN=abc\nPLAIN='hello world'\n");
    assert_eq!(o.status.code(), Some(0));

    let mut c = bin();
    c.args(["export", "--from-file", &fixture("secrets.env"), "--redact"]);
    let o = run(c, None);
    assert_eq!(stdout(&o), "API_TOKEN='<redacted>'\nPLAIN='hello world'\n");
    assert!(!stdout(&o).contains("abc"));
    assert!(!stderr(&o).contains("abc"));
}

#[test]
fn export_json_shape() {
    let mut c = bin();
    c.args([
        "export",
        "--format",
        "json",
        "--from-file",
        &fixture("secrets.env"),
    ]);
    let o = run(c, None);
    let s = stdout(&o);
    assert_eq!(o.status.code(), Some(0));
    // Minimal structural checks without a JSON dependency.
    assert!(s.contains("\"variables\":{\"API_TOKEN\":\"abc\",\"PLAIN\":\"hello world\"}"));
    assert!(s.contains("\"redacted\":false"));
    assert!(s.contains("\"schema_version\":\"1.0\""));
    let at = s
        .split("\"generated_at\":\"")
        .nth(1)
        .and_then(|r| r.split('"').next())
        .expect("generated_at present");
    let bytes: Vec<char> = at.chars().collect();
    assert_eq!(bytes.len(), 20, "{at}");
    assert_eq!(bytes[4], '-');
    assert_eq!(bytes[7], '-');
    assert_eq!(bytes[10], 'T');
    assert_eq!(bytes[13], ':');
    assert_eq!(bytes[16], ':');
    assert_eq!(bytes[19], 'Z');
    assert!(s.ends_with("]}\n"));
}

#[test]
fn export_diff_mode() {
    let mut c = bin();
    c.env("SAME", "1").args([
        "export",
        "--export-mode",
        "diff",
        "--from-file",
        &fixture("layers.env"),
    ]);
    // layers.env sets FOO=bar and TZ=UTC; neither is in this env, so both emit.
    let o = run(c, None);
    assert_eq!(stdout(&o), "FOO=bar\nTZ=UTC\n");

    let mut c = bin();
    c.env("FOO", "bar").args([
        "export",
        "--export-mode",
        "diff",
        "--from-file",
        &fixture("layers.env"),
    ]);
    let o = run(c, None);
    assert_eq!(stdout(&o), "TZ=UTC\n");
}

#[test]
fn check_mirror_conflict_and_ok() {
    let mut c = bin();
    c.args([
        "check",
        "--from-file",
        &fixture("mirror-conflict.env"),
        "--no-proxy-check",
    ]);
    let o = run(c, None);
    // A mirror conflict is a Warning: reported, but exit stays 0.
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(stderr(&o).contains("mirror_conflict"));
    assert!(stderr(&o).contains("differ"));

    let mut c = bin();
    c.args([
        "check",
        "--from-file",
        &fixture("mirror-conflict.env"),
        "--no-proxy-check",
        "--strict",
    ]);
    let o = run(c, None);
    assert_eq!(o.status.code(), Some(3));

    let mut c = bin();
    c.args([
        "check",
        "--from-file",
        &fixture("mirror-ok.env"),
        "--no-proxy-check",
    ]);
    let o = run(c, None);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(stdout(&o).starts_with("ok: "));
}

#[test]
fn check_unreachable_proxy_is_fast() {
    let start = Instant::now();
    let mut c = bin();
    c.args([
        "check",
        "--from-file",
        &fixture("unreachable.env"),
        "--proxy-timeout-ms",
        "50",
    ]);
    let o = run(c, None);
    let elapsed = start.elapsed();
    assert_eq!(o.status.code(), Some(3));
    assert!(stderr(&o).contains("unreachable_proxy"));
    assert!(elapsed.as_secs() < 3, "took {elapsed:?}");
}

#[test]
fn run_inherits_path() {
    let mut c = bin();
    c.args([
        "run",
        "--from-file",
        &fixture("layers.env"),
        "--inherit",
        "PATH",
        "--",
        "/bin/sh",
        "-c",
        "printf %s \"$FOO\"",
    ]);
    let o = run(c, None);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o), "bar");
}

#[test]
fn run_without_path_fails() {
    let mut c = bin();
    c.args([
        "run",
        "--from-file",
        &fixture("layers.env"),
        "--",
        "/bin/sh",
        "-c",
        "true",
    ]);
    let o = run(c, None);
    // /bin/sh is an absolute path, so it runs; the PATH guard applies to bare names.
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));

    let mut c = bin();
    c.args([
        "run",
        "--from-file",
        &fixture("layers.env"),
        "--",
        "sh",
        "-c",
        "true",
    ]);
    let o = run(c, None);
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("PATH is not set by the plan"));
}

#[test]
fn wrap_unset_beats_ambient() {
    let mut c = bin();
    c.env("FOO", "ambient").args([
        "wrap",
        "--from-file",
        &fixture("layers.env"),
        "--allow-unknown",
        "FOO",
        "--unset",
        "FOO",
        "--",
        "/bin/sh",
        "-c",
        "printf %s \"${FOO-unset}\"",
    ]);
    let o = run(c, None);
    assert_eq!(stdout(&o), "unset", "{}", stderr(&o));
    assert_eq!(o.status.code(), Some(0));
}

#[test]
fn wrap_allow_unknown_and_strict() {
    let mut c = bin();
    c.args([
        "wrap",
        "--from-file",
        &fixture("layers.env"),
        "--allow-unknown",
        "FOO",
        "--",
        "/bin/sh",
        "-c",
        "printf %s \"$FOO\"",
    ]);
    let o = run(c, None);
    assert_eq!(stdout(&o), "bar");
    assert_eq!(o.status.code(), Some(0));

    let mut c = bin();
    c.args([
        "wrap",
        "--from-file",
        &fixture("layers.env"),
        "--",
        "/bin/sh",
        "-c",
        "true",
    ]);
    let o = run(c, None);
    assert_eq!(o.status.code(), Some(0));
    assert!(
        stderr(&o).contains("unknown_variable")
            || stderr(&o).contains("not a known environment variable")
    );

    let mut c = bin();
    c.args([
        "wrap",
        "--from-file",
        &fixture("layers.env"),
        "--strict",
        "--",
        "/bin/sh",
        "-c",
        "true",
    ]);
    let o = run(c, None);
    assert_eq!(o.status.code(), Some(4));
}

#[test]
fn wrap_exit_codes() {
    let mut c = bin();
    c.args(["wrap", "--", "/bin/sh", "-c", "exit 7"]);
    let o = run(c, None);
    assert_eq!(o.status.code(), Some(7));

    let mut c = bin();
    c.args(["wrap", "--", "/bin/sh", "-c", "kill -TERM $${$$}"]);
    let o = run(c, None);
    assert_eq!(o.status.code(), Some(143), "{}", stderr(&o));
}

#[test]
fn import_reports_count_and_already_set() {
    let mut c = bin();
    c.args(["import", "--from-file", &fixture("plain.env")]);
    let o = run(c, None);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(stderr(&o).contains("imported 1 variable"));

    let mut c = bin();
    c.env("IMPORTED_PROBE", "old")
        .args(["import", "--from-file", &fixture("plain.env")]);
    let o = run(c, None);
    assert_eq!(o.status.code(), Some(0));
    assert!(stderr(&o).contains("already set"));
}

#[test]
fn version_help_and_unknown_command() {
    let mut c = bin();
    c.arg("--version");
    let o = run(c, None);
    assert_eq!(stdout(&o), "envwrap 0.1.0\n");
    assert_eq!(o.status.code(), Some(0));

    let mut c = bin();
    c.arg("--help");
    let o = run(c, None);
    let s = stdout(&o);
    for sub in ["export", "import", "check", "run", "wrap"] {
        assert!(s.contains(sub), "help missing {sub}");
    }

    let mut c = bin();
    c.arg("bogus");
    let o = run(c, None);
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn names_filter() {
    let mut c = bin();
    c.args([
        "export",
        "--names",
        "API_TOKEN",
        "--from-file",
        &fixture("secrets.env"),
    ]);
    let o = run(c, None);
    assert_eq!(stdout(&o), "API_TOKEN=abc\n");

    let mut c = bin();
    c.args([
        "export",
        "--names",
        "NOPE",
        "--from-file",
        &fixture("secrets.env"),
    ]);
    let o = run(c, None);
    assert_eq!(o.status.code(), Some(0));
    assert!(stderr(&o).contains("not defined"));
}

#[test]
fn wrap_unset_beats_ambient_and_declared() {
    let mut c = bin();
    c.env("FOO", "ambient").args([
        "wrap",
        "--from-file",
        &fixture("layers.env"),
        "--allow-unknown",
        "FOO",
        "--unset",
        "FOO",
        "--",
        "/bin/sh",
        "-c",
        "printf %s \"${FOO-unset}\"",
    ]);
    let o = run(c, None);
    assert_eq!(stdout(&o), "unset");
}

#[test]
fn check_probes_only_the_effective_proxy() {
    // http_proxy shadows HTTP_PROXY, so only 127.0.0.1:1 is probed.
    let plan = "http_proxy=http://127.0.0.1:1\nHTTP_PROXY=https://proxy.example:3128\n";
    let mut c = bin();
    c.args(["check", "--proxy-timeout-ms", "50"]);
    let o = run(c, Some(plan));
    let err = stderr(&o);
    assert!(err.contains("unreachable_proxy"));
    assert!(err.contains("http_proxy:"), "{err}");
    assert!(!err.contains("proxy.example"), "{err}");
    assert!(err.contains("mirror_conflict"));
}

#[test]
fn check_all_proxy_only_when_no_scheme_specific() {
    let mut c = bin();
    c.args(["check", "--proxy-timeout-ms", "50"]);
    let o = run(c, Some("ALL_PROXY=http://127.0.0.1:1\n"));
    assert_eq!(o.status.code(), Some(3));
    assert!(stderr(&o).contains("ALL_PROXY"));

    let mut c = bin();
    c.args(["check", "--proxy-timeout-ms", "50"]);
    let o = run(
        c,
        Some("ALL_PROXY=http://127.0.0.1:1\nHTTPS_PROXY=http://127.0.0.1:2\n"),
    );
    // Only the scheme-specific one is probed.
    assert_eq!(o.status.code(), Some(3));
    assert!(stderr(&o).contains("HTTPS_PROXY"));
    assert!(!stderr(&o).contains("ALL_PROXY:"));
}

#[test]
fn check_no_proxy_wildcard_warns() {
    let mut c = bin();
    c.args(["check", "--proxy-timeout-ms", "50"]);
    let o = run(c, Some("HTTPS_PROXY=http://127.0.0.1:1\nNO_PROXY=*\n"));
    assert!(
        stderr(&o).contains("no_proxy_bypasses_all"),
        "{}",
        stderr(&o)
    );
}

#[test]
fn check_invalid_port_is_bad_proxy_url() {
    let mut c = bin();
    c.args(["check", "--proxy-timeout-ms", "50"]);
    let o = run(c, Some("HTTPS_PROXY=http://h:\n"));
    assert_eq!(o.status.code(), Some(3));
    assert!(stderr(&o).contains("bad_proxy_url"));
}

#[test]
fn export_redacts_empty_userinfo() {
    let mut c = bin();
    c.args(["export", "--redact"]);
    let o = run(c, Some("HTTPS_PROXY=https://@h:1\n"));
    assert_eq!(stdout(&o), "HTTPS_PROXY='https://<redacted>@h:1'\n");
}

#[test]
fn export_redacts_ipv6_proxy() {
    let mut c = bin();
    c.args(["export", "--redact"]);
    let o = run(c, Some("https_proxy=http://u:p@[::1]:3128/x?a=1\n"));
    assert_eq!(
        stdout(&o),
        "https_proxy='http://<redacted>@[::1]:3128'\n",
        "{}",
        stderr(&o)
    );
}
