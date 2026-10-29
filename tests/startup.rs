//! Exercise the real executable with an empty home and no desktop environment.
//! These tests run only on disposable CI runners, never against a live session.
#![cfg(target_os = "linux")]

use std::{
    fs,
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Output,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "hypr-rdp-startup-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::create_dir(path.join("home")).unwrap();
        Self(path)
    }

    fn identity(&self, expired: bool) -> (PathBuf, PathBuf) {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec!["localhost".into()]).unwrap();
        if expired {
            params.not_before = rcgen::date_time_ymd(2000, 1, 1);
            params.not_after = rcgen::date_time_ymd(2001, 1, 1);
        }
        let cert = params.self_signed(&key).unwrap();
        let cert_path = self.0.join("cert.pem");
        let key_path = self.0.join("key.pem");
        fs::write(&cert_path, cert.pem()).unwrap();
        fs::write(&key_path, key.serialize_pem()).unwrap();
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600)).unwrap();
        (cert_path, key_path)
    }

    fn config(&self, content: &str) -> PathBuf {
        let path = self.0.join("config.toml");
        fs::write(&path, content).unwrap();
        path
    }

    async fn run(&self, args: &[&str]) -> Output {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_hypr-rdp"));
        command
            .env_clear()
            .env("HOME", self.0.join("home"))
            .env("XDG_RUNTIME_DIR", self.0.join("absent-runtime"))
            .env("WAYLAND_DISPLAY", "absent-wayland")
            .env("SSLKEYLOGFILE", self.0.join("tls-secrets.log"))
            .args(args)
            .kill_on_drop(true);
        tokio::time::timeout(Duration::from_secs(5), command.output())
            .await
            .expect("startup must be bounded without a desktop")
            .unwrap()
    }

    fn no_home_writes(&self) {
        assert_eq!(fs::read_dir(self.0.join("home")).unwrap().count(), 0);
        assert!(!self.0.join("tls-secrets.log").exists());
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn path(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn failure(output: &Output, expected: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "unexpected success: {stderr}");
    assert!(stderr.contains(expected), "missing {expected:?}: {stderr}");
    assert!(
        !stderr.contains("failed to select the Hyprland instance"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("failed to initialize display capture"),
        "{stderr}"
    );
}

#[tokio::test]
async fn native_config_check_validates_without_runtime_directory_or_driver() {
    let fixture = Fixture::new();
    let (cert, key) = fixture.identity(false);
    let output = fixture
        .run(&[
            "--check-config",
            "--auth-mode",
            "none",
            "--cert",
            path(&cert),
            "--key",
            path(&key),
            "--h264-backend",
            "nvenc",
            "--nvenc-device",
            "GPU-01234567-89ab-cdef-0123-456789abcdef",
        ])
        .await;
    if cfg!(feature = "nvenc") {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    } else {
        failure(&output, "built without the nvenc feature");
    }
    assert!(!fixture.0.join("absent-runtime").exists());
    fixture.no_home_writes();
}

#[tokio::test]
async fn native_config_errors_precede_tls_and_desktop_without_writes() {
    let fixture = Fixture::new();
    for extra in [
        vec!["--h264-backend", "nvenc"],
        vec!["--h264-backend", "nvenc", "--nvenc-device", "0"],
    ] {
        let mut args = vec!["--check-config", "--auth-mode", "none"];
        args.extend(extra);
        let output = fixture.run(&args).await;
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("TLS certificate") && !stderr.contains("Hyprland"),
            "{stderr}"
        );
        fixture.no_home_writes();
    }
}

#[tokio::test]
async fn check_config_uses_existing_identity_without_desktop_listener_or_hooks() {
    let fixture = Fixture::new();
    let (cert, key) = fixture.identity(false);
    let original_cert = fs::read(&cert).unwrap();
    let original_key = fs::read(&key).unwrap();
    let password = fixture.0.join("password");
    fs::write(&password, "canary-password\n").unwrap();
    fs::set_permissions(&password, fs::Permissions::from_mode(0o600)).unwrap();
    // The configured port is already occupied; validation must not bind it.
    let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
    let config = fixture.config(&format!(
        "bind = {:?}\nusername = 'user'\npassword_file = {:?}\ncert = {:?}\nkey = {:?}\non_session_start = 'exit 99'\non_session_end = 'exit 99'\n",
        occupied.local_addr().unwrap().to_string(), path(&password), path(&cert), path(&key)
    ));
    let output = fixture
        .run(&["--check-config", "--config", path(&config)])
        .await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(cert).unwrap(), original_cert);
    assert_eq!(fs::read(key).unwrap(), original_key);
    fixture.no_home_writes();
}

#[tokio::test]
async fn missing_or_incomplete_authentication_fails_before_tls_generation() {
    let fixture = Fixture::new();
    for args in [
        vec![],
        vec!["--username", "user"],
        vec!["--password", "canary-password"],
    ] {
        let output = fixture.run(&args).await;
        failure(&output, "requires a nonempty username and password");
        assert!(!String::from_utf8_lossy(&output.stderr).contains("canary-password"));
        fixture.no_home_writes();
    }
}

#[tokio::test]
async fn anonymous_mode_is_deliberate_and_cannot_hide_conflicting_credentials() {
    let fixture = Fixture::new();
    let (cert, key) = fixture.identity(false);
    let output = fixture
        .run(&[
            "--check-config",
            "--auth-mode",
            "none",
            "--cert",
            path(&cert),
            "--key",
            path(&key),
        ])
        .await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = fixture
        .run(&[
            "--check-config",
            "--auth-mode",
            "none",
            "--username",
            "user",
        ])
        .await;
    failure(&output, "auth-mode none");
    fixture.no_home_writes();
}

#[tokio::test]
async fn check_config_never_generates_missing_default_tls_files() {
    let fixture = Fixture::new();
    let output = fixture
        .run(&[
            "--check-config",
            "--username",
            "user",
            "--password",
            "canary-password",
        ])
        .await;
    failure(&output, "failed to open TLS certificate");
    fixture.no_home_writes();
}

#[tokio::test]
async fn normal_startup_rejects_broken_tls_before_desktop_discovery() {
    let fixture = Fixture::new();
    let (cert, key) = fixture.identity(true);
    let output = fixture
        .run(&[
            "-u",
            "user",
            "-p",
            "canary-password",
            "--cert",
            path(&cert),
            "--key",
            path(&key),
        ])
        .await;
    failure(&output, "has expired");
    fs::write(&key, "CANARY-PRIVATE-KEY").unwrap();
    let output = fixture
        .run(&[
            "-u",
            "user",
            "-p",
            "canary-password",
            "--cert",
            path(&cert),
            "--key",
            path(&key),
        ])
        .await;
    failure(&output, "private key");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("CANARY-PRIVATE-KEY"));
    let output = fixture
        .run(&["-u", "user", "-p", "canary-password", "--cert", path(&cert)])
        .await;
    failure(&output, "--cert provided without --key");
    fixture.no_home_writes();
}

#[tokio::test]
async fn config_diagnostics_reject_typos_without_printing_secret_source_lines() {
    let fixture = Fixture::new();
    for content in [
        "pasword = 'CANARY-SECRET'",
        "password = 'CANARY-SECRET' invalid",
        "password = ['CANARY-SECRET']",
    ] {
        let config = fixture.config(content);
        let output = fixture
            .run(&["--check-config", "--config", path(&config)])
            .await;
        failure(&output, "failed to parse config");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("line 1"), "{stderr}");
        assert!(!stderr.contains("CANARY-SECRET"), "{stderr}");
        fixture.no_home_writes();
    }
}

#[tokio::test]
async fn password_file_rejects_fifo_and_devices_before_tls_or_desktop() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let fixture = Fixture::new();
    let fifo = fixture.0.join("password-fifo");
    let name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: name is NUL-terminated and lives through this call. The FIFO is
    // confined to the test's directory and has no writer.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    // The FIFO is first so unfixed code times out without ever reading the
    // unbounded /dev/zero stream. Fixture::run kills a timed-out child.
    for password in [fifo.as_path(), Path::new("/dev/zero"), fixture.0.as_path()] {
        let output = fixture
            .run(&[
                "--check-config",
                "--username",
                "user",
                "--password-file",
                path(password),
            ])
            .await;
        failure(&output, "must be a regular file");
        fixture.no_home_writes();
    }
}
