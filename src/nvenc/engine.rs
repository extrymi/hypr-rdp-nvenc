use super::{
    AccessUnit, Config, EncodedFrame, Error, Fault, Frame, OutputCheck, Phase, BUFFER_COUNT,
    MAX_OUTPUT_BYTES,
};

/// Distinguishes unaccepted input from accepted input with delayed output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Submission {
    Ready,
    Busy,
    Delayed,
}

/// Implementations own their handles and must tolerate `destroy` after partial
/// initialization. A failed allocation releases its own partial objects; each
/// successful slot allocation is released exactly once by Engine. All methods
/// may block in native code and therefore must run only in the worker process.
pub trait Driver {
    fn probe(&mut self, config: &Config) -> Result<(), Error>;
    fn initialize(&mut self, config: &Config) -> Result<(), Error>;
    fn allocate(&mut self, slot: usize) -> Result<(), Error>;
    fn submit(&mut self, slot: usize, frame: &Frame<'_>) -> Result<Submission, Error>;
    fn output(&mut self, slot: usize) -> Result<AccessUnit, Error>;
    fn drain(&mut self) -> Result<(), Error>;
    fn release(&mut self, slot: usize) -> Result<(), Error>;
    fn destroy(&mut self) -> Result<(), Error>;
}

/// Single accepted frame in flight, zero delayed outputs, at most two retries
/// of input explicitly reported unaccepted. Never used in the server process.
pub struct Engine<D: Driver> {
    driver: D,
    config: Config,
    next_sequence: u64,
    allocated: usize,
    opened: bool,
    active: bool,
}

impl<D: Driver> Engine<D> {
    pub fn new(driver: D, config: Config) -> Result<Self, Error> {
        config.frame_bytes()?;
        let mut engine = Self {
            driver,
            config,
            next_sequence: 0,
            allocated: 0,
            opened: false,
            active: false,
        };
        engine.driver.probe(&engine.config)?;
        // Set before calling initialize: destroy must cover partial setup.
        engine.opened = true;
        engine.driver.initialize(&engine.config)?;
        for slot in 0..BUFFER_COUNT {
            engine.driver.allocate(slot)?;
            engine.allocated += 1;
        }
        engine.active = true;
        Ok(engine)
    }

    pub fn encode(&mut self, frame: Frame<'_>) -> Result<EncodedFrame, Error> {
        if !self.active {
            return Err(Error::Closed);
        }
        if frame.stamp.epoch != self.config.epoch
            || frame.stamp.sequence != self.next_sequence
            || frame.nv12.len() != self.config.frame_bytes()?
            || self.next_sequence == u64::MAX
        {
            return Err(Error::InvalidFrame);
        }
        let frame = Frame {
            force_idr: frame.force_idr || self.next_sequence == 0,
            ..frame
        };
        let result = self.encode_inner(&frame);
        if result.is_err() {
            self.active = false;
        }
        result
    }

    fn encode_inner(&mut self, frame: &Frame<'_>) -> Result<EncodedFrame, Error> {
        let slot = (self.next_sequence % BUFFER_COUNT as u64) as usize;
        for attempt in 0..=2 {
            match self.driver.submit(slot, frame)? {
                Submission::Busy if attempt < 2 => continue,
                Submission::Busy => return Err(Error::backend(Phase::Encode, Fault::Busy)),
                Submission::Delayed => {
                    // The input is accepted. Do not submit it again or lock an
                    // output that is not ready. This MVP cannot pump delays.
                    return Err(Error::backend(Phase::Encode, Fault::DelayedOutput));
                }
                Submission::Ready => break,
            }
        }
        let unit = self.driver.output(slot)?;
        if unit.bytes.is_empty()
            || unit.bytes.len() > MAX_OUTPUT_BYTES
            || (frame.force_idr && !unit.is_idr)
        {
            return Err(Error::invalid_output(OutputCheck::EngineOutput));
        }
        self.next_sequence += 1;
        Ok(EncodedFrame {
            stamp: frame.stamp,
            unit,
        })
    }

    pub fn drain(&mut self) -> Result<(), Error> {
        if !self.active {
            return Err(Error::Closed);
        }
        self.active = false;
        self.driver.drain()
    }

    /// Continue releasing the remaining resources after an individual failure.
    /// Mark each attempted release before calling it, so Drop never retries an
    /// ambiguous native operation. Process termination owns the final boundary.
    pub fn close(&mut self) -> Result<(), Error> {
        self.active = false;
        let mut failure = None;
        while self.allocated > 0 {
            self.allocated -= 1;
            if let Err(error) = self.driver.release(self.allocated) {
                failure.get_or_insert(error);
            }
        }
        if self.opened {
            self.opened = false;
            if let Err(error) = self.driver.destroy() {
                failure.get_or_insert(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }
}

impl<D: Driver> Drop for Engine<D> {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
