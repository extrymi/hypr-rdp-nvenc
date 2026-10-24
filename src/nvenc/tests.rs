use super::*;

// Reuse the same fake driver in engine unit tests and real-process IPC tests.
// Its imports refer to this crate in unit tests via the alias below.
#[path = "../../tests/support/mod.rs"]
mod support;
use support::{config, FakeDriver, Scenario};

fn frame(sequence: u64, bytes: &[u8]) -> Frame<'_> {
    Frame { stamp: Stamp { epoch: 17, sequence }, force_idr: false, nv12: bytes }
}

#[test]
fn config_rejects_invalid_geometry_and_unstable_device_ids() {
    for device in ["0", "/dev/dri/renderD128", "GPU-bad", "GPU-../../../../../../../../../../../../.."] {
        assert!(device.parse::<DeviceId>().is_err());
    }
    for (width, height) in [(0, 4), (3, 4), (4, 3), (8194, 2), (u32::MAX, u32::MAX)] {
        let cfg = Config { width, height, ..config() };
        assert_eq!(cfg.frame_bytes(), Err(Error::InvalidConfig));
    }
    assert_eq!(config().frame_bytes().unwrap(), 24);
}

#[test]
fn probe_errors_remain_distinct_without_initializing_resources() {
    for (scenario, fault) in [(Scenario::MissingLibrary, Fault::LibraryMissing), (Scenario::OldApi, Fault::ApiUnsupported), (Scenario::NoDevice, Fault::DeviceUnavailable), (Scenario::UnsupportedCaps, Fault::CapabilityUnsupported)] {
        let driver = FakeDriver::new(scenario);
        let events = driver.events.clone();
        assert!(matches!(Engine::new(driver, config()), Err(Error::Backend { phase: Phase::Probe, fault: observed, .. }) if observed == fault));
        assert_eq!(*events.lock().unwrap(), ["probe", "drop"]);
    }
}

#[test]
fn partial_initialization_and_allocations_are_cleaned_once() {
    for scenario in [Scenario::FailInitialize, Scenario::FailAllocate(2)] {
        let driver = FakeDriver::new(scenario);
        let events = driver.events.clone();
        assert!(Engine::new(driver, config()).is_err());
        let events = events.lock().unwrap();
        assert_eq!(events.iter().filter(|event| *event == "destroy").count(), 1);
        let released: Vec<_> = events.iter().filter(|event| event.starts_with("release:")).map(String::as_str).collect();
        assert_eq!(released, if scenario == Scenario::FailInitialize { vec![] } else { vec!["release:1", "release:0"] });
    }
}

#[test]
fn release_failure_does_not_prevent_remaining_cleanup_or_cause_double_destroy() {
    let driver = FakeDriver::new(Scenario::FailRelease);
    let events = driver.events.clone();
    let mut engine = Engine::new(driver, config()).unwrap();
    assert!(engine.close().is_err());
    assert!(engine.close().is_ok());
    drop(engine);
    let events = events.lock().unwrap();
    for slot in 0..BUFFER_COUNT {
        assert_eq!(events.iter().filter(|event| **event == format!("release:{slot}")).count(), 1);
    }
    assert_eq!(events.iter().filter(|event| *event == "destroy").count(), 1);
}

#[test]
fn delayed_accepted_frame_is_never_resubmitted_or_locked() {
    let driver = FakeDriver::new(Scenario::Delayed);
    let events = driver.events.clone();
    let mut engine = Engine::new(driver, config()).unwrap();
    assert_eq!(engine.encode(frame(0, &[0; 24])), Err(Error::backend(Phase::Encode, Fault::DelayedOutput)));
    assert_eq!(engine.encode(frame(0, &[0; 24])), Err(Error::Closed));
    let events = events.lock().unwrap();
    assert_eq!(events.iter().filter(|event| event.starts_with("submit:")).count(), 1);
    assert!(!events.iter().any(|event| event.starts_with("output:")));
}

#[test]
fn only_unaccepted_input_is_retried_with_a_finite_budget() {
    for busy in [2, 3] {
        let driver = FakeDriver::new(Scenario::Busy(busy));
        let events = driver.events.clone();
        let mut engine = Engine::new(driver, config()).unwrap();
        let result = engine.encode(frame(0, &[0; 24]));
        assert_eq!(result.is_ok(), busy == 2);
        let events = events.lock().unwrap();
        assert_eq!(events.iter().filter(|event| event.starts_with("submit:")).count(), 3);
        assert_eq!(events.iter().filter(|event| event.starts_with("output:")).count(), usize::from(busy == 2));
    }
}

#[test]
fn invalid_inputs_do_not_touch_driver_and_first_frame_forces_idr() {
    let driver = FakeDriver::new(Scenario::Normal);
    let events = driver.events.clone();
    let mut engine = Engine::new(driver, config()).unwrap();
    assert_eq!(engine.encode(frame(1, &[0; 24])), Err(Error::InvalidFrame));
    assert_eq!(engine.encode(frame(0, &[0; 23])), Err(Error::InvalidFrame));
    let mut stale = frame(0, &[0; 24]); stale.stamp.epoch = 16;
    assert_eq!(engine.encode(stale), Err(Error::InvalidFrame));
    assert_eq!(engine.encode(frame(0, &[0; 24])).unwrap().stamp.sequence, 0);
    assert_eq!(engine.encode(frame(0, &[0; 24])), Err(Error::InvalidFrame));
    assert!(engine.encode(frame(1, &[0; 24])).is_ok());
    let events = events.lock().unwrap();
    let submissions: Vec<_> = events.iter().filter(|event| event.starts_with("submit:")).map(String::as_str).collect();
    assert_eq!(submissions, ["submit:0:0:true", "submit:1:1:false"]);
}

#[test]
fn empty_output_or_missing_requested_idr_ends_generation() {
    for scenario in [Scenario::EmptyOutput, Scenario::NonIdr] {
        let mut engine = Engine::new(FakeDriver::new(scenario), config()).unwrap();
        assert_eq!(engine.encode(frame(0, &[0; 24])), Err(Error::backend(Phase::Encode, Fault::InvalidOutput)));
        assert_eq!(engine.encode(frame(1, &[0; 24])), Err(Error::Closed));
    }
}

#[test]
fn fail_after_n_retains_driver_status_and_prevents_more_submissions() {
    let driver = FakeDriver::new(Scenario::FailAfter(2));
    let events = driver.events.clone();
    let mut engine = Engine::new(driver, config()).unwrap();
    for sequence in 0..2 { engine.encode(frame(sequence, &[0; 24])).unwrap(); }
    assert_eq!(engine.encode(frame(2, &[0; 24])), Err(Error::Backend { phase: Phase::Encode, fault: Fault::Driver, code: -7 }));
    assert_eq!(engine.encode(frame(2, &[0; 24])), Err(Error::Closed));
    assert_eq!(events.lock().unwrap().iter().filter(|event| event.starts_with("submit:")).count(), 3);
}

#[test]
fn policy_matrix_never_bypasses_quarantine_or_strict_selection() {
    for class in [DeviceClass::Nvidia, DeviceClass::Vaapi, DeviceClass::Unknown] {
        assert_eq!(plan(BackendMode::Software, class, false), Ok(Plan::Software));
        assert_eq!(plan(BackendMode::Software, class, true), Ok(Plan::Software));
        assert_eq!(plan(BackendMode::Auto, class, true), Ok(Plan::Software));
        for mode in [BackendMode::Nvenc, BackendMode::Vaapi] { assert_eq!(plan(mode, class, true), Err(Error::Quarantined)); }
    }
    assert_eq!(plan(BackendMode::Auto, DeviceClass::Nvidia, false), Ok(Plan::Nvenc { software_fallback: true }));
    assert_eq!(plan(BackendMode::Auto, DeviceClass::Vaapi, false), Ok(Plan::Vaapi { software_fallback: true }));
    assert_eq!(plan(BackendMode::Auto, DeviceClass::Unknown, false), Ok(Plan::Software));
    let failure = Error::backend(Phase::Probe, Fault::LibraryMissing);
    assert!(!software_fallback_allowed(Plan::Nvenc { software_fallback: false }, failure, true));
    assert!(!software_fallback_allowed(Plan::Nvenc { software_fallback: true }, failure, false));
    assert!(software_fallback_allowed(Plan::Nvenc { software_fallback: true }, failure, true));
    assert!(!software_fallback_allowed(Plan::Nvenc { software_fallback: true }, Error::Protocol, true));
}

#[test]
fn protocol_rejects_bad_versions_trailing_bytes_and_oversized_prefixes() {
    assert!(protocol::decode(b"HNV2\0").is_err());
    assert!(protocol::decode(b"HNV1\0extra").is_err());
    for length in [0, 4, u32::MAX] {
        let bytes = (length as u32).to_le_bytes();
        assert!(protocol::read(&mut &bytes[..], protocol::OVERHEAD).is_err());
    }
    let packet = protocol::encode(&protocol::Message::Init(config())).unwrap();
    match protocol::read(&mut &packet[..], protocol::OVERHEAD).unwrap() {
        protocol::Message::Init(observed) => assert_eq!(observed, config()),
        _ => panic!("expected Init"),
    }
}

proptest::proptest! {
    #[test]
    fn malformed_protocol_does_not_panic(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..256)) {
        let _ = protocol::decode(&bytes);
    }
}
