//! End-to-end Linux networking in two rootless namespaces: no public egress.
use std::process::Command;

fn network(scheme: &str) {
    let root = env!("CARGO_MANIFEST_DIR");
    let output = Command::new("bwrap")
        .env_clear()
        .env("PATH", "/usr/bin:/usr/sbin:/bin")
        .args([
            "--bind",
            "/",
            "/",
            "--dev-bind",
            "/dev",
            "/dev",
            "--proc",
            "/proc",
            "--unshare-user",
            "--unshare-net",
            "--cap-add",
            "CAP_NET_ADMIN",
            "--cap-add",
            "CAP_NET_RAW",
            "--die-with-parent",
            "--",
            "/bin/sh",
        ])
        .arg(format!("{root}/tests/fixtures/network_namespace.sh"))
        .arg(format!("{root}/tests/fixtures/network.py"))
        .arg(env!("CARGO_BIN_EXE_envwrap"))
        .arg(scheme)
        .output()
        .expect(
            "start rootless network acceptance (requires bwrap, ip, nft, setpriv and Python 3.11+)",
        );
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    print!("{}", String::from_utf8_lossy(&output.stdout));
}
#[test]
fn http_transparent_network_and_original_environment() {
    network("http");
}
#[test]
fn socks5_transparent_network_and_original_environment() {
    network("socks5");
}

#[test]
fn socks5h_domain_resolution_and_session_routing() {
    network("socks5h");
}
