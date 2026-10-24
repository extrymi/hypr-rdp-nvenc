use std::{
    ffi::{c_char, CStr, CString},
    mem::{offset_of, size_of},
    path::Path,
};

use openh264::{
    decoder::Decoder,
    encoder::{Encoder, EncoderConfig},
    formats::{YUVBuffer, YUVSource},
    OpenH264API,
};

use super::{driver::NativeDriver, ffi::*, loader::Api};
use crate::nvenc::{
    annexb::{split, AnnexB},
    Config, Driver, Engine, Error, Fault, Frame, Phase, Stamp,
};

extern "C" {
    fn hypr_mock_reset(mode: i32, failure: *const c_char, nth: u32);
    fn hypr_mock_api(cuda: *mut HYPR_CUDA_FUNCTIONS, enc: *mut NV_ENCODE_API_FUNCTION_LIST);
    fn hypr_mock_packet(data: *const u8, len: u32);
    fn hypr_mock_trace() -> *const c_char;
    fn hypr_mock_resources() -> i32;
    fn hypr_mock_selected() -> i32;
    fn hypr_c_sizeof_config() -> usize;
    fn hypr_c_sizeof_pic() -> usize;
    fn hypr_c_sizeof_api() -> usize;
    fn hypr_c_offset_output() -> usize;
    fn hypr_c_bitfield(rc: *mut NV_ENC_RC_PARAMS) -> u32;
}

// Every mock state is C _Thread_local. Tests run concurrently without changing
// process environment, loading system drivers or sharing native handles.
struct Fixture;
impl Fixture {
    fn new(mode: i32, failure: &str, nth: u32) -> Self {
        let failure = CString::new(failure).unwrap();
        // SAFETY: C copies the string and resets only this test thread's state.
        unsafe { hypr_mock_reset(mode, failure.as_ptr(), nth) };
        Self
    }
    fn api(&self) -> Api {
        let (mut cuda, mut enc) = (
            HYPR_CUDA_FUNCTIONS::default(),
            NV_ENCODE_API_FUNCTION_LIST::default(),
        );
        // SAFETY: C writes its independently compiled function-pointer tables.
        unsafe { hypr_mock_api(&mut cuda, &mut enc) };
        Api::checked(cuda, enc, None).unwrap()
    }
    fn driver(&self) -> NativeDriver {
        NativeDriver::with_api(self.api())
    }
    fn trace(&self) -> String {
        // SAFETY: fixture returns a NUL-terminated thread-local trace string.
        unsafe { CStr::from_ptr(hypr_mock_trace()) }
            .to_string_lossy()
            .into_owned()
    }
    fn packet(&self, bytes: &[u8]) {
        // SAFETY: C copies this slice synchronously and owns its copy.
        unsafe { hypr_mock_packet(bytes.as_ptr(), bytes.len() as u32) };
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // SAFETY: fixture state is confined to this thread and drivers have been
        // dropped before their fixture by ordinary reverse local drop order.
        let remaining = unsafe { hypr_mock_resources() };
        if !std::thread::panicking() {
            assert_eq!(remaining, 0, "{}", self.trace());
            assert!(
                !self.trace().contains("destroy-before-eos"),
                "{}",
                self.trace()
            );
        }
        unsafe { hypr_mock_reset(0, c"".as_ptr(), 0) };
    }
}

fn config() -> Config {
    Config {
        device: "GPU-01234567-89ab-cdef-0123-456789abcdef".parse().unwrap(),
        epoch: 23,
        width: 64,
        height: 64,
        fps: 30,
        bitrate: 4_000_000,
    }
}
fn frame(bytes: &[u8], sequence: u64, force_idr: bool) -> Frame<'_> {
    Frame {
        stamp: Stamp {
            epoch: 23,
            sequence,
        },
        force_idr,
        nv12: bytes,
    }
}
fn packets() -> Vec<Vec<u8>> {
    let yuv = YUVBuffer::new(64, 64);
    let mut encoder =
        Encoder::with_api_config(OpenH264API::from_source(), EncoderConfig::new()).unwrap();
    (0..3)
        .map(|_| encoder.encode(&yuv).unwrap().to_vec())
        .collect()
}

#[test]
fn generated_rust_layout_and_bitfields_match_independent_c_compilation() {
    let mut rc = NV_ENC_RC_PARAMS::default();
    rc.set_zeroReorderDelay(1);
    // SAFETY: C sees the generated Rust layout of the pinned header structs.
    unsafe {
        assert_eq!(size_of::<NV_ENC_CONFIG>(), hypr_c_sizeof_config());
        assert_eq!(size_of::<NV_ENC_PIC_PARAMS>(), hypr_c_sizeof_pic());
        assert_eq!(
            size_of::<NV_ENCODE_API_FUNCTION_LIST>(),
            hypr_c_sizeof_api()
        );
        assert_eq!(
            offset_of!(NV_ENC_LOCK_BITSTREAM, bitstreamBufferPtr),
            hypr_c_offset_output()
        );
        assert_eq!(hypr_c_bitfield(&mut rc), 1);
    }
    assert_eq!(rc.enableAQ(), 1);
}

#[test]
fn lazy_construction_does_not_load_or_initialize_a_driver() {
    let fixture = Fixture::new(0, "", 0);
    drop(NativeDriver::unloaded());
    assert!(fixture.trace().is_empty());
}

#[test]
fn uuid_selection_pitch_copy_settings_metadata_and_decode_round_trip() {
    let fixture = Fixture::new(0, "", 0);
    let mut engine = Engine::new(fixture.driver(), config()).unwrap();
    // Device 0 intentionally has the wrong UUID. Resolve device 11 at ordinal1.
    assert_eq!(unsafe { hypr_mock_selected() }, 11);
    let input = vec![0x42; config().frame_bytes().unwrap()];
    let mut decoder = Decoder::new().unwrap();
    for (sequence, packet) in packets().into_iter().enumerate() {
        fixture.packet(&packet);
        let output = engine
            .encode(frame(&input, sequence as u64, false))
            .unwrap();
        assert_eq!(output.stamp.sequence, sequence as u64);
        let picture = decoder
            .decode(&output.unit.bytes)
            .unwrap()
            .expect("one decoded picture per access unit");
        assert_eq!(picture.dimensions(), (64, 64));
        let kinds: Vec<_> = split(&output.unit.bytes)
            .unwrap()
            .iter()
            .map(|n| n[0] & 31)
            .collect();
        assert!(kinds.contains(&7) && kinds.contains(&8));
        assert_eq!(output.unit.is_idr, sequence == 0);
    }
    engine.drain().unwrap();
    engine.close().unwrap();
    drop(engine);
    let trace = fixture.trace();
    assert_eq!(trace.lines().filter(|l| *l == "eos").count(), 1);
    assert_eq!(trace.lines().filter(|l| *l == "destroy-input").count(), 4);
    assert_eq!(trace.lines().filter(|l| *l == "destroy-output").count(), 4);
    assert!(trace.find("close").unwrap() < trace.find("cu-destroy").unwrap());
    assert!(
        !trace.contains("bad-") && !trace.contains("destroy-locked"),
        "{trace}"
    );
}

#[test]
fn acquisition_failure_at_each_native_stage_releases_partial_ownership() {
    for (call, max_n) in [
        ("cu-init", 1),
        ("cu-count", 1),
        ("cu-device", 2),
        ("cu-uuid", 2),
        ("cu-create", 1),
        ("open", 1),
        ("codec-count", 1),
        ("codecs", 1),
        ("specific-count", 3),
        ("profiles", 1),
        ("presets", 1),
        ("formats", 1),
        ("caps", 8),
        ("preset", 1),
        ("initialize", 1),
        ("create-input", 4),
        ("create-output", 4),
    ] {
        for nth in 1..=max_n {
            let fixture = Fixture::new(0, call, nth);
            let result = Engine::new(fixture.driver(), config());
            assert!(result.is_err(), "{call}/{nth}: {}", fixture.trace());
            drop(result);
            assert_eq!(
                unsafe { hypr_mock_resources() },
                0,
                "{call}/{nth}: {}",
                fixture.trace()
            );
        }
    }
}

#[test]
fn unsupported_formats_unbounded_counts_and_null_success_handles_fail_closed() {
    for mode in [1, 9, 11] {
        let fixture = Fixture::new(mode, "", 0);
        assert!(Engine::new(fixture.driver(), config()).is_err());
    }
    let fixture = Fixture::new(0, "", 0);
    let mut cfg = config();
    cfg.device = "GPU-ffffffff-ffff-ffff-ffff-ffffffffffff".parse().unwrap();
    assert!(matches!(
        Engine::new(fixture.driver(), cfg),
        Err(Error::Backend {
            fault: Fault::DeviceUnavailable,
            ..
        })
    ));
    assert!(!fixture.trace().contains("cu-create"));
}

#[test]
fn output_failures_always_unlock_and_never_forward_invalid_or_stale_data() {
    for mode in [2, 3, 4, 5, 8] {
        let fixture = Fixture::new(mode, "", 0);
        fixture.packet(&packets()[0]);
        let mut engine = Engine::new(fixture.driver(), config()).unwrap();
        assert!(engine
            .encode(frame(
                &vec![0x42; config().frame_bytes().unwrap()],
                0,
                false
            ))
            .is_err());
        drop(engine);
        let trace = fixture.trace();
        let lock = if mode == 5 { "input" } else { "output" };
        assert!(trace.contains(&format!("unlock-{lock}")), "{trace}");
        assert!(!trace.contains("destroy-locked"), "{trace}");
    }
}

#[test]
fn delayed_input_is_not_resubmitted_or_locked_and_busy_retries_are_bounded() {
    for mode in [6, 7] {
        let fixture = Fixture::new(mode, "", 0);
        fixture.packet(&packets()[0]);
        let mut engine = Engine::new(fixture.driver(), config()).unwrap();
        let result = engine.encode(frame(
            &vec![0x42; config().frame_bytes().unwrap()],
            0,
            false,
        ));
        if mode == 6 {
            assert_eq!(
                result,
                Err(Error::backend(Phase::Encode, Fault::DelayedOutput))
            );
        } else {
            assert!(result.is_ok());
        }
        drop(engine);
        let trace = fixture.trace();
        assert_eq!(
            trace.lines().filter(|l| *l == "encode").count(),
            if mode == 6 { 1 } else { 3 }
        );
        assert_eq!(
            trace.lines().filter(|l| *l == "lock-output").count(),
            usize::from(mode == 7)
        );
        if mode == 6 {
            assert_eq!(trace.lines().filter(|l| *l == "eos").count(), 1);
        }
    }
}

#[test]
fn encode_errors_are_terminal_and_ambiguous_unlocks_and_eos_are_not_retried() {
    let packet = &packets()[0];
    let input = vec![0x42; config().frame_bytes().unwrap()];
    for call in [
        "lock-input",
        "unlock-input",
        "encode",
        "lock-output",
        "unlock-output",
    ] {
        let fixture = Fixture::new(0, call, 1);
        fixture.packet(packet);
        let mut engine = Engine::new(fixture.driver(), config()).unwrap();
        assert!(engine.encode(frame(&input, 0, false)).is_err(), "{call}");
        let after_failure = fixture.trace();
        assert_eq!(engine.encode(frame(&input, 0, false)), Err(Error::Closed));
        assert_eq!(fixture.trace(), after_failure);
        engine.close().unwrap();
        drop(engine);
        let trace = fixture.trace();
        assert_eq!(trace.lines().filter(|l| *l == call).count(), 1, "{trace}");
        assert_eq!(trace.lines().filter(|l| *l == "eos").count(), 1, "{trace}");
        assert!(!trace.contains("destroy-locked"), "{trace}");
        assert!(trace.find("eos").unwrap() < trace.find("destroy-output").unwrap());
    }
    let fixture = Fixture::new(0, "eos", 1);
    fixture.packet(packet);
    let mut engine = Engine::new(fixture.driver(), config()).unwrap();
    engine.encode(frame(&input, 0, false)).unwrap();
    assert!(engine.drain().is_err());
    assert_eq!(engine.drain(), Err(Error::Closed));
    engine.close().unwrap();
    drop(engine);
    assert_eq!(fixture.trace().lines().filter(|l| *l == "eos").count(), 1);
}

#[test]
fn release_errors_do_not_repeat_ambiguous_calls_or_skip_remaining_resources() {
    for (call, count) in [
        ("destroy-input", 4),
        ("destroy-output", 4),
        ("close", 1),
        ("cu-destroy", 1),
    ] {
        let fixture = Fixture::new(0, call, 1);
        let mut engine = Engine::new(fixture.driver(), config()).unwrap();
        assert!(engine.close().is_err());
        drop(engine);
        assert_eq!(
            fixture.trace().lines().filter(|l| *l == call).count(),
            count
        );
        assert!(fixture.trace().contains("cu-destroy"));
    }
}

#[test]
fn real_dlopen_checks_api_version_function_table_and_library_lifetime_without_gpu() {
    let path = Path::new(env!("HYPR_NVENC_TEST_SO"));
    // SAFETY: this path is built from our C fixture, never a system driver.
    let library = unsafe { libloading::Library::new(path) }.unwrap();
    type Reset = unsafe extern "C" fn(i32, *const c_char, u32);
    type Resources = unsafe extern "C" fn() -> i32;
    let reset = unsafe { library.get::<Reset>(b"hypr_mock_reset\0") }.unwrap();
    let resources = unsafe { library.get::<Resources>(b"hypr_mock_resources\0") }.unwrap();
    for mode in [12, 13] {
        unsafe { reset(mode, c"".as_ptr(), 0) };
        assert!(matches!(
            Api::load_paths(path, path),
            Err(Error::Backend {
                fault: Fault::ApiUnsupported,
                ..
            })
        ));
        assert_eq!(unsafe { resources() }, 0);
    }
    unsafe { reset(0, c"".as_ptr(), 0) };
    let mut driver = NativeDriver::with_api(Api::load_paths(path, path).unwrap());
    driver.probe(&config()).unwrap();
    driver.initialize(&config()).unwrap();
    drop(driver);
    assert_eq!(unsafe { resources() }, 0);
    assert!(matches!(
        Api::missing_libraries(),
        Err(Error::Backend {
            fault: Fault::LibraryMissing,
            ..
        })
    ));
}

#[test]
fn annexb_rejects_bad_boundaries_parameter_sets_geometry_and_idr_mismatch() {
    let first = &packets()[0];
    for bad in [
        &[][..],
        &[0, 0, 1],
        &[0, 0, 1, 0x65, 0x80],
        &[1, 0, 0, 1, 0x65, 0x80],
    ] {
        assert!(AnnexB::default()
            .normalize(bad, 64, 64, true, true)
            .is_err());
    }
    assert!(AnnexB::default()
        .normalize(first, 66, 64, true, true)
        .is_err());
    assert!(AnnexB::default()
        .normalize(first, 64, 64, true, false)
        .is_err());
    let without_pps: Vec<u8> = split(first)
        .unwrap()
        .iter()
        .filter(|n| n[0] & 31 != 8)
        .flat_map(|n| [vec![0, 0, 0, 1], n.to_vec()].concat())
        .collect();
    assert!(AnnexB::default()
        .normalize(&without_pps, 64, 64, true, true)
        .is_err());
    assert!(AnnexB::default()
        .normalize(
            &[first.as_slice(), first.as_slice()].concat(),
            64,
            64,
            true,
            true
        )
        .is_err());
}

proptest::proptest! {
    #[test]
    fn bounded_random_annexb_never_panics(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..4096)) {
        let _ = AnnexB::default().normalize(&bytes, 64, 64, true, true);
    }
}
