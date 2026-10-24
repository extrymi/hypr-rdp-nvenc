//! Header-owned aggregate values cross a tiny typed C boundary. No manually
//! copied GUID values or references to C header-local statics are needed.
use super::ffi;

pub(super) fn h264() -> ffi::GUID {
    // SAFETY: the linked function returns an immutable header constant by value.
    unsafe { ffi::hypr_nvenc_h264() }
}
pub(super) fn profile() -> ffi::GUID {
    // SAFETY: the linked function returns an immutable header constant by value.
    unsafe { ffi::hypr_nvenc_profile() }
}
pub(super) fn preset() -> ffi::GUID {
    // SAFETY: the linked function returns an immutable header constant by value.
    unsafe { ffi::hypr_nvenc_preset() }
}
