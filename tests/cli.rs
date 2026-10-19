//! CLI failures must not launch an unprotected program or expose profile secrets.
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_envwrap");
struct Session(tempfile::TempDir);
impl Session {
    fn new() -> Self {
        Self(tempfile::tempdir().unwrap())
    }
    fn command(&self) -> Command {
        let mut command = Command::new(BIN);
        command
            .env_clear()
            .env("PATH", "/usr/bin:/usr/sbin:/bin")
            .env("XDG_CONFIG_HOME", self.0.path());
        command
    }
    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }
    fn create(&self, name: &str) -> Output {
        self.run(&[
            "profile",
            "create",
            name,
            "--proxy-env",
            "TEST_PROXY",
            "--dns",
            "192.0.2.53:53",
            "--timezone",
            "Etc/UTC",
            "--locale",
            "C.UTF-8",
        ])
    }
    fn path(&self, name: &str) -> std::path::PathBuf {
        self.0
            .path()
            .join("envwrap/profiles")
            .join(format!("{name}.toml"))
    }
    fn read(&self, name: &str) -> toml::Value {
        toml::from_str(&std::fs::read_to_string(self.path(name)).unwrap()).unwrap()
    }
    fn set(&self, name: &str, value: &toml::Value) {
        std::fs::write(self.path(name), toml::to_string(value).unwrap()).unwrap();
    }
}
fn success(output: Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn missing_profile_does_not_create_or_launch() {
    let session = Session::new();
    let marker = session.0.path().join("executed");
    let output = session
        .command()
        .args(["run", "--", "/usr/bin/touch"])
        .arg(&marker)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!marker.exists());
    assert!(!session.0.path().join("envwrap").exists());
}

#[test]
fn explicit_creation_is_private_stable_and_non_overwriting() {
    let session = Session::new();
    success(session.create("first"));
    let before = std::fs::read(session.path("first")).unwrap();
    assert_eq!(
        std::fs::metadata(session.path("first"))
            .unwrap()
            .permissions()
            .mode()
            & 0o077,
        0
    );
    assert!(!session.create("first").status.success());
    assert_eq!(std::fs::read(session.path("first")).unwrap(), before);
    success(session.create("second"));
    let first = session.read("first");
    let second = session.read("second");
    for field in ["hostname", "machine_id", "product_uuid", "mac", "seed"] {
        assert_ne!(
            first["identity"][field], second["identity"][field],
            "cross-profile identity: {field}"
        );
    }
    success(session.run(&["profile", "delete", "first"]));
    assert!(!session.path("first").exists());
    assert!(session.path("second").exists());
}

#[test]
fn invalid_required_configuration_never_executes() {
    let session = Session::new();
    success(session.create("default"));
    let original = session.read("default");
    let marker = session.0.path().join("executed");
    let mut cases = Vec::new();
    for (field, value) in [
        ("timezone", "NoSuchZone/Unavailable"),
        ("locale", "not_installed.UTF-8"),
        ("hostname", "bad\nhost"),
        ("machine_id", "00000000000000000000000000000000"),
    ] {
        let mut config = original.clone();
        config["identity"][field] = value.into();
        cases.push(config);
    }
    let mut old = original.clone();
    old["version"] = 1.into();
    cases.push(old);
    let mut missing = original.clone();
    missing["network"].as_table_mut().unwrap().remove("dns");
    cases.push(missing);
    let mut default_route = original.clone();
    default_route["network"]["direct"] = toml::Value::Array(vec!["0.0.0.0/0".into()]);
    cases.push(default_route);
    for config in cases {
        session.set("default", &config);
        let output = session
            .command()
            .args(["run", "--", "/usr/bin/touch"])
            .arg(&marker)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!marker.exists());
    }
}

#[test]
fn show_and_parse_errors_do_not_expose_proxy_credentials_or_seed() {
    let session = Session::new();
    success(session.create("default"));
    let mut config = session.read("default");
    let seed = config["identity"]["seed"].as_str().unwrap().to_owned();
    let proxy = config["network"]["proxy"].as_table_mut().unwrap();
    proxy.remove("env");
    proxy.insert(
        "url".into(),
        "http://private-user:private-password@127.0.0.1:8123".into(),
    );
    session.set("default", &config);
    let output = session.run(&["profile", "show", "default"]);
    assert!(output.status.success());
    for bytes in [&output.stdout, &output.stderr] {
        let text = String::from_utf8_lossy(bytes);
        assert!(
            !text.contains("private-user")
                && !text.contains("private-password")
                && !text.contains(&seed)
        );
    }
    std::fs::write(
        session.path("default"),
        "version = 2\nname = \"private-password\n",
    )
    .unwrap();
    let output = session.run(&["doctor"]);
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("private-password"));
}

#[test]
fn insecure_profile_and_missing_explicit_proxy_do_not_launch() {
    let session = Session::new();
    success(session.create("default"));
    let marker = session.0.path().join("executed");
    let output = session
        .command()
        .env("HTTP_PROXY", "http://private-password@127.0.0.1:9")
        .args(["run", "--", "/usr/bin/touch"])
        .arg(&marker)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!marker.exists());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("private-password"));
    std::fs::set_permissions(
        session.path("default"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let output = session
        .command()
        .env("TEST_PROXY", "http://127.0.0.1:9")
        .args(["run", "--", "/usr/bin/touch"])
        .arg(&marker)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!marker.exists());
}

#[test]
fn misplaced_credential_url_is_not_repeated_in_usage_errors() {
    let session = Session::new();
    let output = session.run(&[
        "profile",
        "create",
        "default",
        "http://sample-user:sample-password@127.0.0.1:9",
    ]);
    assert!(!output.status.success());
    assert!(!session.path("default").exists());
    for bytes in [&output.stdout, &output.stderr] {
        let text = String::from_utf8_lossy(bytes);
        assert!(!text.contains("sample-user") && !text.contains("sample-password"));
    }
}
