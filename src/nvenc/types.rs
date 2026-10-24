use std::{fmt, str::FromStr};

use super::MAX_FRAME_BYTES;

/// Stable NVIDIA UUID. Ordinals and DRM node names are deliberately not IDs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeviceId([u8; 16]);

impl FromStr for DeviceId {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Error> {
        let suffix = value.strip_prefix("GPU-").ok_or(Error::InvalidConfig)?;
        if !valid_uuid(suffix) {
            return Err(Error::InvalidConfig);
        }
        let hex = suffix.replace('-', "");
        let mut bytes = [0; 16];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16)
                .map_err(|_| Error::InvalidConfig)?;
        }
        Ok(Self(bytes))
    }
}

pub(super) fn valid_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, byte)| {
            if [8, 13, 18, 23].contains(&i) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GPU-")?;
        for (index, byte) in self.0.iter().enumerate() {
            if [4, 6, 8, 10].contains(&index) {
                f.write_str("-")?;
            }
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// One immutable encoder generation. Resize requires a new worker and epoch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Config {
    pub device: DeviceId,
    pub epoch: u64,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
}

impl Config {
    /// Input is tightly packed SDR 8-bit NV12, with even dimensions. Driver
    /// pitch and device capability checks remain the native adapter's job.
    pub fn frame_bytes(&self) -> Result<usize, Error> {
        if self.epoch == 0
            || self.width == 0
            || self.height == 0
            || self.width > 8192
            || self.height > 8192
            || !self.width.is_multiple_of(2)
            || !self.height.is_multiple_of(2)
            || self.fps == 0
            || self.fps > 240
            || self.bitrate == 0
        {
            return Err(Error::InvalidConfig);
        }
        let bytes = (self.width as usize)
            .checked_mul(self.height as usize)
            .and_then(|pixels| pixels.checked_mul(3))
            .map(|bytes| bytes / 2)
            .filter(|bytes| *bytes <= MAX_FRAME_BYTES)
            .ok_or(Error::InvalidConfig)?;
        Ok(bytes)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Stamp {
    pub epoch: u64,
    pub sequence: u64,
}

pub struct Frame<'a> {
    pub stamp: Stamp,
    pub force_idr: bool,
    pub nv12: &'a [u8],
}

/// The native adapter must validate/normalize H.264 Annex B and its parameter
/// sets before constructing this value. A nonempty byte array alone is not a
/// bitstream validity check. This layer only enforces allocation/flow bounds.
#[derive(Debug, Eq, PartialEq)]
pub struct AccessUnit {
    pub bytes: Vec<u8>,
    pub is_idr: bool,
}

#[derive(Debug, Eq, PartialEq)]
pub struct EncodedFrame {
    pub stamp: Stamp,
    pub unit: AccessUnit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Phase {
    Startup = 0,
    Probe = 1,
    Initialize = 2,
    Encode = 3,
    Drain = 4,
    Destroy = 5,
    Exit = 6,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Fault {
    LibraryMissing = 0,
    ApiUnsupported = 1,
    DeviceUnavailable = 2,
    CapabilityUnsupported = 3,
    Driver = 4,
    Busy = 5,
    DelayedOutput = 6,
    InvalidOutput = 7,
}

/// Typed failures cross IPC without driver strings, pointers or credentials.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidConfig,
    InvalidFrame,
    Closed,
    Quarantined,
    StateStore,
    Spawn,
    Io,
    Protocol,
    Timeout(Phase),
    WorkerExited,
    Backend {
        phase: Phase,
        fault: Fault,
        code: i32,
    },
}

impl Error {
    pub fn backend(phase: Phase, fault: Fault) -> Self {
        Self::Backend {
            phase,
            fault,
            code: 0,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NVENC: {self:?}")
    }
}

impl std::error::Error for Error {}
