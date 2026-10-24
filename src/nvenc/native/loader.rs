use std::path::Path;

use libloading::Library;

use super::ffi;
use crate::nvenc::{Error, Fault, Phase};

pub(super) struct Api {
    pub cuda: ffi::HYPR_CUDA_FUNCTIONS,
    pub enc: ffi::NV_ENCODE_API_FUNCTION_LIST,
    // Function tables must never outlive the libraries providing their pointers.
    _libraries: Option<(Library, Library)>,
}

fn failure(fault: Fault) -> Error { Error::backend(Phase::Probe, fault) }

impl Api {
    /// Called only inside the private worker, never by desktop/server probing.
    pub fn load() -> Result<Self, Error> {
        Self::load_paths(Path::new("libcuda.so.1"), Path::new("libnvidia-encode.so.1"))
    }

    pub(super) fn load_paths(cuda: &Path, encoder: &Path) -> Result<Self, Error> {
        // SAFETY: only the worker loads system driver SONAMEs. No configurable
        // search path or caller-supplied library path is exposed in production.
        let cuda = unsafe { Library::new(cuda) }.map_err(|_| failure(Fault::LibraryMissing))?;
        let encoder = unsafe { Library::new(encoder) }.map_err(|_| failure(Fault::LibraryMissing))?;
        // SAFETY: every symbol signature comes from the pinned C header and both
        // owning libraries are retained until every context/session is destroyed.
        unsafe {
            let maximum: ffi::HYPR_GET_MAX_VERSION = symbol(&encoder, b"NvEncodeAPIGetMaxSupportedVersion\0")?;
            let create: ffi::HYPR_CREATE_INSTANCE = symbol(&encoder, b"NvEncodeAPICreateInstance\0")?;
            let mut version = 0;
            let status = maximum.ok_or_else(|| failure(Fault::ApiUnsupported))?(&mut version);
            if status != ffi::NV_ENC_SUCCESS || version < ffi::HYPR_MIN_API {
                return Err(failure(Fault::ApiUnsupported));
            }
            let cuda_api = ffi::HYPR_CUDA_FUNCTIONS {
                init: symbol(&cuda, b"cuInit\0")?,
                count: symbol(&cuda, b"cuDeviceGetCount\0")?,
                device: symbol(&cuda, b"cuDeviceGet\0")?,
                uuid: symbol(&cuda, b"cuDeviceGetUuid\0")?,
                create: symbol(&cuda, b"cuCtxCreate_v2\0")?,
                destroy: symbol(&cuda, b"cuCtxDestroy_v2\0")?,
            };
            let mut enc = ffi::NV_ENCODE_API_FUNCTION_LIST { version: ffi::HYPR_NV_ENCODE_API_FUNCTION_LIST_VER, ..Default::default() };
            if create.ok_or_else(|| failure(Fault::ApiUnsupported))?(&mut enc) != ffi::NV_ENC_SUCCESS {
                return Err(failure(Fault::ApiUnsupported));
            }
            Self::checked(cuda_api, enc, Some((cuda, encoder)))
        }
    }

    pub fn checked(cuda: ffi::HYPR_CUDA_FUNCTIONS, enc: ffi::NV_ENCODE_API_FUNCTION_LIST,
        libraries: Option<(Library, Library)>) -> Result<Self, Error> {
        let complete = cuda.init.is_some() && cuda.count.is_some() && cuda.device.is_some()
            && cuda.uuid.is_some() && cuda.create.is_some() && cuda.destroy.is_some()
            && enc.nvEncOpenEncodeSessionEx.is_some() && enc.nvEncDestroyEncoder.is_some()
            && enc.nvEncGetEncodeGUIDCount.is_some() && enc.nvEncGetEncodeGUIDs.is_some()
            && enc.nvEncGetEncodeProfileGUIDCount.is_some() && enc.nvEncGetEncodeProfileGUIDs.is_some()
            && enc.nvEncGetInputFormatCount.is_some() && enc.nvEncGetInputFormats.is_some()
            && enc.nvEncGetEncodePresetCount.is_some() && enc.nvEncGetEncodePresetGUIDs.is_some()
            && enc.nvEncGetEncodeCaps.is_some() && enc.nvEncGetEncodePresetConfigEx.is_some()
            && enc.nvEncInitializeEncoder.is_some() && enc.nvEncCreateInputBuffer.is_some()
            && enc.nvEncDestroyInputBuffer.is_some() && enc.nvEncCreateBitstreamBuffer.is_some()
            && enc.nvEncDestroyBitstreamBuffer.is_some() && enc.nvEncLockInputBuffer.is_some()
            && enc.nvEncUnlockInputBuffer.is_some() && enc.nvEncEncodePicture.is_some()
            && enc.nvEncLockBitstream.is_some() && enc.nvEncUnlockBitstream.is_some();
        if !complete { return Err(failure(Fault::ApiUnsupported)); }
        Ok(Self { cuda, enc, _libraries: libraries })
    }

    #[cfg(test)]
    pub fn missing_libraries() -> Result<Self, Error> {
        Self::load_paths(Path::new("/nonexistent-hypr-rdp-test/libcuda.so"), Path::new("/nonexistent-hypr-rdp-test/encode.so"))
    }
}

unsafe fn symbol<T: Copy>(library: &Library, name: &[u8]) -> Result<T, Error> {
    // SAFETY: caller supplies the matching generated function-pointer typedef.
    unsafe { library.get::<T>(name) }.map(|value| *value).map_err(|_| failure(Fault::ApiUnsupported))
}
