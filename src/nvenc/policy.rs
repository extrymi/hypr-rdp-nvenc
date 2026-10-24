use super::{Error, Fault};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackendMode {
    Auto,
    Software,
    Vaapi,
    Nvenc,
}

/// Classification of the explicitly selected device, not all installed GPUs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeviceClass {
    Nvidia,
    Vaapi,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Plan {
    Software,
    Vaapi { software_fallback: bool },
    Nvenc { software_fallback: bool },
}

/// Pure selection policy, to be wired into the server in the integration step.
/// `quarantined` refers to the selected device. It blocks VAAPI as an alternate
/// route to the same GPU as well as NVENC. Software performs no device probing.
pub fn plan(mode: BackendMode, device: DeviceClass, quarantined: bool) -> Result<Plan, Error> {
    use BackendMode::*;
    match mode {
        Software => Ok(Plan::Software),
        Auto if quarantined => Ok(Plan::Software),
        Nvenc | Vaapi if quarantined => Err(Error::Quarantined),
        Nvenc => Ok(Plan::Nvenc { software_fallback: false }),
        Vaapi => Ok(Plan::Vaapi { software_fallback: false }),
        Auto => Ok(match device {
            DeviceClass::Nvidia => Plan::Nvenc { software_fallback: true },
            DeviceClass::Vaapi => Plan::Vaapi { software_fallback: true },
            DeviceClass::Unknown => Plan::Software,
        }),
    }
}

/// A strict request never falls back. An auto request can use software only
/// after the old process has been reaped and for a classified backend failure.
/// Protocol, state-store and transport faults require explicit investigation.
pub fn software_fallback_allowed(plan: Plan, error: Error, reaped: bool) -> bool {
    let allows = matches!(plan, Plan::Nvenc { software_fallback: true } | Plan::Vaapi { software_fallback: true });
    allows
        && reaped
        && matches!(error, Error::Backend { fault: Fault::LibraryMissing | Fault::ApiUnsupported | Fault::DeviceUnavailable | Fault::CapabilityUnsupported | Fault::Driver | Fault::Busy | Fault::DelayedOutput | Fault::InvalidOutput, .. })
}
