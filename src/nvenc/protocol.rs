//! Private, versioned, length-delimited protocol. Limits are checked before
//! allocating payloads; no native handles or memory addresses cross this API.

use std::io::{Read, Write};

use super::{AccessUnit, Config, EncodedFrame, Error, Fault, Phase, Stamp, MAX_FRAME_BYTES};

pub(crate) const OVERHEAD: usize = 128;
const MAGIC: &[u8] = b"HNV1";

pub(crate) enum Message {
    Hello,
    Init(Config),
    Ready,
    Encode { stamp: Stamp, force_idr: bool, data: Vec<u8> },
    Output(EncodedFrame),
    Drain,
    Destroy,
    Exit,
    Ack,
    Failure(Error),
}

pub(crate) fn encode(message: &Message) -> Result<Vec<u8>, Error> {
    let mut out = MAGIC.to_vec();
    match message {
        Message::Hello => out.push(0),
        Message::Init(config) => {
            config.frame_bytes()?;
            out.push(1);
            out.extend_from_slice(config.device.to_string().as_bytes());
            out.extend_from_slice(&config.epoch.to_le_bytes());
            for value in [config.width, config.height, config.fps, config.bitrate] {
                out.extend_from_slice(&value.to_le_bytes());
            }
        }
        Message::Ready => out.push(2),
        Message::Encode { stamp, force_idr, data } => {
            out.push(3);
            put_stamp(&mut out, *stamp);
            out.push(u8::from(*force_idr));
            out.extend_from_slice(data);
        }
        Message::Output(frame) => {
            out.push(4);
            put_stamp(&mut out, frame.stamp);
            out.push(u8::from(frame.unit.is_idr));
            out.extend_from_slice(&frame.unit.bytes);
        }
        Message::Drain => out.push(5),
        Message::Destroy => out.push(6),
        Message::Exit => out.push(7),
        Message::Ack => out.push(8),
        Message::Failure(error) => {
            out.push(9);
            // Only engine errors are legitimate worker replies. Local I/O,
            // timeout, process and lease errors are never accepted from it.
            match error {
                Error::Backend { phase, fault, code } => {
                    out.extend_from_slice(&[0, *phase as u8, *fault as u8]);
                    out.extend_from_slice(&code.to_le_bytes());
                }
                Error::InvalidConfig => out.push(1),
                Error::InvalidFrame => out.push(2),
                Error::Closed => out.push(3),
                _ => return Err(Error::Protocol),
            }
        }
    }
    check_length(out.len(), MAX_FRAME_BYTES + OVERHEAD)?;
    let mut packet = (out.len() as u32).to_le_bytes().to_vec();
    packet.extend_from_slice(&out);
    Ok(packet)
}

fn put_stamp(out: &mut Vec<u8>, stamp: Stamp) {
    out.extend_from_slice(&stamp.epoch.to_le_bytes());
    out.extend_from_slice(&stamp.sequence.to_le_bytes());
}

pub(crate) fn check_length(length: usize, limit: usize) -> Result<(), Error> {
    if length < MAGIC.len() + 1 || length > limit { return Err(Error::Protocol); }
    Ok(())
}

struct Fields<'a>(&'a [u8]);

impl<'a> Fields<'a> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let value = self.0.get(..N).ok_or(Error::Protocol)?.try_into().map_err(|_| Error::Protocol)?;
        self.0 = &self.0[N..];
        Ok(value)
    }
    fn byte(&mut self) -> Result<u8, Error> { Ok(self.take::<1>()?[0]) }
    fn boolean(&mut self) -> Result<bool, Error> {
        match self.byte()? { 0 => Ok(false), 1 => Ok(true), _ => Err(Error::Protocol) }
    }
    fn u32(&mut self) -> Result<u32, Error> { Ok(u32::from_le_bytes(self.take()?)) }
    fn u64(&mut self) -> Result<u64, Error> { Ok(u64::from_le_bytes(self.take()?)) }
    fn stamp(&mut self) -> Result<Stamp, Error> {
        Ok(Stamp { epoch: self.u64()?, sequence: self.u64()? })
    }
}

pub(crate) fn decode(bytes: &[u8]) -> Result<Message, Error> {
    if !bytes.starts_with(MAGIC) { return Err(Error::Protocol); }
    let mut fields = Fields(&bytes[MAGIC.len()..]);
    let result = match fields.byte()? {
        0 => Message::Hello,
        1 => {
            let device = fields.take::<40>()?;
            let config = Config {
                device: std::str::from_utf8(&device).map_err(|_| Error::Protocol)?.parse().map_err(|_| Error::Protocol)?,
                epoch: fields.u64()?, width: fields.u32()?, height: fields.u32()?, fps: fields.u32()?, bitrate: fields.u32()?,
            };
            config.frame_bytes()?;
            Message::Init(config)
        }
        2 => Message::Ready,
        kind @ (3 | 4) => {
            let stamp = fields.stamp()?;
            let flag = fields.boolean()?;
            let data = fields.0.to_vec();
            fields.0 = &[];
            if kind == 3 { Message::Encode { stamp, force_idr: flag, data } }
            else { Message::Output(EncodedFrame { stamp, unit: AccessUnit { bytes: data, is_idr: flag } }) }
        }
        5 => Message::Drain,
        6 => Message::Destroy,
        7 => Message::Exit,
        8 => Message::Ack,
        9 => Message::Failure(match fields.byte()? {
            0 => {
                let phase = [Phase::Startup, Phase::Probe, Phase::Initialize, Phase::Encode, Phase::Drain, Phase::Destroy, Phase::Exit]
                    .get(fields.byte()? as usize).copied().ok_or(Error::Protocol)?;
                let fault = [Fault::LibraryMissing, Fault::ApiUnsupported, Fault::DeviceUnavailable, Fault::CapabilityUnsupported, Fault::Driver, Fault::Busy, Fault::DelayedOutput, Fault::InvalidOutput]
                    .get(fields.byte()? as usize).copied().ok_or(Error::Protocol)?;
                Error::Backend { phase, fault, code: i32::from_le_bytes(fields.take()?) }
            }
            1 => Error::InvalidConfig,
            2 => Error::InvalidFrame,
            3 => Error::Closed,
            _ => return Err(Error::Protocol),
        }),
        _ => return Err(Error::Protocol),
    };
    if !fields.0.is_empty() { return Err(Error::Protocol); }
    Ok(result)
}

pub(crate) fn read(stream: &mut impl Read, limit: usize) -> Result<Message, Error> {
    let mut prefix = [0; 4];
    stream.read_exact(&mut prefix).map_err(|_| Error::Io)?;
    let length = u32::from_le_bytes(prefix) as usize;
    check_length(length, limit)?;
    let mut payload = vec![0; length];
    stream.read_exact(&mut payload).map_err(|_| Error::Io)?;
    decode(&payload)
}

pub(crate) fn write(stream: &mut impl Write, message: &Message) -> Result<(), Error> {
    stream.write_all(&encode(message)?).map_err(|_| Error::Io)
}
