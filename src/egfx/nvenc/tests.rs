//! CPU compression oracle behind the same private worker protocol. This is not
//! an NVENC implementation and never loads a system NVIDIA library.
use super::*;
use hypr_rdp::nvenc::{
    AccessUnit, Config, Driver, Error, Fault, Frame, Phase, Submission, WORKER_FD,
};
use openh264::{
    encoder::{Encoder, EncoderConfig},
    formats::YUVSource,
    OpenH264API,
};
use std::{
    os::fd::FromRawFd,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);
pub(crate) struct TestDirectory(PathBuf);
impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(crate) fn test_encoder(fail: bool) -> (NvencEncoder, TestDirectory) {
    let root = TestDirectory(std::env::temp_dir().join(format!(
        "hypr-rdp-capture-oracle-{}-{}",
        std::process::id(),
        NEXT_DIR.fetch_add(1, Ordering::Relaxed)
    )));
    let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
    command.args([
        "--exact",
        if fail {
            "egfx::nvenc::tests::fixture_driver_error"
        } else {
            "egfx::nvenc::tests::fixture_normal"
        },
        "--ignored",
        "--test-threads=1",
    ]);
    let config = config();
    let encoder =
        NvencEncoder::with_command(config, &root.0, command, Deadlines::default()).unwrap();
    (encoder, root)
}

fn config() -> Config {
    Config {
        device: "GPU-01234567-89ab-cdef-0123-456789abcdef".parse().unwrap(),
        epoch: 77,
        width: 64,
        height: 64,
        bitrate: 1_000_000,
        fps: 30,
    }
}

struct Planar {
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    width: usize,
    height: usize,
}
impl YUVSource for Planar {
    fn dimensions(&self) -> (usize, usize) {
        (self.width, self.height)
    }
    fn strides(&self) -> (usize, usize, usize) {
        (self.width, self.width / 2, self.width / 2)
    }
    fn y(&self) -> &[u8] {
        &self.y
    }
    fn u(&self) -> &[u8] {
        &self.u
    }
    fn v(&self) -> &[u8] {
        &self.v
    }
}
struct CpuDriver {
    config: Option<Config>,
    encoder: Option<Encoder>,
    output: Option<AccessUnit>,
    fail: bool,
}
impl Driver for CpuDriver {
    fn probe(&mut self, config: &Config) -> Result<(), Error> {
        self.config = Some(config.clone());
        Ok(())
    }
    fn initialize(&mut self, _: &Config) -> Result<(), Error> {
        self.encoder = Some(
            Encoder::with_api_config(OpenH264API::from_source(), EncoderConfig::new()).unwrap(),
        );
        Ok(())
    }
    fn allocate(&mut self, _: usize) -> Result<(), Error> {
        Ok(())
    }
    fn submit(&mut self, _: usize, frame: &Frame<'_>) -> Result<Submission, Error> {
        if self.fail && frame.stamp.sequence > 0 {
            return Err(Error::backend(Phase::Encode, Fault::Driver));
        }
        let config = self.config.as_ref().unwrap();
        let size = config.width as usize * config.height as usize;
        let mut source = Planar {
            y: frame.nv12[..size].to_vec(),
            u: Vec::new(),
            v: Vec::new(),
            width: config.width as usize,
            height: config.height as usize,
        };
        for uv in frame.nv12[size..].as_chunks::<2>().0.iter() {
            source.u.push(uv[0]);
            source.v.push(uv[1]);
        }
        let encoder = self.encoder.as_mut().unwrap();
        if frame.force_idr {
            encoder.force_intra_frame();
        }
        let bytes = encoder.encode(&source).unwrap().to_vec();
        let is_idr = crate::egfx::h264::annex_b_nal_types(&bytes).contains(&5);
        self.output = Some(AccessUnit { bytes, is_idr });
        Ok(Submission::Ready)
    }
    fn output(&mut self, _: usize) -> Result<AccessUnit, Error> {
        self.output.take().ok_or(Error::Closed)
    }
    fn drain(&mut self) -> Result<(), Error> {
        Ok(())
    }
    fn release(&mut self, _: usize) -> Result<(), Error> {
        Ok(())
    }
    fn destroy(&mut self) -> Result<(), Error> {
        self.encoder = None;
        Ok(())
    }
}
fn fixture(fail: bool) {
    if std::env::var("HYPR_RDP_NVENC_WORKER").as_deref() != Ok("1") {
        return;
    }
    // SAFETY: only the supervisor-launched child owns inherited fd3.
    let stream = unsafe { std::os::unix::net::UnixStream::from_raw_fd(WORKER_FD) };
    let _ = hypr_rdp::nvenc::serve(
        stream,
        CpuDriver {
            config: None,
            encoder: None,
            output: None,
            fail,
        },
    );
}
#[test]
#[ignore = "subprocess fixture"]
fn fixture_normal() {
    fixture(false);
}
#[test]
#[ignore = "subprocess fixture"]
fn fixture_driver_error() {
    fixture(true);
}

#[test]
fn nv12_conversion_uses_full_range_bt709_and_interleaves_uv_with_padding() {
    for (pixel, expected) in [
        ([0, 0, 255, 0], [54, 99, 255]),
        ([255, 0, 0, 0], [18, 255, 116]),
        ([0, 255, 0, 0], [182, 30, 12]),
        ([255, 255, 255, 0], [255, 128, 128]),
    ] {
        let cfg = config();
        let stride = cfg.width as usize * 4 + 32;
        let mut bytes = vec![0xED; stride * cfg.height as usize];
        for row in bytes.chunks_exact_mut(stride) {
            for p in row[..cfg.width as usize * 4]
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
            {
                p.copy_from_slice(&pixel);
            }
        }
        let mut pixels = Nv12Pixels::new(&cfg).unwrap();
        let nv12 = pixels.convert(&bytes, stride).unwrap();
        let y_size = cfg.width as usize * cfg.height as usize;
        assert!(nv12[..y_size]
            .iter()
            .all(|y| (i16::from(*y) - expected[0]).abs() <= 2));
        assert!(nv12[y_size..]
            .as_chunks::<2>()
            .0
            .iter()
            .all(|uv| (i16::from(uv[0]) - expected[1]).abs() <= 2
                && (i16::from(uv[1]) - expected[2]).abs() <= 2));
        assert!(pixels.convert(&bytes[..4], stride).is_err());
    }
}

#[test]
fn capture_ipc_cpu_encode_decode_and_idr_recovery_preserve_pixels() {
    let (mut encoder, root) = test_encoder(false);
    let data: Vec<u8> = [0, 0, 255, 255].repeat(64 * 64);
    let mut decoder = openh264::decoder::Decoder::new().unwrap();
    for index in 0..3 {
        if index == 2 {
            encoder.force_idr();
        }
        let bytes = encoder.encode(&data, 64 * 4).unwrap();
        if index == 0 || index == 2 {
            assert!(crate::egfx::h264::annex_b_nal_types(&bytes).contains(&5));
        }
        let frame = decoder.decode(&bytes).unwrap().unwrap();
        assert_eq!(frame.dimensions(), (64, 64));
        assert!((i16::from(frame.y()[0]) - 54).abs() <= 4);
        assert!((i16::from(frame.u()[0]) - 99).abs() <= 4);
        assert!((i16::from(frame.v()[0]) - 255).abs() <= 4);
    }
    drop(encoder);
    drop(root);
}

#[test]
fn fallback_requires_a_classified_fault_and_confirmed_exit() {
    for error in [
        Error::Protocol,
        Error::Io,
        Error::Quarantined,
        Error::Timeout(Phase::Encode),
        Error::StateStore,
    ] {
        assert!(!can_fallback(
            &Failure {
                error,
                reaped: true
            }
            .into()
        ));
    }
    let error = Error::backend(Phase::Encode, Fault::Driver);
    assert!(!can_fallback(
        &Failure {
            error,
            reaped: false
        }
        .into()
    ));
    assert!(can_fallback(
        &Failure {
            error,
            reaped: true
        }
        .into()
    ));
}

/// pacman extracts with ARCHIVE_EXTRACT_UNLINK: an upgrade unlinks the running
/// executable's pathname and creates a new inode there. A private copy of this
/// test binary replaces its own pathname, then both production self-exec paths
/// must still start the running image (same inode), not the replacement.
#[test]
fn self_exec_paths_survive_replacement_of_the_running_executable() {
    let directory = std::env::temp_dir().join(format!(
        "hypr-rdp-self-exec-{}-{}",
        std::process::id(),
        NEXT_DIR.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let copy = directory.join("hypr-rdp-under-test");
    std::fs::copy(std::env::current_exe().unwrap(), &copy).unwrap();
    let status = std::process::Command::new(&copy)
        .args([
            "--exact",
            "egfx::nvenc::tests::fixture_self_exec_parent",
            "--ignored",
            "--nocapture",
        ])
        .env("HYPR_RDP_SELF_EXEC_PARENT", "1")
        .status()
        .unwrap();
    std::fs::remove_dir_all(&directory).unwrap();
    assert!(status.success());
}

#[test]
#[ignore = "subprocess fixture"]
fn fixture_self_exec_parent() {
    use std::os::unix::fs::MetadataExt;
    if std::env::var_os("HYPR_RDP_SELF_EXEC_PARENT").is_none() {
        return;
    }
    let image = std::fs::metadata("/proc/self/exe").unwrap().ino();
    // PamValidator resolves its helper once at server start, before an upgrade.
    let helper_at_start = crate::server::auth::helper_executable().unwrap();
    let path = std::env::current_exe().unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, b"replacement package payload").unwrap();
    let worker = super::worker_command().unwrap();
    let programs = [
        ("NVENC worker", worker.as_std().get_program().to_owned()),
        ("PAM helper resolved at start", helper_at_start.into_os_string()),
        (
            "PAM helper resolved now",
            crate::server::auth::helper_executable()
                .unwrap()
                .into_os_string(),
        ),
    ];
    let mut failures = Vec::new();
    for (role, program) in programs {
        let output = match std::process::Command::new(&program)
            .args([
                "--exact",
                "egfx::nvenc::tests::fixture_self_exec_child",
                "--ignored",
                "--nocapture",
            ])
            .env("HYPR_RDP_SELF_EXEC_CHILD", "1")
            .output()
        {
            Ok(output) => output,
            Err(error) => {
                failures.push(format!("{role}: spawn {program:?} after replacement: {error}"));
                continue;
            }
        };
        let reported = String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|line| line.strip_prefix("self-exec-inode="))
            .and_then(|inode| inode.parse::<u64>().ok());
        if !output.status.success() || reported != Some(image) {
            failures.push(format!(
                "{role}: {program:?} ran image {reported:?}, expected {image}"
            ));
        }
    }
    assert!(failures.is_empty(), "self-exec after replacement: {failures:#?}");
}

#[test]
#[ignore = "subprocess fixture"]
fn fixture_self_exec_child() {
    use std::os::unix::fs::MetadataExt;
    if std::env::var_os("HYPR_RDP_SELF_EXEC_CHILD").is_none() {
        return;
    }
    let image = std::fs::metadata("/proc/self/exe").unwrap().ino();
    println!("self-exec-inode={image}");
}
