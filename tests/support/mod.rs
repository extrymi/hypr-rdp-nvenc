#![allow(dead_code)] // Shared fixtures exercise different subsets in unit/IPC tests.

use std::sync::{Arc, Mutex};

use hypr_rdp::nvenc::{AccessUnit, Config, Driver, Error, Fault, Frame, Phase, Submission};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Scenario {
    #[default]
    Normal,
    MissingLibrary,
    OldApi,
    NoDevice,
    UnsupportedCaps,
    FailInitialize,
    FailAllocate(usize),
    FailRelease,
    FailAfter(usize),
    Busy(usize),
    Delayed,
    EmptyOutput,
    NonIdr,
    Hang(Phase),
    HangAllocate,
    HangOutput,
}

pub type Events = Arc<Mutex<Vec<String>>>;

pub fn config() -> Config {
    Config {
        device: "GPU-01234567-89ab-cdef-0123-456789abcdef".parse().unwrap(),
        epoch: 17,
        width: 4,
        height: 4,
        fps: 30,
        bitrate: 1_000_000,
    }
}

pub struct FakeDriver {
    pub scenario: Scenario,
    pub events: Events,
    submits: usize,
    frames: usize,
    force_idr: bool,
}

impl FakeDriver {
    pub fn new(scenario: Scenario) -> Self {
        Self {
            scenario,
            events: Arc::default(),
            submits: 0,
            frames: 0,
            force_idr: false,
        }
    }

    fn record(&self, event: impl Into<String>) {
        self.events.lock().unwrap().push(event.into());
    }
    fn hang(&self, phase: Phase) {
        if self.scenario == Scenario::Hang(phase) {
            stall();
        }
    }
}

pub fn stall() -> ! {
    loop {
        std::thread::park();
    }
}

impl Driver for FakeDriver {
    fn probe(&mut self, _: &Config) -> Result<(), Error> {
        self.record("probe");
        self.hang(Phase::Probe);
        let fault = match self.scenario {
            Scenario::MissingLibrary => Some(Fault::LibraryMissing),
            Scenario::OldApi => Some(Fault::ApiUnsupported),
            Scenario::NoDevice => Some(Fault::DeviceUnavailable),
            Scenario::UnsupportedCaps => Some(Fault::CapabilityUnsupported),
            _ => None,
        };
        fault.map_or(Ok(()), |fault| Err(Error::backend(Phase::Probe, fault)))
    }
    fn initialize(&mut self, _: &Config) -> Result<(), Error> {
        self.record("initialize");
        self.hang(Phase::Initialize);
        if self.scenario == Scenario::FailInitialize {
            return Err(Error::backend(Phase::Initialize, Fault::Driver));
        }
        Ok(())
    }
    fn allocate(&mut self, slot: usize) -> Result<(), Error> {
        self.record(format!("allocate:{slot}"));
        if self.scenario == Scenario::HangAllocate {
            stall();
        }
        if self.scenario == Scenario::FailAllocate(slot) {
            return Err(Error::backend(Phase::Initialize, Fault::Driver));
        }
        Ok(())
    }
    fn submit(&mut self, slot: usize, frame: &Frame<'_>) -> Result<Submission, Error> {
        self.record(format!(
            "submit:{slot}:{}:{}",
            frame.stamp.sequence, frame.force_idr
        ));
        self.hang(Phase::Encode);
        self.submits += 1;
        self.force_idr = frame.force_idr;
        match self.scenario {
            Scenario::Busy(count) if self.submits <= count => Ok(Submission::Busy),
            Scenario::Delayed => Ok(Submission::Delayed),
            Scenario::FailAfter(count) if self.frames >= count => Err(Error::Backend {
                phase: Phase::Encode,
                fault: Fault::Driver,
                code: -7,
            }),
            _ => {
                self.frames += 1;
                Ok(Submission::Ready)
            }
        }
    }
    fn output(&mut self, slot: usize) -> Result<AccessUnit, Error> {
        self.record(format!("output:{slot}"));
        if self.scenario == Scenario::HangOutput {
            stall();
        }
        Ok(AccessUnit {
            bytes: if self.scenario == Scenario::EmptyOutput {
                vec![]
            } else {
                vec![0, 0, 0, 1, 0x65, 0x80]
            },
            is_idr: self.force_idr && self.scenario != Scenario::NonIdr,
        })
    }
    fn drain(&mut self) -> Result<(), Error> {
        self.record("drain");
        self.hang(Phase::Drain);
        Ok(())
    }
    fn release(&mut self, slot: usize) -> Result<(), Error> {
        self.record(format!("release:{slot}"));
        if self.scenario == Scenario::FailRelease {
            return Err(Error::backend(Phase::Destroy, Fault::Driver));
        }
        Ok(())
    }
    fn destroy(&mut self) -> Result<(), Error> {
        self.record("destroy");
        self.hang(Phase::Destroy);
        Ok(())
    }
}

impl Drop for FakeDriver {
    fn drop(&mut self) {
        self.record("drop");
        self.hang(Phase::Exit);
    }
}
