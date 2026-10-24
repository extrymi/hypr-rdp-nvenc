use std::{os::{fd::AsRawFd, unix::net::UnixStream as StdStream}, process::Stdio, time::Duration};

use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::UnixStream, process::{Child, Command}, time::{timeout, Instant, timeout_at}};

use super::{protocol::{self, Message, OVERHEAD}, Config, DeviceLease, Driver, EncodedFrame, Engine, Error, Frame, Phase, Stamp, MAX_FRAME_BYTES, MAX_OUTPUT_BYTES};

/// The sole descriptor inherited by the worker for its private socket pair.
pub const WORKER_FD: i32 = 3;

#[derive(Clone, Copy, Debug)]
pub struct Deadlines {
    pub startup: Duration,
    pub initialize: Duration,
    pub encode: Duration,
    pub drain: Duration,
    pub destroy: Duration,
    pub exit: Duration,
    pub reap: Duration,
}

impl Default for Deadlines {
    fn default() -> Self {
        Self { startup: Duration::from_secs(3), initialize: Duration::from_secs(5), encode: Duration::from_secs(2), drain: Duration::from_secs(2), destroy: Duration::from_secs(2), exit: Duration::from_secs(1), reap: Duration::from_secs(1) }
    }
}

/// Parent-side supervisor. Native code never runs here. Every socket exchange
/// has one absolute deadline covering writes, prefix, and complete payload;
/// a peer sending one byte at a time cannot extend the deadline.
pub struct Worker {
    child: Child,
    stream: UnixStream,
    lease: Option<DeviceLease>,
    config: Config,
    deadlines: Deadlines,
    next_sequence: u64,
    closed: bool,
    reaped: bool,
}

impl Worker {
    /// The production caller must pass the same package executable with its
    /// private worker argument. An injected Command also permits CPU fixtures
    /// to exercise actual subprocess hangs. No production entry point is wired
    /// yet. The child gets no inherited environment, stdin, stdout or stderr.
    pub async fn spawn(mut command: Command, lease: DeviceLease, config: Config, deadlines: Deadlines) -> Result<Self, Error> {
        config.frame_bytes()?;
        if lease.device() != &config.device { return Err(Error::InvalidConfig); }
        if [deadlines.startup, deadlines.initialize, deadlines.encode, deadlines.drain, deadlines.destroy, deadlines.exit, deadlines.reap].iter().any(|duration| duration.is_zero() || *duration > Duration::from_secs(60)) {
            return Err(Error::InvalidConfig);
        }
        let (parent, child_socket) = StdStream::pair().map_err(|_| Error::Io)?;
        parent.set_nonblocking(true).map_err(|_| Error::Io)?;
        let stream = UnixStream::from_std(parent).map_err(|_| Error::Io)?;
        let descriptor = child_socket.as_raw_fd();
        command.env_clear().stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
        // SAFETY: the closure uses only async-signal-safe dup2/fcntl and errno
        // access after fork. child_socket remains alive until spawn returns.
        // dup2 clears CLOEXEC, except when its two descriptors are equal.
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(descriptor, WORKER_FD) < 0 || libc::fcntl(WORKER_FD, libc::F_SETFD, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().map_err(|_| Error::Spawn)?;
        drop(child_socket);
        let mut worker = Self { child, stream, lease: Some(lease), config, deadlines, next_sequence: 0, closed: false, reaped: false };
        let identity = worker.child.id().ok_or(Error::Spawn).and_then(|pid| worker.lease.as_ref().ok_or(Error::StateStore)?.bind(pid));
        if let Err(error) = identity { return Err(worker.fail(error).await); }
        let startup = worker.receive(Phase::Startup, deadlines.startup, OVERHEAD).await;
        if !matches!(startup, Ok(Message::Hello)) {
            return Err(worker.fail(startup.err().unwrap_or(Error::Protocol)).await);
        }
        let init = Message::Init(worker.config.clone());
        let initialized = worker.exchange(&init, Phase::Initialize, deadlines.initialize, OVERHEAD).await;
        match initialized {
            Ok(Message::Ready) => Ok(worker),
            Ok(Message::Failure(error)) => Err(worker.fail(error).await),
            Ok(_) => Err(worker.fail(Error::Protocol).await),
            Err(error) => Err(worker.fail(error).await),
        }
    }

    pub fn was_reaped(&self) -> bool { self.reaped }

    pub async fn encode(&mut self, frame: Frame<'_>) -> Result<EncodedFrame, Error> {
        if self.closed { return Err(Error::Closed); }
        if frame.stamp != (Stamp { epoch: self.config.epoch, sequence: self.next_sequence })
            || frame.nv12.len() != self.config.frame_bytes()?
            || self.next_sequence == u64::MAX { return Err(Error::InvalidFrame); }
        let force_idr = frame.force_idr || self.next_sequence == 0;
        let request = Message::Encode { stamp: frame.stamp, force_idr, data: frame.nv12.to_vec() };
        let reply = self.exchange(&request, Phase::Encode, self.deadlines.encode, MAX_OUTPUT_BYTES + OVERHEAD).await;
        let error = match reply {
            Ok(Message::Output(output)) if output.stamp == frame.stamp
                && !output.unit.bytes.is_empty()
                && output.unit.bytes.len() <= MAX_OUTPUT_BYTES
                && (!force_idr || output.unit.is_idr) => {
                    self.next_sequence += 1;
                    return Ok(output);
                }
            Ok(Message::Failure(error)) => error,
            Ok(_) => Error::Protocol,
            Err(error) => error,
        };
        Err(self.fail(error).await)
    }

    pub async fn shutdown(&mut self) -> Result<(), Error> {
        if self.closed { return if self.reaped { Ok(()) } else { Err(Error::Quarantined) }; }
        for (request, phase, duration) in [(Message::Drain, Phase::Drain, self.deadlines.drain), (Message::Destroy, Phase::Destroy, self.deadlines.destroy)] {
            let reply = self.exchange(&request, phase, duration, OVERHEAD).await;
            let error = match reply {
                Ok(Message::Ack) => continue,
                Ok(Message::Failure(error)) => error,
                Ok(_) => Error::Protocol,
                Err(error) => error,
            };
            return Err(self.fail(error).await);
        }
        let exit = async {
            self.stream.write_all(&protocol::encode(&Message::Exit)?).await.map_err(|_| Error::Io)?;
            let status = self.child.wait().await.map_err(|_| Error::Io)?;
            self.reaped = true;
            if !status.success() { return Err(Error::WorkerExited); }
            Ok(())
        };
        match timeout(self.deadlines.exit, exit).await {
            Ok(Ok(())) => {
                self.closed = true;
                self.release_lease()
            }
            Ok(Err(error)) => Err(self.fail(error).await),
            Err(_) => Err(self.fail(Error::Timeout(Phase::Exit)).await),
        }
    }

    async fn exchange(&mut self, message: &Message, phase: Phase, duration: Duration, limit: usize) -> Result<Message, Error> {
        let deadline = Instant::now() + duration;
        let bytes = protocol::encode(message)?;
        timeout_at(deadline, async {
            self.stream.write_all(&bytes).await.map_err(|_| Error::Io)?;
            Self::read(&mut self.stream, limit).await
        }).await.map_err(|_| Error::Timeout(phase))?
    }

    async fn receive(&mut self, phase: Phase, duration: Duration, limit: usize) -> Result<Message, Error> {
        timeout(duration, Self::read(&mut self.stream, limit)).await.map_err(|_| Error::Timeout(phase))?
    }

    async fn read(stream: &mut UnixStream, limit: usize) -> Result<Message, Error> {
        let length = stream.read_u32_le().await.map_err(|_| Error::Io)? as usize;
        protocol::check_length(length, limit)?;
        let mut bytes = vec![0; length];
        stream.read_exact(&mut bytes).await.map_err(|_| Error::Io)?;
        protocol::decode(&bytes)
    }

    async fn fail(&mut self, original: Error) -> Error {
        self.closed = true;
        let _ = self.child.start_kill();
        match timeout(self.deadlines.reap, self.child.wait()).await {
            Ok(Ok(_)) => {
                self.reaped = true;
                match self.release_lease() { Ok(()) => original, Err(error) => error }
            }
            // Keep the marker. Even if kill_on_drop is attempted, only a later
            // boot/PID/start-time reconciliation can permit another worker.
            _ => Error::Quarantined,
        }
    }

    fn release_lease(&mut self) -> Result<(), Error> {
        if let Some(lease) = self.lease.take() { lease.clear_if_gone()?; }
        Ok(())
    }
}

/// Blocking worker loop. Driver construction must be inert; the first probe
/// happens only after Init, after its identity marker has been persisted.
/// A future native driver is constructed and used on this same OS thread.
pub fn serve<D: Driver>(mut stream: StdStream, driver: D) -> Result<(), Error> {
    protocol::write(&mut stream, &Message::Hello)?;
    let config = match protocol::read(&mut stream, OVERHEAD)? {
        Message::Init(config) => config,
        _ => return Err(Error::Protocol),
    };
    let mut engine = match Engine::new(driver, config) {
        Ok(engine) => engine,
        Err(error) => { protocol::write(&mut stream, &Message::Failure(error))?; return Err(error); }
    };
    protocol::write(&mut stream, &Message::Ready)?;
    // Protocol ordering makes shutdown idempotent and forbids encoding after
    // drain/destroy. Driver errors terminate this generation immediately.
    let mut state = Phase::Encode;
    loop {
        let message = protocol::read(&mut stream, MAX_FRAME_BYTES + OVERHEAD)?;
        let reply = match (state, message) {
            (Phase::Encode, Message::Encode { stamp, force_idr, data }) => engine.encode(Frame { stamp, force_idr, nv12: &data }).map(Message::Output),
            (Phase::Encode, Message::Drain) => { state = Phase::Destroy; engine.drain().map(|()| Message::Ack) }
            (Phase::Destroy, Message::Destroy) => { state = Phase::Exit; engine.close().map(|()| Message::Ack) }
            (Phase::Exit, Message::Exit) => return Ok(()),
            _ => return Err(Error::Protocol),
        };
        match reply {
            Ok(reply) => protocol::write(&mut stream, &reply)?,
            Err(error) => { protocol::write(&mut stream, &Message::Failure(error))?; return Err(error); }
        }
    }
}
