//! CPU AVC420 upload through the private NVENC process. The capture thread owns
//! this facade; NVIDIA libraries and native handles live only in the child.
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result};
use hypr_rdp::nvenc::{self, BlockingWorker, Config, Deadlines, DeviceId, Error, Plan};

use super::h264::{convert_bgra_to_yuv420_planes, validate_bgra_buffer};

static NEXT_EPOCH: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
pub(super) struct Failure {
    pub error: Error,
    pub reaped: bool,
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}
impl std::error::Error for Failure {}

pub(super) fn can_fallback(error: &anyhow::Error) -> bool {
    error.downcast_ref::<Failure>().is_some_and(|failure| {
        nvenc::software_fallback_allowed(
            Plan::Nvenc {
                software_fallback: true,
            },
            failure.error,
            failure.reaped,
        )
    })
}

pub struct NvencEncoder {
    session: BlockingWorker,
    pixels: Nv12Pixels,
    force_idr: bool,
}

impl NvencEncoder {
    pub fn new(device: DeviceId, width: u32, height: u32, bitrate: u32, fps: u32) -> Result<Self> {
        let epoch = NEXT_EPOCH
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| Error::InvalidConfig)?;
        let config = Config {
            device,
            epoch,
            width,
            height,
            bitrate,
            fps,
        };
        let root = runtime_root()?;
        Self::with_command(config, &root, worker_command()?, Deadlines::default())
    }

    fn with_command(
        config: Config,
        root: &std::path::Path,
        command: tokio::process::Command,
        deadlines: Deadlines,
    ) -> Result<Self> {
        let pixels = Nv12Pixels::new(&config)?;
        let session =
            BlockingWorker::spawn(command, root, config, deadlines).map_err(|error| Failure {
                // Worker::spawn returns a Backend error only after its failure path
                // confirmed child exit. Unconfirmed reap is Quarantined instead.
                reaped: matches!(error, Error::Backend { .. }),
                error,
            })?;
        Ok(Self {
            session,
            pixels,
            force_idr: true,
        })
    }

    pub fn encode(&mut self, bgra: &[u8], stride: usize) -> Result<Vec<u8>> {
        let nv12 = self.pixels.convert(bgra, stride)?;
        Self::submit(&mut self.session, &mut self.force_idr, nv12)
    }

    fn submit(session: &mut BlockingWorker, force_idr: &mut bool, nv12: &[u8]) -> Result<Vec<u8>> {
        match session.encode(nv12, *force_idr) {
            Ok(frame) => {
                *force_idr = false;
                Ok(frame.unit.bytes)
            }
            Err(error) => Err(Failure {
                error,
                reaped: session.was_reaped(),
            }
            .into()),
        }
    }

    pub fn force_idr(&mut self) {
        self.force_idr = true;
    }
}

/// Private worker invocation of this server's own executable.
pub(crate) fn worker_command() -> std::io::Result<tokio::process::Command> {
    let mut command = tokio::process::Command::new(std::env::current_exe()?);
    command.arg(nvenc::WORKER_ARGUMENT);
    Ok(command)
}

fn runtime_root() -> Result<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let parent = PathBuf::from(
        std::env::var_os("XDG_RUNTIME_DIR").context("NVENC requires XDG_RUNTIME_DIR")?,
    );
    let metadata = std::fs::symlink_metadata(&parent)?;
    // SAFETY: geteuid has no pointer arguments.
    let uid = unsafe { libc::geteuid() };
    anyhow::ensure!(
        parent.is_absolute()
            && metadata.is_dir()
            && metadata.uid() == uid
            && metadata.mode() & 0o077 == 0,
        "NVENC requires a private, owned runtime directory"
    );
    Ok(parent.join("hypr-rdp-nvenc"))
}

struct Nv12Pixels {
    width: usize,
    height: usize,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    nv12: Vec<u8>,
}

#[cfg(test)]
pub(crate) mod tests;

impl Nv12Pixels {
    fn new(config: &Config) -> Result<Self> {
        let bytes = config.frame_bytes()?;
        let pixels = config.width as usize * config.height as usize;
        Ok(Self {
            width: config.width as usize,
            height: config.height as usize,
            y: vec![0; pixels],
            u: vec![0; pixels / 4],
            v: vec![0; pixels / 4],
            nv12: vec![0; bytes],
        })
    }

    fn convert(&mut self, bgra: &[u8], stride: usize) -> Result<&[u8]> {
        validate_bgra_buffer(self.width, self.height, stride, bgra.len())?;
        anyhow::ensure!(
            u32::try_from(stride).is_ok(),
            "BGRA stride exceeds converter range"
        );
        convert_bgra_to_yuv420_planes(
            self.width,
            self.height,
            bgra,
            stride,
            &mut self.y,
            &mut self.u,
            &mut self.v,
        )?;
        let (y, uv) = self.nv12.split_at_mut(self.y.len());
        y.copy_from_slice(&self.y);
        for ((pair, u), v) in uv
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .zip(&self.u)
            .zip(&self.v)
        {
            pair.copy_from_slice(&[*u, *v]);
        }
        Ok(&self.nv12)
    }
}
