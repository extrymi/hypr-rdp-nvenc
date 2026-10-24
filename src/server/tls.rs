use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::{fd::AsRawFd, unix::fs::OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{atomic::{AtomicU64, Ordering}, Arc};
use std::time::SystemTime;

use anyhow::{Context, Result};
use rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
use tokio_rustls::TlsAcceptor;
use x509_cert::der::Decode;

const MAX_TLS_FILE_BYTES: u64 = 1024 * 1024;
static NEXT_STAGING: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy)]
pub(super) enum Provisioning {
    ExistingOnly,
    CreateIfAbsent,
}

pub(super) struct PreparedTls {
    pub acceptor: TlsAcceptor,
    pub pub_key: Vec<u8>,
}

/// Prepare the same TLS object later handed to the server, before desktop I/O.
/// ExistingOnly is used by --check-config and never generates an identity.
pub(super) fn prepare(cert: Option<&str>, key: Option<&str>, provisioning: Provisioning) -> Result<PreparedTls> {
    let (cert, key) = resolve_tls_paths(cert, key, provisioning)?;
    load_identity_at(Path::new(&cert), Path::new(&key), SystemTime::now())
}

fn resolve_tls_paths(cert: Option<&str>, key: Option<&str>, provisioning: Provisioning) -> Result<(String, String)> {
    match (cert, key) {
        (Some(c), Some(k)) => Ok((c.to_string(), k.to_string())),
        (Some(_), None) => anyhow::bail!("--cert provided without --key"),
        (None, Some(_)) => anyhow::bail!("--key provided without --cert"),
        (None, None) => {
            let home = std::env::var("HOME").context("HOME not set")?;
            let dir = PathBuf::from(home).join(".config").join("hypr-rdp");
            let (cert, key) = match provisioning {
                Provisioning::ExistingOnly => (dir.join("cert.pem"), dir.join("key.pem")),
                Provisioning::CreateIfAbsent => auto_generate_tls_in(&dir)?,
            };
            Ok((cert.to_string_lossy().into_owned(), key.to_string_lossy().into_owned()))
        }
    }
}

/// Read through one descriptor, checking that descriptor's metadata. NONBLOCK
/// allows rejecting a FIFO/device instead of hanging while opening it.
fn read_pem(path: &Path, private: bool) -> Result<Vec<u8>> {
    let file = OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC).open(path)
        .with_context(|| format!("failed to open TLS {} {}", if private { "key" } else { "certificate" }, path.display()))?;
    let metadata = file.metadata().context("failed to inspect TLS file")?;
    anyhow::ensure!(metadata.is_file(), "TLS file {} must be a regular file", path.display());
    if private {
        use std::os::unix::fs::PermissionsExt;
        anyhow::ensure!(metadata.permissions().mode() & 0o077 == 0,
            "TLS private key {} must not be accessible by group or others (use mode 0600 or 0400)", path.display());
    }
    let mut bytes = Vec::new();
    file.take(MAX_TLS_FILE_BYTES + 1).read_to_end(&mut bytes).context("failed to read TLS file")?;
    anyhow::ensure!(!bytes.is_empty() && bytes.len() as u64 <= MAX_TLS_FILE_BYTES,
        "TLS file {} is empty or exceeds 1 MiB", path.display());
    Ok(bytes)
}

fn load_identity_at(cert_path: &Path, key_path: &Path, now: SystemTime) -> Result<PreparedTls> {
    let cert_bytes = read_pem(cert_path, false)?;
    let key_bytes = read_pem(key_path, true)?;
    // Never attach parser errors that might echo input/key material.
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(&cert_bytes)
        .map(|item| item.map(CertificateDer::into_owned).map_err(|_| anyhow::anyhow!("invalid TLS certificate PEM")))
        .collect::<Result<_>>()?;
    anyhow::ensure!(!certs.is_empty(), "TLS certificate PEM contains no certificates");
    let mut keys = PrivateKeyDer::pem_slice_iter(&key_bytes)
        .map(|item| item.map_err(|_| anyhow::anyhow!("invalid TLS private key PEM")));
    let key = keys.next().context("TLS key PEM contains no private key")??;
    anyhow::ensure!(keys.next().is_none(), "TLS key PEM must contain exactly one private key");
    let mut pub_key = None;
    for (index, der) in certs.iter().enumerate() {
        let cert = x509_cert::Certificate::from_der(der.as_ref())
            .map_err(|_| anyhow::anyhow!("invalid X.509 certificate at chain position {index}"))?;
        let validity = cert.tbs_certificate().validity();
        anyhow::ensure!(now >= validity.not_before.to_system_time(), "TLS certificate at chain position {index} is not yet valid");
        anyhow::ensure!(now <= validity.not_after.to_system_time(), "TLS certificate at chain position {index} has expired");
        if index == 0 {
            // CredSSP uses the leaf's public-key BIT STRING, matching the
            // pinned IronRDP helper's wire contract (not the full SPKI DER).
            pub_key = Some(cert.tbs_certificate().subject_public_key_info().subject_public_key
                .as_bytes().context("TLS public key is not byte-aligned")?.to_vec());
        }
    }
    let config = rustls::ServerConfig::builder().with_no_client_auth()
        .with_single_cert(certs, key)
        .context("TLS certificate and private key do not match or are unsupported")?;
    // Keep rustls' default NoKeyLog. Do not opt into SSLKEYLOGFILE as the
    // dependency helper does. Client authentication remains PAM/CredSSP.
    Ok(PreparedTls { acceptor: TlsAcceptor::from(Arc::new(config)), pub_key: pub_key.context("TLS leaf certificate missing")? })
}

fn entry_exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("failed to inspect TLS path {}", path.display())),
    }
}

fn reuse_pair(cert: &Path, key: &Path) -> Result<bool> {
    match (entry_exists(cert)?, entry_exists(key)?) {
        (true, true) => Ok(true),
        (false, false) => Ok(false),
        _ => anyhow::bail!("incomplete saved TLS identity; restore the matching cert/key pair instead of replacing either file"),
    }
}

struct IdentityLock(File);
impl Drop for IdentityLock {
    fn drop(&mut self) {
        // Explicit unlock also releases duplicates temporarily inherited by fork.
        // SAFETY: this struct still owns the valid descriptor during Drop.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

struct StagingDirectory(PathBuf);
impl StagingDirectory {
    fn create(parent: &Path) -> Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let path = parent.join(format!(".tls-pending-{}-{}", std::process::id(), NEXT_STAGING.fetch_add(1, Ordering::Relaxed)));
        fs::DirBuilder::new().mode(0o700).create(&path).context("failed to create private TLS staging directory")?;
        Ok(Self(path))
    }
    fn write(&self, name: &str, bytes: &[u8], mode: u32) -> Result<PathBuf> {
        let path = self.0.join(name);
        let mut file = OpenOptions::new().write(true).create_new(true).mode(mode).open(&path)
            .context("failed to create staged TLS file")?;
        file.write_all(bytes).context("failed to write staged TLS file")?;
        file.sync_all().context("failed to sync staged TLS file")?;
        Ok(path)
    }
}
impl Drop for StagingDirectory {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}

/// Generate only if both paths are absent. Publish without replacing any entry.
/// A crash between the two publications leaves an incomplete pair that fails
/// closed on the next start, rather than silently changing the server identity.
fn auto_generate_tls_in(config_dir: &Path) -> Result<(PathBuf, PathBuf)> {
    let cert_path = config_dir.join("cert.pem");
    let key_path = config_dir.join("key.pem");
    if reuse_pair(&cert_path, &key_path)? { return Ok((cert_path, key_path)); }
    fs::create_dir_all(config_dir).context("failed to create config directory")?;
    let lock_file = OpenOptions::new().read(true).write(true).create(true).truncate(false).mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(config_dir.join(".tls.lock")).context("failed to open TLS lock")?;
    anyhow::ensure!(lock_file.metadata()?.is_file(), "TLS lock must be a regular file");
    // SAFETY: lock_file owns a valid fd. LOCK_NB prevents an unbounded wait.
    anyhow::ensure!(unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "TLS identity provisioning is busy or unavailable; retry after the other process exits");
    let _lock = IdentityLock(lock_file);
    if reuse_pair(&cert_path, &key_path)? { return Ok((cert_path, key_path)); }

    let key_pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_RSA_SHA256)
        .context("failed to generate an RSA TLS key")?;
    let cert = rcgen::CertificateParams::new(vec!["localhost".into(), "127.0.0.1".into()])?
        .self_signed(&key_pair).context("failed to generate TLS certificate")?;
    let staging = StagingDirectory::create(config_dir)?;
    let tmp_key = staging.write("key.pem", key_pair.serialize_pem().as_bytes(), 0o600)?;
    let tmp_cert = staging.write("cert.pem", cert.pem().as_bytes(), 0o644)?;
    fs::hard_link(tmp_key, &key_path).context("failed to publish TLS key without replacement")?;
    fs::hard_link(tmp_cert, &cert_path).context("failed to publish TLS certificate without replacement")?;
    File::open(config_dir)?.sync_all().context("failed to sync TLS identity directory")?;
    tracing::info!("Generated self-signed TLS identity in {}", config_dir.display());
    Ok((cert_path, key_path))
}

#[cfg(test)]
mod tests {
    use super::{auto_generate_tls_in, load_identity_at, resolve_tls_paths, Provisioning};
    use std::os::unix::fs::PermissionsExt;

    use ironrdp_server::TlsIdentityCtx;

    #[test]
    fn resolve_tls_paths_requires_cert_and_key_pair() {
        let cert_only =
            resolve_tls_paths(Some("/tmp/cert.pem"), None, Provisioning::ExistingOnly).expect_err("cert without key must fail");
        assert!(format!("{cert_only:#}").contains("--cert provided without --key"));

        let key_only =
            resolve_tls_paths(None, Some("/tmp/key.pem"), Provisioning::ExistingOnly).expect_err("key without cert must fail");
        assert!(format!("{key_only:#}").contains("--key provided without --cert"));
    }

    #[test]
    fn resolve_tls_paths_preserves_custom_cert_and_key_paths() {
        let (cert, key) = resolve_tls_paths(Some("/tmp/cert.pem"), Some("/tmp/key.pem"), Provisioning::ExistingOnly)
            .expect("custom cert/key pair");

        assert_eq!(cert, "/tmp/cert.pem");
        assert_eq!(key, "/tmp/key.pem");
    }

    #[test]
    fn auto_generated_tls_identity_loads_and_reuses_existing_pair() {
        let _ = rustls::crypto::ring::default_provider().install_default();

        let dir = unique_temp_dir();
        std::fs::create_dir_all(&dir).expect("temp dir");

        let (cert, key) = auto_generate_tls_in(&dir).expect("generate TLS identity");
        assert!(cert.exists());
        assert!(key.exists());
        assert_eq!(cert, dir.join("cert.pem"));
        assert_eq!(key, dir.join("key.pem"));

        let key_mode = std::fs::metadata(&key)
            .expect("key metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(key_mode, 0o600);

        let tls_ctx =
            TlsIdentityCtx::init_from_paths(&cert, &key).expect("generated TLS identity loads");
        tls_ctx
            .make_acceptor()
            .expect("generated TLS acceptor is valid");

        let cert_metadata = std::fs::metadata(&cert).expect("cert metadata");
        let key_metadata = std::fs::metadata(&key).expect("key metadata");
        let reused = auto_generate_tls_in(&dir).expect("reuse TLS identity");
        assert_eq!(reused, (cert.clone(), key.clone()));
        assert_eq!(
            std::fs::metadata(&cert)
                .expect("reused cert metadata")
                .modified()
                .expect("reused cert modified time"),
            cert_metadata.modified().expect("cert modified time")
        );
        assert_eq!(
            std::fs::metadata(&key)
                .expect("reused key metadata")
                .modified()
                .expect("reused key modified time"),
            key_metadata.modified().expect("key modified time")
        );

        std::fs::remove_dir_all(&dir).expect("cleanup temp dir");
    }

    const RSA_ENCRYPTION_OID: &[u8] = &[
        0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01,
    ];
    const EC_PUBLIC_KEY_OID: &[u8] = &[0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    #[test]
    fn the_auto_generated_certificate_uses_an_rsa_key() {
        let _ = rustls::crypto::ring::default_provider().install_default();

        let dir = unique_temp_dir();
        std::fs::create_dir_all(&dir).expect("temp dir");
        let (cert, key) = auto_generate_tls_in(&dir).expect("generate TLS identity");

        let tls_ctx =
            TlsIdentityCtx::init_from_paths(&cert, &key).expect("generated TLS identity loads");
        let der = tls_ctx.certs.first().expect("one certificate").as_ref();

        assert!(contains(der, RSA_ENCRYPTION_OID));
        assert!(!contains(der, EC_PUBLIC_KEY_OID));

        std::fs::remove_dir_all(&dir).expect("cleanup temp dir");
    }

    #[test]
    fn an_existing_ecdsa_identity_is_not_replaced() {
        let _ = rustls::crypto::ring::default_provider().install_default();

        let dir = unique_temp_dir();
        std::fs::create_dir_all(&dir).expect("temp dir");
        let key_pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
            .expect("generate ECDSA key");
        let cert = rcgen::CertificateParams::new(vec!["localhost".to_string()])
            .expect("certificate parameters")
            .self_signed(&key_pair)
            .expect("self-signed certificate");
        let cert_path = dir.join("cert.pem");
        let key_path = dir.join("key.pem");
        let cert_pem = cert.pem();
        let key_pem = key_pair.serialize_pem();
        std::fs::write(&cert_path, &cert_pem).expect("write certificate");
        std::fs::write(&key_path, &key_pem).expect("write key");
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let reused = auto_generate_tls_in(&dir).expect("reuse TLS identity");

        assert_eq!(reused, (cert_path.clone(), key_path.clone()));
        assert_eq!(
            std::fs::read_to_string(&cert_path).expect("read certificate"),
            cert_pem
        );
        assert_eq!(
            std::fs::read_to_string(&key_path).expect("read key"),
            key_pem
        );
        TlsIdentityCtx::init_from_paths(&cert_path, &key_path).expect("reused identity loads");
        load_identity_at(&cert_path, &key_path, std::time::SystemTime::now()).unwrap();
        std::fs::remove_dir_all(&dir).expect("cleanup temp dir");
    }

    fn fixed_pair(dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
        std::fs::create_dir_all(dir).unwrap();
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec!["localhost".into()]).unwrap();
        params.not_before = rcgen::date_time_ymd(2020, 1, 1);
        params.not_after = rcgen::date_time_ymd(2030, 1, 1);
        let cert = params.self_signed(&key).unwrap();
        let cert_path = dir.join("cert.pem");
        let key_path = dir.join("key.pem");
        std::fs::write(&cert_path, cert.pem()).unwrap();
        std::fs::write(&key_path, key.serialize_pem()).unwrap();
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        (cert_path, key_path)
    }

    #[test]
    fn incomplete_saved_pairs_are_never_replaced_or_completed_implicitly() {
        for name in ["cert.pem", "key.pem"] {
            let dir = unique_temp_dir();
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join(name);
            std::fs::write(&path, "CANARY-IDENTITY").unwrap();
            assert!(auto_generate_tls_in(&dir).is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "CANARY-IDENTITY");
            assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn dangling_identity_entries_count_as_existing_not_as_permission_to_replace() {
        let dir = unique_temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("key.pem");
        std::os::unix::fs::symlink(dir.join("absent-target"), &path).unwrap();
        assert!(auto_generate_tls_in(&dir).is_err());
        assert!(std::fs::symlink_metadata(&path).unwrap().file_type().is_symlink());
        assert!(!dir.join("cert.pem").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn loaded_identity_checks_key_permissions_matching_and_validity() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dir = unique_temp_dir();
        let (cert, key) = fixed_pair(&dir);
        let valid_time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let prepared = load_identity_at(&cert, &key, valid_time).unwrap();
        let original = TlsIdentityCtx::init_from_paths(&cert, &key).unwrap();
        assert_eq!(prepared.pub_key, original.pub_key);
        assert!(!prepared.acceptor.config().key_log.will_log("CLIENT_RANDOM"));
        assert!(format!("{:#}", load_identity_at(&cert, &key, std::time::UNIX_EPOCH).err().unwrap()).contains("not yet valid"));
        let expired_time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(2_000_000_000);
        assert!(format!("{:#}", load_identity_at(&cert, &key, expired_time).err().unwrap()).contains("expired"));
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(format!("{:#}", load_identity_at(&cert, &key, valid_time).err().unwrap()).contains("group or others"));
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&key, rcgen::KeyPair::generate().unwrap().serialize_pem()).unwrap();
        assert!(format!("{:#}", load_identity_at(&cert, &key, valid_time).err().unwrap()).contains("do not match"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn malformed_pem_errors_do_not_echo_private_file_contents() {
        let dir = unique_temp_dir();
        let (cert, key) = fixed_pair(&dir);
        std::fs::write(&key, "CANARY-PRIVATE-KEY").unwrap();
        let error = load_identity_at(&cert, &key, std::time::SystemTime::now()).err().unwrap();
        assert!(!format!("{error:#}").contains("CANARY-PRIVATE-KEY"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn identity_provisioning_does_not_wait_on_a_busy_lock() {
        use std::os::fd::AsRawFd;
        let dir = unique_temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let lock = super::IdentityLock(std::fs::File::create(dir.join(".tls.lock")).unwrap());
        // SAFETY: lock owns the descriptor. This is a disposable test file.
        assert_eq!(unsafe { libc::flock(lock.0.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }, 0);
        assert!(auto_generate_tls_in(&dir).is_err());
        assert!(!dir.join("cert.pem").exists());
        assert!(!dir.join("key.pem").exists());
        drop(lock);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn fifo_is_rejected_without_waiting_for_a_writer() {
        use std::os::unix::ffi::OsStrExt;
        let dir = unique_temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("key.pem");
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: name is NUL terminated and the test owns its directory.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(super::read_pem(&path, true).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn unique_temp_dir() -> std::path::PathBuf {
        let unique = format!(
            "hypr-rdp-tls-test-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        );
        std::env::temp_dir().join(unique)
    }
}
