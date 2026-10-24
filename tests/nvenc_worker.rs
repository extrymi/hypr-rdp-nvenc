//! CPU-only subprocess tests. Ignored `fixture_*` entries are invoked explicitly
//! as children with a private descriptor; they are not missing hardware tests.
#![cfg(target_os = "linux")]

mod support;

use std::{io::{Read, Write}, os::{fd::FromRawFd, unix::net::UnixStream}, path::PathBuf, sync::atomic::{AtomicUsize, Ordering}, time::Duration};

use hypr_rdp::nvenc::{serve, Config, Deadlines, DeviceLease, Error, Fault, Frame, Phase, Stamp, Worker, WORKER_FD};
use support::{config, stall, FakeDriver, Scenario};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct RuntimeDir(PathBuf);
impl RuntimeDir {
    fn new() -> Self { Self(std::env::temp_dir().join(format!("hypr-rdp-nvenc-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)))) }
    fn lease(&self) -> DeviceLease { DeviceLease::acquire(&self.0, config().device).unwrap() }
}
impl Drop for RuntimeDir { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }

fn deadlines() -> Deadlines {
    Deadlines { startup: Duration::from_secs(2), initialize: Duration::from_millis(300), encode: Duration::from_millis(150), drain: Duration::from_millis(150), destroy: Duration::from_millis(150), exit: Duration::from_millis(150), reap: Duration::from_secs(2) }
}

async fn start(name: &str, directory: &RuntimeDir, cfg: Config) -> Result<Worker, Error> {
    let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", name, "--ignored", "--test-threads=1"]);
    Worker::spawn(command, directory.lease(), cfg, deadlines()).await
}

fn inherited_socket() -> UnixStream {
    // SAFETY: only fixture_* child entries call this. Worker::spawn has placed
    // their private socket at WORKER_FD; this is its only owning Rust handle.
    unsafe { UnixStream::from_raw_fd(WORKER_FD) }
}

fn run(scenario: Scenario) { let _ = serve(inherited_socket(), FakeDriver::new(scenario)); }

#[test] #[ignore = "subprocess fixture"] fn fixture_normal() { run(Scenario::Normal); }
#[test] #[ignore = "subprocess fixture"] fn fixture_missing() { run(Scenario::MissingLibrary); }
#[test] #[ignore = "subprocess fixture"] fn fixture_initialize_error() { run(Scenario::FailInitialize); }
#[test] #[ignore = "subprocess fixture"] fn fixture_fail_after() { run(Scenario::FailAfter(2)); }
#[test] #[ignore = "subprocess fixture"] fn fixture_startup() { stall(); }
#[test] #[ignore = "subprocess fixture"] fn fixture_probe() { run(Scenario::Hang(Phase::Probe)); }
#[test] #[ignore = "subprocess fixture"] fn fixture_initialize() { run(Scenario::Hang(Phase::Initialize)); }
#[test] #[ignore = "subprocess fixture"] fn fixture_allocate() { run(Scenario::HangAllocate); }
#[test] #[ignore = "subprocess fixture"] fn fixture_encode() { run(Scenario::Hang(Phase::Encode)); }
#[test] #[ignore = "subprocess fixture"] fn fixture_output() { run(Scenario::HangOutput); }
#[test] #[ignore = "subprocess fixture"] fn fixture_drain() { run(Scenario::Hang(Phase::Drain)); }
#[test] #[ignore = "subprocess fixture"] fn fixture_destroy() { run(Scenario::Hang(Phase::Destroy)); }
#[test] #[ignore = "subprocess fixture"] fn fixture_exit() { run(Scenario::Hang(Phase::Exit)); }

fn packet(socket: &mut UnixStream, payload: &[u8]) {
    socket.write_all(&(payload.len() as u32).to_le_bytes()).unwrap();
    socket.write_all(payload).unwrap();
}
fn discard(socket: &mut UnixStream) {
    let mut prefix = [0; 4]; socket.read_exact(&mut prefix).unwrap();
    let length = u32::from_le_bytes(prefix) as usize;
    assert!(length <= 64 * 1024 * 1024 + 128);
    let mut bytes = vec![0; length]; socket.read_exact(&mut bytes).unwrap();
}
fn raw_ready() -> UnixStream {
    let mut socket = inherited_socket();
    packet(&mut socket, b"HNV1\0"); discard(&mut socket); packet(&mut socket, b"HNV1\x02"); socket
}
#[test] #[ignore = "subprocess fixture"] fn fixture_stale() {
    let mut socket = raw_ready(); discard(&mut socket);
    let mut reply = b"HNV1\x04".to_vec();
    reply.extend_from_slice(&16_u64.to_le_bytes()); // old epoch
    reply.extend_from_slice(&0_u64.to_le_bytes());
    reply.extend_from_slice(&[1, 0, 0, 0, 1, 0x65]);
    packet(&mut socket, &reply); stall();
}
#[test] #[ignore = "subprocess fixture"] fn fixture_oversized() {
    let mut socket = raw_ready(); discard(&mut socket);
    socket.write_all(&u32::MAX.to_le_bytes()).unwrap(); stall();
}
#[test] #[ignore = "subprocess fixture"] fn fixture_partial() {
    let mut socket = raw_ready(); discard(&mut socket);
    socket.write_all(&30_u32.to_le_bytes()).unwrap();
    for byte in b"HNV1\x04" {
        socket.write_all(&[*byte]).unwrap();
        std::thread::sleep(Duration::from_millis(100));
    }
    stall();
}
#[test] #[ignore = "subprocess fixture"] fn fixture_not_reading() { let _socket = raw_ready(); stall(); }

#[tokio::test]
async fn real_process_roundtrip_and_repeated_shutdown_release_the_lease() {
    let dir = RuntimeDir::new();
    for _ in 0..4 {
        let mut worker = start("fixture_normal", &dir, config()).await.unwrap();
        for sequence in 0..5 {
            let stamp = Stamp { epoch: 17, sequence };
            let output = worker.encode(Frame { stamp, force_idr: sequence == 3, nv12: &[0; 24] }).await.unwrap();
            assert_eq!(output.stamp, stamp);
            assert_eq!(output.unit.is_idr, sequence == 0 || sequence == 3);
        }
        worker.shutdown().await.unwrap();
        worker.shutdown().await.unwrap();
        assert!(worker.was_reaped());
        assert!(!dir.0.join(format!("{}.worker", config().device)).exists());
    }
}

#[tokio::test]
async fn classified_startup_failures_are_preserved_and_do_not_poison_next_worker() {
    let dir = RuntimeDir::new();
    for (fixture, phase, fault) in [("fixture_missing", Phase::Probe, Fault::LibraryMissing), ("fixture_initialize_error", Phase::Initialize, Fault::Driver)] {
        assert!(matches!(start(fixture, &dir, config()).await, Err(Error::Backend { phase: observed_phase, fault: observed_fault, .. }) if observed_phase == phase && observed_fault == fault));
        let mut worker = start("fixture_normal", &dir, config()).await.unwrap();
        worker.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn startup_probe_initialize_and_allocation_hangs_have_bounded_parent_wait() {
    let dir = RuntimeDir::new();
    for (fixture, phase) in [("fixture_startup", Phase::Startup), ("fixture_probe", Phase::Initialize), ("fixture_initialize", Phase::Initialize), ("fixture_allocate", Phase::Initialize)] {
        let began = std::time::Instant::now();
        assert!(matches!(start(fixture, &dir, config()).await, Err(Error::Timeout(observed)) if observed == phase));
        assert!(began.elapsed() < Duration::from_secs(8));
        // A successfully reaped process permits another generation.
        let mut worker = start("fixture_normal", &dir, config()).await.unwrap();
        worker.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn encode_output_and_stalled_writer_hangs_end_generation_and_reap_child() {
    let dir = RuntimeDir::new();
    for fixture in ["fixture_encode", "fixture_output", "fixture_not_reading", "fixture_partial"] {
        let cfg = if fixture == "fixture_not_reading" { Config { width: 2048, height: 2048, ..config() } } else { config() };
        let bytes = vec![0; cfg.frame_bytes().unwrap()];
        let mut worker = start(fixture, &dir, cfg).await.unwrap();
        let stamp = Stamp { epoch: 17, sequence: 0 };
        let began = std::time::Instant::now();
        assert_eq!(worker.encode(Frame { stamp, force_idr: false, nv12: &bytes }).await, Err(Error::Timeout(Phase::Encode)));
        assert!(began.elapsed() < Duration::from_secs(5));
        assert!(worker.was_reaped());
        assert_eq!(worker.encode(Frame { stamp, force_idr: false, nv12: &bytes }).await, Err(Error::Closed));
    }
}

#[tokio::test]
async fn drain_destroy_and_exit_hangs_are_killed_without_blocking_shutdown() {
    let dir = RuntimeDir::new();
    for (fixture, phase) in [("fixture_drain", Phase::Drain), ("fixture_destroy", Phase::Destroy), ("fixture_exit", Phase::Exit)] {
        let mut worker = start(fixture, &dir, config()).await.unwrap();
        let began = std::time::Instant::now();
        assert_eq!(worker.shutdown().await, Err(Error::Timeout(phase)));
        assert!(began.elapsed() < Duration::from_secs(5));
        assert!(worker.was_reaped());
        let mut next = start("fixture_normal", &dir, config()).await.unwrap();
        next.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn stale_and_oversized_replies_are_not_accepted_as_video() {
    let dir = RuntimeDir::new();
    for fixture in ["fixture_stale", "fixture_oversized"] {
        let mut worker = start(fixture, &dir, config()).await.unwrap();
        assert_eq!(worker.encode(Frame { stamp: Stamp { epoch: 17, sequence: 0 }, force_idr: false, nv12: &[0; 24] }).await, Err(Error::Protocol));
        assert!(worker.was_reaped());
    }
}

#[tokio::test]
async fn backend_failure_after_two_frames_is_terminal_and_keeps_numeric_status() {
    let dir = RuntimeDir::new();
    let mut worker = start("fixture_fail_after", &dir, config()).await.unwrap();
    for sequence in 0..2 { worker.encode(Frame { stamp: Stamp { epoch: 17, sequence }, force_idr: false, nv12: &[0; 24] }).await.unwrap(); }
    assert_eq!(worker.encode(Frame { stamp: Stamp { epoch: 17, sequence: 2 }, force_idr: false, nv12: &[0; 24] }).await, Err(Error::Backend { phase: Phase::Encode, fault: Fault::Driver, code: -7 }));
    assert!(worker.was_reaped());
}
