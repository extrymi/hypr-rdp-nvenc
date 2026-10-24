use std::{ffi::c_void, marker::PhantomData, ptr, rc::Rc};

use super::{ffi::*, guids, loader::Api};
use crate::nvenc::{
    annexb::AnnexB, AccessUnit, Config, Driver, Error, Fault, Frame, Phase, Stamp, Submission,
    BUFFER_COUNT, MAX_OUTPUT_BYTES,
};

#[derive(Default)]
struct Slot {
    input: NV_ENC_INPUT_PTR,
    output: NV_ENC_OUTPUT_PTR,
    pending: Option<(Stamp, bool)>,
}

pub(super) struct NativeDriver {
    api: Option<Api>,
    context: CUcontext,
    session: *mut c_void,
    config: Option<Config>,
    initialized: bool,
    eos_sent: bool,
    slots: [Slot; BUFFER_COUNT],
    annexb: AnnexB,
    // CUDA current-context state belongs to the worker's one OS thread.
    _same_thread: PhantomData<Rc<()>>,
}

fn nv(status: NVENCSTATUS, phase: Phase) -> Result<(), Error> {
    if status == NV_ENC_SUCCESS {
        Ok(())
    } else {
        Err(Error::Backend {
            phase,
            fault: Fault::Driver,
            code: status as i32,
        })
    }
}
fn cu(status: CUresult) -> Result<(), Error> {
    if status == CUDA_SUCCESS {
        Ok(())
    } else {
        Err(Error::Backend {
            phase: Phase::Probe,
            fault: Fault::DeviceUnavailable,
            code: status as i32,
        })
    }
}
fn unsupported() -> Error {
    Error::backend(Phase::Probe, Fault::CapabilityUnsupported)
}
fn invalid_output() -> Error {
    Error::backend(Phase::Encode, Fault::InvalidOutput)
}

fn same_guid(a: GUID, b: GUID) -> bool {
    a.Data1 == b.Data1 && a.Data2 == b.Data2 && a.Data3 == b.Data3 && a.Data4 == b.Data4
}

impl NativeDriver {
    pub fn unloaded() -> Self {
        Self {
            api: None,
            context: ptr::null_mut(),
            session: ptr::null_mut(),
            config: None,
            initialized: false,
            eos_sent: false,
            slots: std::array::from_fn(|_| Slot::default()),
            annexb: AnnexB::default(),
            _same_thread: PhantomData,
        }
    }

    #[cfg(test)]
    pub(super) fn with_api(api: Api) -> Self {
        let mut driver = Self::unloaded();
        driver.api = Some(api);
        driver
    }

    fn api(&self) -> &Api {
        self.api
            .as_ref()
            .expect("probe loads and checks the API first")
    }

    fn cap(&self, capability: NV_ENC_CAPS) -> Result<i32, Error> {
        let mut params = NV_ENC_CAPS_PARAM {
            version: HYPR_NV_ENC_CAPS_PARAM_VER,
            capsToQuery: capability,
            ..Default::default()
        };
        let mut value = 0;
        // SAFETY: checked function table, live session and writable C structs.
        nv(
            unsafe {
                self.api().enc.nvEncGetEncodeCaps.unwrap()(
                    self.session,
                    guids::h264(),
                    &mut params,
                    &mut value,
                )
            },
            Phase::Probe,
        )?;
        Ok(value)
    }

    fn capabilities(&self, config: &Config) -> Result<(), Error> {
        // Bound every count/output supplied by the driver before allocating.
        let mut count = 0;
        let mut guids = [GUID::default(); 64];
        // SAFETY: all array lengths match the supplied capacities. The function
        // table was checked before opening the session and remains immutable.
        unsafe {
            nv(
                self.api().enc.nvEncGetEncodeGUIDCount.unwrap()(self.session, &mut count),
                Phase::Probe,
            )?;
            if count == 0 || count > guids.len() as u32 {
                return Err(unsupported());
            }
            let capacity = count;
            nv(
                self.api().enc.nvEncGetEncodeGUIDs.unwrap()(
                    self.session,
                    guids.as_mut_ptr(),
                    capacity,
                    &mut count,
                ),
                Phase::Probe,
            )?;
            if count > capacity
                || !guids[..count as usize]
                    .iter()
                    .any(|g| same_guid(*g, guids::h264()))
            {
                return Err(unsupported());
            }
            nv(
                self.api().enc.nvEncGetEncodeProfileGUIDCount.unwrap()(
                    self.session,
                    guids::h264(),
                    &mut count,
                ),
                Phase::Probe,
            )?;
            if count == 0 || count > guids.len() as u32 {
                return Err(unsupported());
            }
            let capacity = count;
            nv(
                self.api().enc.nvEncGetEncodeProfileGUIDs.unwrap()(
                    self.session,
                    guids::h264(),
                    guids.as_mut_ptr(),
                    capacity,
                    &mut count,
                ),
                Phase::Probe,
            )?;
            if count > capacity
                || !guids[..count as usize]
                    .iter()
                    .any(|g| same_guid(*g, guids::profile()))
            {
                return Err(unsupported());
            }
            nv(
                self.api().enc.nvEncGetEncodePresetCount.unwrap()(
                    self.session,
                    guids::h264(),
                    &mut count,
                ),
                Phase::Probe,
            )?;
            if count == 0 || count > guids.len() as u32 {
                return Err(unsupported());
            }
            let capacity = count;
            nv(
                self.api().enc.nvEncGetEncodePresetGUIDs.unwrap()(
                    self.session,
                    guids::h264(),
                    guids.as_mut_ptr(),
                    capacity,
                    &mut count,
                ),
                Phase::Probe,
            )?;
            if count > capacity
                || !guids[..count as usize]
                    .iter()
                    .any(|g| same_guid(*g, guids::preset()))
            {
                return Err(unsupported());
            }
            let mut formats = [0; 64];
            nv(
                self.api().enc.nvEncGetInputFormatCount.unwrap()(
                    self.session,
                    guids::h264(),
                    &mut count,
                ),
                Phase::Probe,
            )?;
            if count == 0 || count > formats.len() as u32 {
                return Err(unsupported());
            }
            let capacity = count;
            nv(
                self.api().enc.nvEncGetInputFormats.unwrap()(
                    self.session,
                    guids::h264(),
                    formats.as_mut_ptr(),
                    capacity,
                    &mut count,
                ),
                Phase::Probe,
            )?;
            if count > capacity || !formats[..count as usize].contains(&NV_ENC_BUFFER_FORMAT_NV12) {
                return Err(unsupported());
            }
        }
        for (value, low, high) in [
            (config.width, NV_ENC_CAPS_WIDTH_MIN, NV_ENC_CAPS_WIDTH_MAX),
            (
                config.height,
                NV_ENC_CAPS_HEIGHT_MIN,
                NV_ENC_CAPS_HEIGHT_MAX,
            ),
        ] {
            let (min, max) = (self.cap(low)?, self.cap(high)?);
            if min < 0 || max <= 0 || value < min as u32 || value > max as u32 {
                return Err(unsupported());
            }
        }
        let mb = (config.width as u64).div_ceil(16) * (config.height as u64).div_ceil(16);
        let max_mb = self.cap(NV_ENC_CAPS_MB_NUM_MAX)?;
        let max_rate = self.cap(NV_ENC_CAPS_MB_PER_SEC_MAX)?;
        if max_mb <= 0
            || max_rate <= 0
            || mb > max_mb as u64
            || mb * config.fps as u64 > max_rate as u64
            || self.cap(NV_ENC_CAPS_SUPPORT_CABAC)? == 0
            || self.cap(NV_ENC_CAPS_SUPPORTED_RATECONTROL_MODES)? & (1 << NV_ENC_PARAMS_RC_CBR) == 0
        {
            return Err(unsupported());
        }
        Ok(())
    }

    /// EOS is attempted once even during failed-generation cleanup. A blocking
    /// driver call remains inside the worker and its parent's destroy deadline.
    fn flush(&mut self, phase: Phase) -> Result<(), Error> {
        if !self.initialized || self.eos_sent {
            return Ok(());
        }
        self.eos_sent = true;
        let mut params = NV_ENC_PIC_PARAMS {
            version: HYPR_NV_ENC_PIC_PARAMS_VER,
            encodePicFlags: NV_ENC_PIC_FLAG_EOS,
            ..Default::default()
        };
        // SAFETY: live initialized session, no frame pointers required for EOS.
        nv(
            unsafe { self.api().enc.nvEncEncodePicture.unwrap()(self.session, &mut params) },
            phase,
        )
    }
}

impl Driver for NativeDriver {
    fn probe(&mut self, config: &Config) -> Result<(), Error> {
        config.frame_bytes()?;
        if self.config.is_some() {
            return Err(Error::Closed);
        }
        if self.api.is_none() {
            self.api = Some(Api::load()?);
        }
        // SAFETY: generated CUDA signatures, checked symbols and valid output
        // addresses. Context and session handles are saved before status checks
        // so partial acquisition is covered by NativeDriver's Drop as well.
        unsafe {
            cu(self.api().cuda.init.unwrap()(0))?;
            let mut count = 0;
            cu(self.api().cuda.count.unwrap()(&mut count))?;
            if !(1..=128).contains(&count) {
                return Err(Error::backend(Phase::Probe, Fault::DeviceUnavailable));
            }
            let mut selected = None;
            for ordinal in 0..count {
                let mut device = 0;
                cu(self.api().cuda.device.unwrap()(&mut device, ordinal))?;
                let mut uuid = CUuuid::default();
                cu(self.api().cuda.uuid.unwrap()(&mut uuid, device))?;
                let bytes: Vec<String> = uuid
                    .bytes
                    .iter()
                    .map(|b| format!("{:02x}", *b as u8))
                    .collect();
                let id = format!(
                    "GPU-{}-{}-{}-{}-{}",
                    bytes[..4].concat(),
                    bytes[4..6].concat(),
                    bytes[6..8].concat(),
                    bytes[8..10].concat(),
                    bytes[10..].concat()
                );
                if id == config.device.to_string() && selected.replace(device).is_some() {
                    return Err(Error::backend(Phase::Probe, Fault::DeviceUnavailable));
                }
            }
            let device =
                selected.ok_or_else(|| Error::backend(Phase::Probe, Fault::DeviceUnavailable))?;
            let result = self.api().cuda.create.unwrap()(
                &mut self.context,
                CU_CTX_SCHED_BLOCKING_SYNC,
                device,
            );
            cu(result)?;
            if self.context.is_null() {
                return Err(Error::backend(Phase::Probe, Fault::DeviceUnavailable));
            }
            let mut open = NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS {
                version: HYPR_NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER,
                deviceType: NV_ENC_DEVICE_TYPE_CUDA,
                device: self.context.cast(),
                apiVersion: NVENCAPI_VERSION,
                ..Default::default()
            };
            let result =
                self.api().enc.nvEncOpenEncodeSessionEx.unwrap()(&mut open, &mut self.session);
            nv(result, Phase::Probe)?;
            if self.session.is_null() {
                return Err(unsupported());
            }
        }
        self.capabilities(config)?;
        self.config = Some(config.clone());
        Ok(())
    }

    fn initialize(&mut self, config: &Config) -> Result<(), Error> {
        if self.initialized || self.config.as_ref() != Some(config) {
            return Err(Error::Closed);
        }
        let mut preset = NV_ENC_PRESET_CONFIG {
            version: HYPR_NV_ENC_PRESET_CONFIG_VER,
            ..Default::default()
        };
        preset.presetCfg.version = HYPR_NV_ENC_CONFIG_VER;
        // SAFETY: initialized C structs and checked preset/session combination.
        nv(
            unsafe {
                self.api().enc.nvEncGetEncodePresetConfigEx.unwrap()(
                    self.session,
                    guids::h264(),
                    guids::preset(),
                    NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY,
                    &mut preset,
                )
            },
            Phase::Initialize,
        )?;
        let mut codec = preset.presetCfg;
        codec.version = HYPR_NV_ENC_CONFIG_VER;
        codec.profileGUID = guids::profile();
        codec.gopLength = config.fps * 2;
        codec.frameIntervalP = 1;
        codec.frameFieldMode = NV_ENC_PARAMS_FRAME_FIELD_MODE_FRAME;
        codec.rcParams.version = HYPR_NV_ENC_RC_PARAMS_VER;
        codec.rcParams.rateControlMode = NV_ENC_PARAMS_RC_CBR;
        codec.rcParams.averageBitRate = config.bitrate;
        codec.rcParams.maxBitRate = config.bitrate;
        codec.rcParams.vbvBufferSize = config.bitrate.div_ceil(config.fps);
        codec.rcParams.vbvInitialDelay = codec.rcParams.vbvBufferSize;
        codec.rcParams.set_enableLookahead(0);
        codec.rcParams.set_enableExtLookahead(0);
        codec.rcParams.lookaheadDepth = 0;
        codec.rcParams.set_zeroReorderDelay(1);
        codec.rcParams.set_enableAQ(0);
        codec.rcParams.set_enableTemporalAQ(0);
        // SAFETY: the selected codec is H.264, so this is the active union field.
        let h264 = unsafe { &mut codec.encodeCodecConfig.h264Config };
        h264.idrPeriod = codec.gopLength;
        h264.set_repeatSPSPPS(1);
        h264.set_disableSPSPPS(0);
        h264.set_outputAUD(1);
        h264.set_enableTemporalSVC(0);
        h264.set_enableStereoMVC(0);
        h264.set_hierarchicalPFrames(0);
        h264.set_hierarchicalBFrames(0);
        h264.set_enableLTR(0);
        h264.set_enableIntraRefresh(0);
        h264.tfLevel = NV_ENC_TEMPORAL_FILTER_LEVEL_0;
        h264.chromaFormatIDC = 1;
        h264.inputBitDepth = NV_ENC_BIT_DEPTH_8;
        h264.outputBitDepth = NV_ENC_BIT_DEPTH_8;
        h264.entropyCodingMode = NV_ENC_H264_ENTROPY_CODING_MODE_CABAC;
        h264.h264VUIParameters.videoSignalTypePresentFlag = 1;
        h264.h264VUIParameters.videoFormat = 5;
        h264.h264VUIParameters.videoFullRangeFlag = 1;
        h264.h264VUIParameters.colourDescriptionPresentFlag = 1;
        h264.h264VUIParameters.colourPrimaries = 1;
        h264.h264VUIParameters.transferCharacteristics = 1;
        h264.h264VUIParameters.colourMatrix = 1;
        let mut params = NV_ENC_INITIALIZE_PARAMS {
            version: HYPR_NV_ENC_INITIALIZE_PARAMS_VER,
            encodeGUID: guids::h264(),
            presetGUID: guids::preset(),
            encodeWidth: config.width,
            encodeHeight: config.height,
            darWidth: config.width,
            darHeight: config.height,
            frameRateNum: config.fps,
            frameRateDen: 1,
            enableEncodeAsync: 0,
            enablePTD: 1,
            encodeConfig: &mut codec,
            tuningInfo: NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY,
            maxEncodeWidth: config.width,
            maxEncodeHeight: config.height,
            ..Default::default()
        };
        // SAFETY: codec is live for the synchronous initialization call. No Rust
        // address is retained by this adapter after the driver copies settings.
        nv(
            unsafe { self.api().enc.nvEncInitializeEncoder.unwrap()(self.session, &mut params) },
            Phase::Initialize,
        )?;
        self.initialized = true;
        Ok(())
    }

    fn allocate(&mut self, slot: usize) -> Result<(), Error> {
        let config = self.config.as_ref().ok_or(Error::Closed)?;
        if !self.initialized || slot >= BUFFER_COUNT || !self.slots[slot].input.is_null() {
            return Err(Error::Closed);
        }
        let mut input = NV_ENC_CREATE_INPUT_BUFFER {
            version: HYPR_NV_ENC_CREATE_INPUT_BUFFER_VER,
            width: config.width,
            height: config.height,
            bufferFmt: NV_ENC_BUFFER_FORMAT_NV12,
            ..Default::default()
        };
        // SAFETY: checked API and immutable generation dimensions.
        let result =
            unsafe { self.api().enc.nvEncCreateInputBuffer.unwrap()(self.session, &mut input) };
        self.slots[slot].input = input.inputBuffer;
        if let Err(error) = nv(result, Phase::Initialize) {
            let _ = self.release(slot);
            return Err(error);
        }
        if input.inputBuffer.is_null() {
            return Err(Error::backend(Phase::Initialize, Fault::Driver));
        }
        let mut output = NV_ENC_CREATE_BITSTREAM_BUFFER {
            version: HYPR_NV_ENC_CREATE_BITSTREAM_BUFFER_VER,
            ..Default::default()
        };
        // SAFETY: output pointer refers to the writable generated struct.
        let result = unsafe {
            self.api().enc.nvEncCreateBitstreamBuffer.unwrap()(self.session, &mut output)
        };
        self.slots[slot].output = output.bitstreamBuffer;
        if let Err(error) = nv(result, Phase::Initialize) {
            let _ = self.release(slot);
            return Err(error);
        }
        if output.bitstreamBuffer.is_null() {
            let _ = self.release(slot);
            return Err(Error::backend(Phase::Initialize, Fault::Driver));
        }
        Ok(())
    }

    fn submit(&mut self, slot: usize, frame: &Frame<'_>) -> Result<Submission, Error> {
        let config = self.config.as_ref().ok_or(Error::Closed)?;
        if !self.initialized
            || self.eos_sent
            || slot >= BUFFER_COUNT
            || self.slots[slot].input.is_null()
            || self.slots[slot].output.is_null()
            || self.slots.iter().any(|s| s.pending.is_some())
        {
            return Err(Error::Closed);
        }
        if frame.stamp.epoch != config.epoch || frame.nv12.len() != config.frame_bytes()? {
            return Err(Error::InvalidFrame);
        }
        let (width, height) = (config.width as usize, config.height as usize);
        let mut lock = NV_ENC_LOCK_INPUT_BUFFER {
            version: HYPR_NV_ENC_LOCK_INPUT_BUFFER_VER,
            inputBuffer: self.slots[slot].input,
            ..Default::default()
        };
        lock.set_doNotWait(1);
        // SAFETY: live input handle. On success NVENC owns a writable mapping
        // until one matching unlock; no Rust slice outlives that lock.
        let result =
            unsafe { self.api().enc.nvEncLockInputBuffer.unwrap()(self.session, &mut lock) };
        if result == NV_ENC_ERR_ENCODER_BUSY || result == NV_ENC_ERR_LOCK_BUSY {
            return Ok(Submission::Busy);
        }
        nv(result, Phase::Encode)?;
        let copy =
            if lock.bufferDataPtr.is_null() || (lock.pitch as usize) < width || lock.pitch > 131072
            {
                Err(invalid_output())
            } else {
                // NV12 has Y rows followed by interleaved UV rows, both with the
                // driver-provided pitch. Padding is never read from the source.
                for row in 0..height + height / 2 {
                    // SAFETY: the checked dimensions fit the input allocation;
                    // NVENC's lock contract provides pitch * (height * 3/2) bytes.
                    unsafe {
                        ptr::copy_nonoverlapping(
                            frame.nv12.as_ptr().add(row * width),
                            lock.bufferDataPtr
                                .cast::<u8>()
                                .add(row * lock.pitch as usize),
                            width,
                        )
                    };
                }
                Ok(())
            };
        // SAFETY: this is the sole unlock attempt for the successful lock above.
        let unlock = nv(
            unsafe {
                self.api().enc.nvEncUnlockInputBuffer.unwrap()(self.session, lock.inputBuffer)
            },
            Phase::Encode,
        );
        copy?;
        unlock?;
        let mut params = NV_ENC_PIC_PARAMS {
            version: HYPR_NV_ENC_PIC_PARAMS_VER,
            inputWidth: width as u32,
            inputHeight: height as u32,
            inputPitch: lock.pitch,
            inputBuffer: self.slots[slot].input,
            outputBitstream: self.slots[slot].output,
            bufferFmt: NV_ENC_BUFFER_FORMAT_NV12,
            pictureStruct: NV_ENC_PIC_STRUCT_FRAME,
            inputTimeStamp: frame.stamp.sequence,
            inputDuration: 1,
            frameIdx: frame.stamp.sequence as u32,
            encodePicFlags: if frame.force_idr {
                NV_ENC_PIC_FLAG_FORCEIDR | NV_ENC_PIC_FLAG_OUTPUT_SPSPPS
            } else {
                0
            },
            ..Default::default()
        };
        // SAFETY: both buffers belong to this session and remain allocated until
        // output or terminal cleanup. No input is retried after acceptance.
        self.slots[slot].pending = Some((frame.stamp, frame.force_idr));
        let result =
            unsafe { self.api().enc.nvEncEncodePicture.unwrap()(self.session, &mut params) };
        if result == NV_ENC_ERR_ENCODER_BUSY {
            self.slots[slot].pending = None;
            return Ok(Submission::Busy);
        }
        if result == NV_ENC_ERR_NEED_MORE_INPUT {
            return Ok(Submission::Delayed);
        }
        nv(result, Phase::Encode)?;
        Ok(Submission::Ready)
    }

    fn output(&mut self, slot: usize) -> Result<AccessUnit, Error> {
        let config = self.config.as_ref().ok_or(Error::Closed)?;
        let (width, height) = (config.width, config.height);
        let (stamp, force_idr) = self
            .slots
            .get(slot)
            .and_then(|s| s.pending)
            .ok_or(Error::Closed)?;
        let mut lock = NV_ENC_LOCK_BITSTREAM {
            version: HYPR_NV_ENC_LOCK_BITSTREAM_VER,
            outputBitstream: self.slots[slot].output,
            ..Default::default()
        };
        // SAFETY: exactly one successfully accepted zero-delay frame is pending.
        // Synchronous retrieval can block only inside the supervised worker.
        nv(
            unsafe { self.api().enc.nvEncLockBitstream.unwrap()(self.session, &mut lock) },
            Phase::Encode,
        )?;
        let checked = if lock.bitstreamBufferPtr.is_null()
            || lock.bitstreamSizeInBytes == 0
            || lock.bitstreamSizeInBytes as usize > MAX_OUTPUT_BYTES
            || lock.hwEncodeStatus != 0
            || lock.outputTimeStamp != stamp.sequence
            || lock.frameIdx != stamp.sequence as u32
            || ![NV_ENC_PIC_TYPE_IDR, NV_ENC_PIC_TYPE_P, NV_ENC_PIC_TYPE_I]
                .contains(&lock.pictureType)
        {
            Err(invalid_output())
        } else {
            // SAFETY: locked output guarantees this bounded readable extent.
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    lock.bitstreamBufferPtr.cast::<u8>(),
                    lock.bitstreamSizeInBytes as usize,
                )
            };
            self.annexb.normalize(
                bytes,
                width,
                height,
                force_idr,
                lock.pictureType == NV_ENC_PIC_TYPE_IDR,
            )
        };
        // Always unlock even when bounds/bitstream/identity validation fails.
        // SAFETY: exactly one successful output lock is being matched.
        let unlocked = nv(
            unsafe {
                self.api().enc.nvEncUnlockBitstream.unwrap()(self.session, lock.outputBitstream)
            },
            Phase::Encode,
        );
        self.slots[slot].pending = None;
        let unit = checked?;
        unlocked?;
        Ok(unit)
    }

    fn drain(&mut self) -> Result<(), Error> {
        self.flush(Phase::Drain)
    }

    fn release(&mut self, slot: usize) -> Result<(), Error> {
        if slot >= BUFFER_COUNT {
            return Err(Error::Closed);
        }
        let mut failure = None;
        // The header's NvEncDestroyEncoder contract requires flushing before
        // freeing resources, even if the last frame was already retrieved or
        // initialization allocated no complete slot. Explicit drain shares the
        // same once-only guard; failed flushes are never retried during Drop.
        if let Err(error) = self.flush(Phase::Destroy) {
            failure = Some(error);
        }
        // Remove ownership before every foreign destroy call: an error must
        // never cause a second attempt from Engine or NativeDriver::drop.
        let owned = std::mem::take(&mut self.slots[slot]);
        if !owned.output.is_null() {
            // SAFETY: this slot exclusively owned the output handle.
            if let Err(error) = nv(
                unsafe {
                    self.api().enc.nvEncDestroyBitstreamBuffer.unwrap()(self.session, owned.output)
                },
                Phase::Destroy,
            ) {
                failure.get_or_insert(error);
            }
        }
        if !owned.input.is_null() {
            // SAFETY: this slot exclusively owned the input handle.
            if let Err(error) = nv(
                unsafe {
                    self.api().enc.nvEncDestroyInputBuffer.unwrap()(self.session, owned.input)
                },
                Phase::Destroy,
            ) {
                failure.get_or_insert(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }

    fn destroy(&mut self) -> Result<(), Error> {
        let mut failure = None;
        for slot in (0..BUFFER_COUNT).rev() {
            if let Err(error) = self.release(slot) {
                failure.get_or_insert(error);
            }
        }
        let session = std::mem::replace(&mut self.session, ptr::null_mut());
        if !session.is_null() {
            // SAFETY: all owned buffers have had one release attempt. Session
            // destroy is attempted once; process exit is the final boundary.
            if let Err(error) = nv(
                unsafe { self.api().enc.nvEncDestroyEncoder.unwrap()(session) },
                Phase::Destroy,
            ) {
                failure.get_or_insert(error);
            }
        }
        let context = std::mem::replace(&mut self.context, ptr::null_mut());
        if !context.is_null() {
            // SAFETY: dedicated same-thread CUDA context; encoder is destroyed
            // first and both providing libraries are still owned by self.api.
            let status = unsafe { self.api().cuda.destroy.unwrap()(context) };
            if status != CUDA_SUCCESS {
                failure.get_or_insert(Error::Backend {
                    phase: Phase::Destroy,
                    fault: Fault::Driver,
                    code: status as i32,
                });
            }
        }
        self.initialized = false;
        failure.map_or(Ok(()), Err)
    }
}

impl Drop for NativeDriver {
    fn drop(&mut self) {
        let _ = self.destroy();
    }
}
