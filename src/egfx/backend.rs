use anyhow::Result;

use super::avc444::{Avc444EncodedFrame, Avc444Encoder};
use super::frame::{EgfxFrameCodec, EncodedEgfxFrame};
use super::h264::H264Encoder;
#[cfg(feature = "vaapi")]
use super::vaapi;

#[cfg(test)]
pub(crate) type Avc444ReferenceRegions<'a> =
    (&'a [(i32, i32, i32, i32)], &'a [(i32, i32, i32, i32)]);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum H264RateControl {
    Vbr,
    Cqp,
    /// Fixed native NVENC mode; software fallback explicitly uses VBR.
    Cbr,
}

/// Which H.264 backend encoder creation is allowed to pick.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum H264BackendPolicy {
    /// Try VA-API, fall back to software (historical behavior).
    #[default]
    Auto,
    /// Never try VA-API.
    Software,
    /// Require VA-API; encoder creation fails if it is unavailable.
    Vaapi,
    /// Require native NVENC on this stable device UUID.
    Nvenc(hypr_rdp::nvenc::DeviceId),
    /// Prefer NVENC on an explicitly identified NVIDIA device.
    AutoNvenc(hypr_rdp::nvenc::DeviceId),
}

impl H264BackendPolicy {
    pub(crate) fn nvenc_device(self) -> Option<hypr_rdp::nvenc::DeviceId> {
        match self {
            Self::Nvenc(device) | Self::AutoNvenc(device) => Some(device),
            _ => None,
        }
    }

    pub(crate) fn software_rate(self, rate: H264RateControl) -> H264RateControl {
        if self.nvenc_device().is_some() {
            H264RateControl::Vbr
        } else {
            rate
        }
    }

    pub(crate) fn allows_vaapi(self) -> bool {
        matches!(self, Self::Auto | Self::Vaapi)
    }

    pub(crate) fn allows_software(self) -> bool {
        !matches!(self, Self::Vaapi | Self::Nvenc(_))
    }

    pub(crate) fn allows_runtime_software_fallback(self) -> bool {
        matches!(self, Self::Auto | Self::AutoNvenc(_))
    }
}

/// Encoder backend: hardware VAAPI, common H.264 software, or AVC444 software.
pub enum FrameEncoder {
    #[cfg(feature = "nvenc")]
    Nvenc(Box<super::nvenc::NvencEncoder>),
    #[cfg(feature = "vaapi")]
    Vaapi(Box<vaapi::VaapiEncoder>),
    #[cfg(test)]
    FailingVaapiForTest {
        force_idr_requests: u32,
    },
    Software(Box<H264Encoder>),
    SoftwareAvc444(Box<Avc444Encoder>),
}

impl FrameEncoder {
    pub(crate) fn native_software_fallback_allowed(&self, error: &anyhow::Error) -> bool {
        #[cfg(feature = "nvenc")]
        if matches!(self, Self::Nvenc(_)) {
            return super::nvenc::can_fallback(error);
        }
        let _ = error;
        false
    }

    pub(crate) fn is_nvenc(&self) -> bool {
        #[cfg(feature = "nvenc")]
        if matches!(self, Self::Nvenc(_)) {
            return true;
        }
        false
    }

    /// Create an encoder following the caller-supplied [`H264BackendPolicy`].
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        policy: H264BackendPolicy,
        width: u32,
        height: u32,
        bitrate: u32,
        fps: u32,
        quality: u8,
        rate_control: H264RateControl,
    ) -> Result<Self> {
        if let Some(device) = policy.nvenc_device() {
            anyhow::ensure!(rate_control == H264RateControl::Cbr, "NVENC requires CBR");
            #[cfg(feature = "nvenc")]
            match super::nvenc::NvencEncoder::new(device, width, height, bitrate, fps) {
                Ok(encoder) => return Ok(Self::Nvenc(Box::new(encoder))),
                Err(error)
                    if policy.allows_runtime_software_fallback()
                        && super::nvenc::can_fallback(&error) =>
                {
                    tracing::warn!(%error, "NVENC startup failed after confirmed exit; using software VBR");
                    return Self::new_software_only(
                        width,
                        height,
                        bitrate,
                        fps,
                        quality,
                        H264RateControl::Vbr,
                    );
                }
                Err(error) => return Err(error.context("NVENC initialization failed")),
            }
            #[cfg(not(feature = "nvenc"))]
            {
                let _ = device;
                anyhow::bail!("h264_backend=nvenc requested but built without the nvenc feature");
            }
        }
        anyhow::ensure!(
            rate_control != H264RateControl::Cbr,
            "CBR is currently supported only by NVENC"
        );
        #[cfg(feature = "vaapi")]
        if policy.allows_vaapi() {
            match vaapi::VaapiEncoder::new(width, height, bitrate, fps, quality, rate_control) {
                Ok(enc) => {
                    tracing::info!("Using VA-API hardware encoder");
                    return Ok(Self::Vaapi(Box::new(enc)));
                }
                Err(e) if !policy.allows_software() => {
                    return Err(e.context("h264_backend=vaapi requested but VA-API init failed"));
                }
                Err(e) => {
                    tracing::warn!("VA-API init failed, falling back to software: {:#}", e);
                }
            }
        }

        #[cfg(not(feature = "vaapi"))]
        if !policy.allows_software() {
            anyhow::bail!("h264_backend=vaapi requested but built without the vaapi feature");
        }

        let enc = H264Encoder::new(width, height, bitrate, fps, quality, rate_control)?;
        tracing::info!("Using OpenH264 software encoder");
        Ok(Self::Software(Box::new(enc)))
    }

    pub fn encode(&mut self, bgra: &[u8], stride: usize) -> Result<Vec<u8>> {
        match self {
            #[cfg(feature = "nvenc")]
            Self::Nvenc(enc) => enc.encode(bgra, stride),
            #[cfg(feature = "vaapi")]
            Self::Vaapi(enc) => enc.encode(bgra, stride),
            #[cfg(test)]
            Self::FailingVaapiForTest { .. } => anyhow::bail!("test VA-API encode failure"),
            Self::Software(enc) => enc.encode(bgra, stride),
            Self::SoftwareAvc444(_) => anyhow::bail!("AVC444 encoder requires encode_avc444"),
        }
    }

    pub(crate) fn encode_egfx_frame(
        &mut self,
        codec: EgfxFrameCodec,
        bgra: &[u8],
        stride: usize,
        candidate_regions: &[(i32, i32, i32, i32)],
    ) -> Result<EncodedEgfxFrame> {
        match codec {
            EgfxFrameCodec::Avc420 => self.encode(bgra, stride).map(EncodedEgfxFrame::Avc420),
            EgfxFrameCodec::Avc444 => self
                .encode_avc444(bgra, stride, candidate_regions)
                .map(EncodedEgfxFrame::Avc444),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn encode_capture_frame(
        &mut self,
        codec: EgfxFrameCodec,
        data: &[u8],
        stride: usize,
        regions: &[(i32, i32, i32, i32)],
        format: ironrdp_server::PixelFormat,
        dimensions: (u32, u32),
    ) -> Result<EncodedEgfxFrame> {
        let bgra = super::h264::canonical_bgra(data, dimensions, stride, format)?;
        self.encode_egfx_frame(codec, &bgra, stride, regions)
    }

    pub fn encode_avc444(
        &mut self,
        bgra: &[u8],
        stride: usize,
        candidate_regions: &[(i32, i32, i32, i32)],
    ) -> Result<Avc444EncodedFrame> {
        match self {
            Self::SoftwareAvc444(enc) => enc.encode(bgra, stride, candidate_regions),
            #[cfg(feature = "nvenc")]
            Self::Nvenc(_) => anyhow::bail!("NVENC supports only AVC420"),
            #[cfg(feature = "vaapi")]
            Self::Vaapi(_) => anyhow::bail!("AVC444 encoding requires software encoder"),
            #[cfg(test)]
            Self::FailingVaapiForTest { .. } => {
                anyhow::bail!("AVC444 encoding requires software encoder")
            }
            Self::Software(_) => anyhow::bail!("AVC444 encoding requires AVC444 encoder"),
        }
    }

    pub fn commit_avc444_reference(&mut self) {
        if let Self::SoftwareAvc444(enc) = self {
            enc.commit_reference();
        }
    }

    #[cfg(test)]
    pub(crate) fn avc444_luma_reference_y_for_test(&self) -> Option<&[u8]> {
        match self {
            Self::SoftwareAvc444(enc) => enc.luma_reference_y_for_test(),
            #[cfg(feature = "nvenc")]
            Self::Nvenc(_) => None,
            #[cfg(feature = "vaapi")]
            Self::Vaapi(_) | Self::Software(_) => None,
            Self::FailingVaapiForTest { .. } => None,
            #[cfg(not(feature = "vaapi"))]
            Self::Software(_) => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn avc444_last_reference_regions_for_test(
        &self,
    ) -> Option<Avc444ReferenceRegions<'_>> {
        match self {
            Self::SoftwareAvc444(enc) => Some(enc.last_reference_regions_for_test()),
            #[cfg(feature = "nvenc")]
            Self::Nvenc(_) => None,
            #[cfg(feature = "vaapi")]
            Self::Vaapi(_) | Self::Software(_) => None,
            Self::FailingVaapiForTest { .. } => None,
            #[cfg(not(feature = "vaapi"))]
            Self::Software(_) => None,
        }
    }

    /// Encode from an NV12 DMA-BUF (zero-copy path). Only available with VA-API backend.
    #[cfg(feature = "vaapi")]
    #[allow(clippy::too_many_arguments)]
    pub fn encode_dmabuf(
        &mut self,
        nv12_fd: std::os::unix::io::RawFd,
        width: u32,
        height: u32,
        stride: u32,
        offset: u32,
        modifier: u64,
        uv_stride: u32,
        uv_offset: u32,
    ) -> Result<Vec<u8>> {
        match self {
            Self::Vaapi(enc) => enc.encode_dmabuf(
                nv12_fd, width, height, stride, offset, modifier, uv_stride, uv_offset,
            ),
            #[cfg(test)]
            Self::FailingVaapiForTest { .. } => anyhow::bail!("test VA-API DMA-BUF failure"),
            #[cfg(feature = "nvenc")]
            Self::Nvenc(_) => anyhow::bail!("NVENC requires CPU NV12 input"),
            Self::Software(_) => anyhow::bail!("DMA-BUF encode requires VA-API backend"),
            Self::SoftwareAvc444(_) => anyhow::bail!("DMA-BUF encode requires VA-API backend"),
        }
    }

    pub fn backend_name(&self) -> &'static str {
        match self {
            #[cfg(feature = "vaapi")]
            Self::Vaapi(_) => "vaapi",
            #[cfg(feature = "nvenc")]
            Self::Nvenc(_) => "nvenc",
            #[cfg(test)]
            Self::FailingVaapiForTest { .. } => "vaapi-test-failing",
            Self::Software(_) => "openh264",
            Self::SoftwareAvc444(enc) => enc.backend_name(),
        }
    }

    pub fn is_vaapi(&self) -> bool {
        match self {
            #[cfg(feature = "nvenc")]
            Self::Nvenc(_) => false,
            #[cfg(feature = "vaapi")]
            Self::Vaapi(_) => true,
            #[cfg(test)]
            Self::FailingVaapiForTest { .. } => true,
            Self::Software(_) => false,
            Self::SoftwareAvc444(enc) => enc.is_vaapi(),
        }
    }

    /// Create a software-only encoder (fallback when VA-API fails at runtime).
    pub fn new_software_only(
        width: u32,
        height: u32,
        bitrate: u32,
        fps: u32,
        quality: u8,
        rate_control: H264RateControl,
    ) -> Result<Self> {
        let enc = H264Encoder::new(width, height, bitrate, fps, quality, rate_control)?;
        tracing::info!("Using OpenH264 software encoder (runtime fallback)");
        Ok(Self::Software(Box::new(enc)))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_for_egfx_codec(
        policy: H264BackendPolicy,
        codec: EgfxFrameCodec,
        width: u32,
        height: u32,
        bitrate: u32,
        fps: u32,
        quality: u8,
        rate_control: H264RateControl,
    ) -> Result<Self> {
        match codec {
            EgfxFrameCodec::Avc420 => {
                Self::new(policy, width, height, bitrate, fps, quality, rate_control)
            }
            EgfxFrameCodec::Avc444 => {
                Self::new_avc444(policy, width, height, bitrate, fps, quality, rate_control)
            }
        }
    }

    pub fn new_avc444(
        policy: H264BackendPolicy,
        width: u32,
        height: u32,
        bitrate: u32,
        fps: u32,
        quality: u8,
        rate_control: H264RateControl,
    ) -> Result<Self> {
        anyhow::ensure!(
            policy.nvenc_device().is_none(),
            "NVENC supports only AVC420"
        );
        if !policy.allows_software() {
            anyhow::bail!("h264_backend=vaapi requested for AVC444, but the native VA-API backend only supports AVC420");
        }

        let enc = Avc444Encoder::new(width, height, bitrate, fps, quality, rate_control)?;
        tracing::info!("Using OpenH264 software AVC444 encoder");
        Ok(Self::SoftwareAvc444(Box::new(enc)))
    }

    pub fn new_avc444_software_only(
        width: u32,
        height: u32,
        bitrate: u32,
        fps: u32,
        quality: u8,
        rate_control: H264RateControl,
    ) -> Result<Self> {
        let enc = Avc444Encoder::new(width, height, bitrate, fps, quality, rate_control)?;
        tracing::info!("Using OpenH264 software AVC444 encoder");
        Ok(Self::SoftwareAvc444(Box::new(enc)))
    }

    pub(crate) fn new_software_only_for_egfx_codec(
        codec: EgfxFrameCodec,
        width: u32,
        height: u32,
        bitrate: u32,
        fps: u32,
        quality: u8,
        rate_control: H264RateControl,
    ) -> Result<Self> {
        match codec {
            EgfxFrameCodec::Avc420 => {
                Self::new_software_only(width, height, bitrate, fps, quality, rate_control)
            }
            EgfxFrameCodec::Avc444 => {
                Self::new_avc444_software_only(width, height, bitrate, fps, quality, rate_control)
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn failing_vaapi_for_test() -> Self {
        Self::FailingVaapiForTest {
            force_idr_requests: 0,
        }
    }

    /// Force the next encoded frame to be an IDR (recovery after dropped frames).
    pub fn force_idr(&mut self) {
        match self {
            #[cfg(feature = "vaapi")]
            Self::Vaapi(enc) => enc.force_idr(),
            #[cfg(test)]
            Self::FailingVaapiForTest { force_idr_requests } => {
                *force_idr_requests = force_idr_requests.saturating_add(1);
            }
            #[cfg(feature = "nvenc")]
            Self::Nvenc(enc) => enc.force_idr(),
            Self::Software(enc) => enc.force_idr(),
            Self::SoftwareAvc444(enc) => enc.force_idr(),
        }
    }

    pub fn force_full_frame(&mut self) {
        #[cfg(feature = "nvenc")]
        if let Self::Nvenc(encoder) = self {
            encoder.force_idr();
        }
        if let Self::SoftwareAvc444(enc) = self {
            enc.force_full_frame();
        }
    }

    #[cfg(test)]
    pub(crate) fn force_idr_requests_for_test(&self) -> Option<u32> {
        match self {
            #[cfg(feature = "vaapi")]
            Self::Vaapi(_) => None,
            #[cfg(feature = "nvenc")]
            Self::Nvenc(_) => None,
            Self::FailingVaapiForTest { force_idr_requests } => Some(*force_idr_requests),
            Self::Software(enc) => Some(enc.force_idr_requests_for_test()),
            Self::SoftwareAvc444(enc) => Some(enc.force_idr_requests_for_test()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn software_policy_never_selects_vaapi() {
        let encoder = FrameEncoder::new(
            H264BackendPolicy::Software,
            64,
            64,
            1_000_000,
            30,
            23,
            H264RateControl::Cqp,
        )
        .expect("software encoder initializes");

        assert_eq!(encoder.backend_name(), "openh264");
        assert!(!encoder.is_vaapi());
    }

    #[test]
    fn backend_policy_permissions_match_the_documented_modes() {
        assert!(H264BackendPolicy::Auto.allows_vaapi());
        assert!(H264BackendPolicy::Auto.allows_software());
        assert!(H264BackendPolicy::Auto.allows_runtime_software_fallback());

        assert!(!H264BackendPolicy::Software.allows_vaapi());
        assert!(H264BackendPolicy::Software.allows_software());
        assert!(!H264BackendPolicy::Software.allows_runtime_software_fallback());

        assert!(H264BackendPolicy::Vaapi.allows_vaapi());
        assert!(!H264BackendPolicy::Vaapi.allows_software());
        assert!(!H264BackendPolicy::Vaapi.allows_runtime_software_fallback());
    }

    #[test]
    fn avc444_software_backend_reports_openh264_and_not_vaapi() {
        let encoder = FrameEncoder::new_avc444(
            H264BackendPolicy::Software,
            64,
            64,
            1_000_000,
            30,
            23,
            H264RateControl::Cqp,
        )
        .expect("software AVC444 encoder initializes");

        assert_eq!(encoder.backend_name(), "openh264-avc444");
        assert!(!encoder.is_vaapi());
    }

    #[cfg(not(feature = "vaapi"))]
    #[test]
    fn forced_vaapi_reports_unavailable_in_non_vaapi_builds() {
        let avc420 = FrameEncoder::new(
            H264BackendPolicy::Vaapi,
            64,
            64,
            1_000_000,
            30,
            23,
            H264RateControl::Cqp,
        );
        let avc420_error = match avc420 {
            Ok(_) => panic!("VA-API must be unavailable"),
            Err(error) => error,
        };
        assert!(format!("{avc420_error:#}").contains("built without the vaapi feature"));

        let avc444 = FrameEncoder::new_avc444(
            H264BackendPolicy::Vaapi,
            64,
            64,
            1_000_000,
            30,
            23,
            H264RateControl::Cqp,
        );
        let avc444_error = match avc444 {
            Ok(_) => panic!("VA-API must be unavailable"),
            Err(error) => error,
        };
        assert!(format!("{avc444_error:#}").contains("native VA-API backend only supports AVC420"));
    }
}
