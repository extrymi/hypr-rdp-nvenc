use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};

use super::{DeviceId, Error};

/// Exclusive per-device lease with a boot/PID/start-time record. Dropping a
/// lease does not erase the record: a parent crash must not permit a second
/// GPU worker. No PID read from disk is ever signalled or killed.
pub struct DeviceLease {
    lock: File,
    path: PathBuf,
    device: DeviceId,
}

impl Drop for DeviceLease {
    fn drop(&mut self) {
        // flock is attached to the open file description. A concurrently
        // forked child can retain a duplicate until exec closes its CLOEXEC
        // descriptors. Closing only our File would transiently retain the
        // lock. Explicit unlock releases it across those inherited copies.
        // The worker identity record deliberately remains for reconciliation.
        // SAFETY: this lease still owns a valid descriptor during Drop.
        unsafe { libc::flock(self.lock.as_raw_fd(), libc::LOCK_UN) };
    }
}

#[derive(Debug)]
struct Record {
    boot: String,
    pid: u32,
    start: u64,
}

impl DeviceLease {
    /// `root` must be a dedicated private directory under the user's runtime
    /// directory. Its parent must already exist. Symlinks and shared paths fail.
    pub fn acquire(root: &Path, device: DeviceId) -> Result<Self, Error> {
        match fs::DirBuilder::new().mode(0o700).create(root) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(_) => return Err(Error::StateStore),
        }
        let metadata = fs::symlink_metadata(root).map_err(|_| Error::StateStore)?;
        // SAFETY: geteuid has no pointer arguments and cannot violate memory safety.
        let uid = unsafe { libc::geteuid() };
        if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
            return Err(Error::StateStore);
        }
        let lock_path = root.join(format!("{device}.lock"));
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(lock_path)
            .map_err(|_| Error::StateStore)?;
        let metadata = lock.metadata().map_err(|_| Error::StateStore)?;
        if !metadata.is_file() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
            return Err(Error::StateStore);
        }
        // SAFETY: lock owns a valid descriptor for the duration of this call.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(
                if std::io::Error::last_os_error().raw_os_error() == Some(libc::EWOULDBLOCK) {
                    Error::Quarantined
                } else {
                    Error::StateStore
                },
            );
        }
        let lease = Self {
            lock,
            path: root.join(format!("{device}.worker")),
            device,
        };
        lease.clear_if_gone()?;
        Ok(lease)
    }

    pub fn device(&self) -> &DeviceId {
        &self.device
    }

    /// Persist identity before the parent sends Init. Until then, the worker
    /// may exchange Hello but must not load or call a native driver.
    pub(crate) fn bind(&self, pid: u32) -> Result<(), Error> {
        let start = process_start(pid)?.ok_or(Error::StateStore)?;
        let boot = boot_id()?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&self.path)
            .map_err(|_| Error::StateStore)?;
        // Interrupted writes remain malformed and fail closed on reconciliation.
        write!(file, "1\n{boot}\n{pid}\n{start}\n").map_err(|_| Error::StateStore)?;
        file.sync_all().map_err(|_| Error::StateStore)?;
        File::open(self.path.parent().ok_or(Error::StateStore)?)
            .and_then(|dir| dir.sync_all())
            .map_err(|_| Error::StateStore)
    }

    pub(crate) fn clear_if_gone(&self) -> Result<(), Error> {
        let mut file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&self.path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(Error::StateStore),
        };
        let mut text = String::new();
        (&mut file)
            .take(257)
            .read_to_string(&mut text)
            .map_err(|_| Error::StateStore)?;
        let record = parse_record(&text)?;
        let observed = if record.boot == boot_id()? {
            process_start(record.pid)?
        } else {
            None
        };
        if record_is_live(&record, &boot_id()?, observed) {
            return Err(Error::Quarantined);
        }
        fs::remove_file(&self.path).map_err(|_| Error::StateStore)
    }
}

fn parse_record(text: &str) -> Result<Record, Error> {
    let fields: Vec<_> = text.lines().collect();
    if text.len() > 256
        || fields.len() != 4
        || fields[0] != "1"
        || !super::types::valid_uuid(fields[1])
    {
        return Err(Error::StateStore);
    }
    let record = Record {
        boot: fields[1].into(),
        pid: fields[2].parse().map_err(|_| Error::StateStore)?,
        start: fields[3].parse().map_err(|_| Error::StateStore)?,
    };
    if record.pid == 0 || record.start == 0 {
        return Err(Error::StateStore);
    }
    Ok(record)
}

fn record_is_live(record: &Record, boot: &str, observed_start: Option<u64>) -> bool {
    record.boot == boot && observed_start == Some(record.start)
}

fn boot_id() -> Result<String, Error> {
    fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map(|id| id.trim().to_owned())
        .map_err(|_| Error::StateStore)
}

fn process_start(pid: u32) -> Result<Option<u64>, Error> {
    let stat = match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(Error::StateStore),
    };
    // comm may contain spaces and ')'; starttime is field22, state is field3.
    let suffix = stat.rsplit_once(')').ok_or(Error::StateStore)?.1;
    suffix
        .split_whitespace()
        .nth(19)
        .ok_or(Error::StateStore)?
        .parse()
        .map(Some)
        .map_err(|_| Error::StateStore)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconciliation_checks_boot_and_start_time_instead_of_pid_alone() {
        let record = Record {
            boot: "boot-a".into(),
            pid: 42,
            start: 100,
        };
        assert!(record_is_live(&record, "boot-a", Some(100)));
        assert!(!record_is_live(&record, "boot-b", Some(100)));
        assert!(!record_is_live(&record, "boot-a", Some(101)));
        assert!(!record_is_live(&record, "boot-a", None));
    }

    #[test]
    fn malformed_or_partial_records_fail_closed() {
        for text in [
            "",
            "1\n",
            "1\nboot\n12\n",
            "1\nzzzzzzzz-zzzz-zzzz-zzzz-zzzzzzzzzzzz\n1\n2\n",
            "2\n00000000-0000-0000-0000-000000000000\n1\n2\n",
        ] {
            assert!(parse_record(text).is_err());
        }
    }

    #[test]
    fn live_worker_record_survives_dropped_parent_lease_until_process_is_gone() {
        let root = std::env::temp_dir().join(format!("hypr-rdp-lease-{}", std::process::id()));
        let device: DeviceId = "GPU-01234567-89ab-cdef-0123-456789abcdef".parse().unwrap();
        let lease = DeviceLease::acquire(&root, device.clone()).unwrap();
        assert!(matches!(
            DeviceLease::acquire(&root, device.clone()),
            Err(Error::Quarantined)
        ));
        // A benign disposable child represents a worker whose exit has not
        // been confirmed. Only its original Child handle is used to kill it.
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("10")
            .spawn()
            .unwrap();
        lease.bind(child.id()).unwrap();
        drop(lease);
        let blocked = matches!(
            DeviceLease::acquire(&root, device.clone()),
            Err(Error::Quarantined)
        );
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(blocked);
        let next = DeviceLease::acquire(&root, device).unwrap();
        assert!(!next.path.exists());
        drop(next);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn inherited_descriptor_does_not_retain_a_released_parent_lock() {
        let root = std::env::temp_dir().join(format!("hypr-rdp-inherited-{}", std::process::id()));
        let device: DeviceId = "GPU-01234567-89ab-cdef-0123-456789abcdef".parse().unwrap();
        let lease = DeviceLease::acquire(&root, device.clone()).unwrap();
        // dup shares the open file description, just like a descriptor
        // inherited by another thread's fork before the child reaches exec.
        let inherited = lease.lock.try_clone().unwrap();
        drop(lease);
        let next = DeviceLease::acquire(&root, device).unwrap();
        drop(next);
        drop(inherited);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn shared_or_symlinked_state_directory_is_rejected() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let root =
            std::env::temp_dir().join(format!("hypr-rdp-permissions-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        let device: DeviceId = "GPU-01234567-89ab-cdef-0123-456789abcdef".parse().unwrap();
        assert!(matches!(
            DeviceLease::acquire(&root, device.clone()),
            Err(Error::StateStore)
        ));
        let link = root.with_extension("link");
        symlink(&root, &link).unwrap();
        assert!(matches!(
            DeviceLease::acquire(&link, device),
            Err(Error::StateStore)
        ));
        std::fs::remove_file(link).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}
