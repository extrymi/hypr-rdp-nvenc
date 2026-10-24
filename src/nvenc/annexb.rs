//! Bounded H.264 framing/parameter checks for the fixed progressive AVC420 path.
//! This is not a replacement for decoder/hardware conformance qualification.
use super::{AccessUnit, Error, Fault, Phase, MAX_OUTPUT_BYTES};

fn invalid() -> Error {
    Error::backend(Phase::Encode, Fault::InvalidOutput)
}

#[derive(Default)]
pub(super) struct AnnexB {
    parameters: Option<Parameters>,
}

#[derive(Clone, PartialEq, Eq)]
struct Parameters {
    sps: Vec<u8>,
    pps: Vec<u8>,
    sps_id: u32,
    pps_id: u32,
    width: u32,
    height: u32,
}

impl AnnexB {
    pub fn normalize(
        &mut self,
        data: &[u8],
        width: u32,
        height: u32,
        force_idr: bool,
        driver_idr: bool,
    ) -> Result<AccessUnit, Error> {
        if data.is_empty() || data.len() > MAX_OUTPUT_BYTES {
            return Err(invalid());
        }
        let nals = split(data)?;
        let mut parameters = self.parameters.clone();
        let mut fresh_sps = None;
        let mut fresh_pps = None;
        let mut slice = None;
        let mut last_mb = None;
        let mut aud = false;
        for nal in &nals {
            if nal[0] & 0x80 != 0 {
                return Err(invalid());
            }
            match nal[0] & 31 {
                7 => {
                    if slice.is_some() || fresh_sps.is_some() {
                        return Err(invalid());
                    }
                    let (id, w, h) = sps(nal)?;
                    if w != width || h != height {
                        return Err(invalid());
                    }
                    fresh_sps = Some((id, nal.to_vec()));
                }
                8 => {
                    if slice.is_some() || fresh_pps.is_some() {
                        return Err(invalid());
                    }
                    let mut bits = Bits::new(nal)?;
                    let pps_id = bits.ue()?;
                    let sps_id = bits.ue()?;
                    if pps_id > 255 || sps_id > 31 {
                        return Err(invalid());
                    }
                    fresh_pps = Some((pps_id, sps_id, nal.to_vec()));
                }
                kind @ (1 | 5) => {
                    if fresh_sps.is_some() || fresh_pps.is_some() {
                        let (sps_id, sps) = fresh_sps.take().ok_or_else(invalid)?;
                        let (pps_id, referenced_sps, pps) = fresh_pps.take().ok_or_else(invalid)?;
                        if referenced_sps != sps_id {
                            return Err(invalid());
                        }
                        parameters = Some(Parameters {
                            sps,
                            pps,
                            sps_id,
                            pps_id,
                            width,
                            height,
                        });
                    }
                    let p = parameters.as_ref().ok_or_else(invalid)?;
                    if p.width != width || p.height != height || p.sps_id > 31 {
                        return Err(invalid());
                    }
                    let mut bits = Bits::new(nal)?;
                    let first_mb = bits.ue()?;
                    let slice_type = bits.ue()?;
                    let pps = bits.ue()?;
                    let idr = kind == 5;
                    if pps != p.pps_id
                        || slice_type > 9
                        || ![0, 2].contains(&(slice_type % 5))
                        || (idr && (nal[0] & 0x60 == 0 || slice_type % 5 != 2))
                        || first_mb as u64
                            >= (width as u64).div_ceil(16) * (height as u64).div_ceil(16)
                        || last_mb.map_or(first_mb != 0, |last| first_mb <= last)
                        || slice.is_some_and(|previous| previous != idr)
                    {
                        return Err(invalid());
                    }
                    last_mb = Some(first_mb);
                    slice = Some(idr);
                }
                9 => {
                    if aud || slice.is_some() {
                        return Err(invalid());
                    }
                    aud = true;
                }
                6 | 12 => {} // SEI/filler are retained, never treated as slices.
                _ => return Err(invalid()),
            }
        }
        if fresh_sps.is_some() || fresh_pps.is_some() {
            return Err(invalid());
        }
        let is_idr = slice.ok_or_else(invalid)?;
        if is_idr != driver_idr || (force_idr && !is_idr) {
            return Err(invalid());
        }
        // A recovery/first IDR must carry this generation's parameter sets,
        // rather than borrowing stale state from a previous encoder instance.
        let p = parameters.ok_or_else(invalid)?;
        if !is_idr && self.parameters.as_ref() != Some(&p) {
            return Err(invalid());
        }
        let contains_sps = nals.iter().any(|n| n[0] & 31 == 7);
        let contains_pps = nals.iter().any(|n| n[0] & 31 == 8);
        if is_idr && !(contains_sps && contains_pps) {
            return Err(invalid());
        }
        let mut output = Vec::new();
        // Keep AUD first even when repeating the Windows parameter-set prefix.
        for nal in &nals {
            if nal[0] & 31 == 9 {
                append(&mut output, nal)?;
            }
        }
        // Upstream's Windows MFT workaround: repeat cached SPS/PPS on P frames.
        if !is_idr && !contains_sps {
            append(&mut output, &p.sps)?;
            append(&mut output, &p.pps)?;
        }
        for nal in nals {
            if nal[0] & 31 != 9 {
                append(&mut output, nal)?;
            }
        }
        self.parameters = Some(p);
        Ok(AccessUnit {
            bytes: output,
            is_idr,
        })
    }
}

fn append(output: &mut Vec<u8>, nal: &[u8]) -> Result<(), Error> {
    if output
        .len()
        .checked_add(nal.len())
        .and_then(|n| n.checked_add(4))
        .is_none_or(|n| n > MAX_OUTPUT_BYTES)
    {
        return Err(invalid());
    }
    output.extend_from_slice(&[0, 0, 0, 1]);
    output.extend_from_slice(nal);
    Ok(())
}

pub(super) fn split(data: &[u8]) -> Result<Vec<&[u8]>, Error> {
    let mut starts = Vec::new();
    let mut offset = 0;
    while offset + 3 <= data.len() {
        let prefix = if data[offset..].starts_with(&[0, 0, 0, 1]) {
            4
        } else if data[offset..].starts_with(&[0, 0, 1]) {
            3
        } else {
            offset += 1;
            continue;
        };
        starts.push((offset, prefix));
        if starts.len() > 4096 {
            return Err(invalid());
        }
        offset += prefix;
    }
    let first = starts.first().ok_or_else(invalid)?.0;
    if data[..first].iter().any(|b| *b != 0) {
        return Err(invalid());
    }
    let mut nals = Vec::with_capacity(starts.len());
    for (index, &(start, prefix)) in starts.iter().enumerate() {
        let mut end = starts.get(index + 1).map_or(data.len(), |next| next.0);
        while end > start + prefix && data[end - 1] == 0 {
            end -= 1;
        }
        let nal = &data[start + prefix..end];
        if nal.len() < 2 {
            return Err(invalid());
        }
        nals.push(nal);
    }
    Ok(nals)
}

struct Bits {
    rbsp: Vec<u8>,
    position: usize,
}
impl Bits {
    fn new(nal: &[u8]) -> Result<Self, Error> {
        let mut rbsp = Vec::with_capacity(nal.len());
        let mut zeros = 0;
        for (index, &byte) in nal[1..].iter().enumerate() {
            if zeros >= 2 && byte == 3 {
                if nal.get(index + 2).is_none_or(|next| *next > 3) {
                    return Err(invalid());
                }
                zeros = 0;
                continue;
            }
            if zeros >= 2 && byte <= 2 {
                return Err(invalid());
            }
            rbsp.push(byte);
            zeros = if byte == 0 { zeros + 1 } else { 0 };
        }
        if rbsp.is_empty() {
            return Err(invalid());
        }
        Ok(Self { rbsp, position: 0 })
    }
    fn take(&mut self, count: usize) -> Result<u32, Error> {
        if count > 32 || self.position + count > self.rbsp.len() * 8 {
            return Err(invalid());
        }
        let mut value = 0;
        for _ in 0..count {
            value = (value << 1)
                | ((self.rbsp[self.position / 8] >> (7 - self.position % 8)) & 1) as u32;
            self.position += 1;
        }
        Ok(value)
    }
    fn ue(&mut self) -> Result<u32, Error> {
        let mut zeros = 0;
        while self.take(1)? == 0 {
            zeros += 1;
            if zeros > 30 {
                return Err(invalid());
            }
        }
        Ok((1 << zeros) - 1 + self.take(zeros)?)
    }
    fn se(&mut self) -> Result<i32, Error> {
        let value = self.ue()?;
        Ok(if value.is_multiple_of(2) {
            -(value as i32 / 2)
        } else {
            value.div_ceil(2) as i32
        })
    }
}

fn sps(nal: &[u8]) -> Result<(u32, u32, u32), Error> {
    let mut b = Bits::new(nal)?;
    let profile = b.take(8)?;
    b.take(16)?; // Constraint flags and level_idc.
    let id = b.ue()?;
    if id > 31 {
        return Err(invalid());
    }
    if [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135].contains(&profile) {
        if b.ue()? != 1 || b.ue()? != 0 || b.ue()? != 0 {
            return Err(invalid());
        }
        b.take(1)?;
        if b.take(1)? != 0 {
            for list in 0..8 {
                if b.take(1)? == 0 {
                    continue;
                }
                let (mut last, mut next) = (8, 8);
                for _ in 0..if list < 6 { 16 } else { 64 } {
                    if next != 0 {
                        next = (last + b.se()?).rem_euclid(256);
                    }
                    if next != 0 {
                        last = next;
                    }
                }
            }
        }
    } else if ![66, 77, 88].contains(&profile) {
        return Err(invalid());
    }
    if b.ue()? > 12 {
        return Err(invalid());
    }
    match b.ue()? {
        0 => {
            if b.ue()? > 12 {
                return Err(invalid());
            }
        }
        1 => {
            b.take(1)?;
            b.se()?;
            b.se()?;
            let count = b.ue()?;
            if count > 255 {
                return Err(invalid());
            }
            for _ in 0..count {
                b.se()?;
            }
        }
        2 => {}
        _ => return Err(invalid()),
    }
    if b.ue()? > 16 {
        return Err(invalid());
    }
    b.take(1)?;
    let w = b
        .ue()?
        .checked_add(1)
        .and_then(|v| v.checked_mul(16))
        .ok_or_else(invalid)?;
    let h = b
        .ue()?
        .checked_add(1)
        .and_then(|v| v.checked_mul(16))
        .ok_or_else(invalid)?;
    if b.take(1)? != 1 {
        return Err(invalid());
    } // Progressive only.
    b.take(1)?;
    let (mut crop_x, mut crop_y) = (0u32, 0u32);
    if b.take(1)? != 0 {
        crop_x = b
            .ue()?
            .checked_add(b.ue()?)
            .and_then(|v| v.checked_mul(2))
            .ok_or_else(invalid)?;
        crop_y = b
            .ue()?
            .checked_add(b.ue()?)
            .and_then(|v| v.checked_mul(2))
            .ok_or_else(invalid)?;
    }
    let width = w
        .checked_sub(crop_x)
        .filter(|v| *v > 0)
        .ok_or_else(invalid)?;
    let height = h
        .checked_sub(crop_y)
        .filter(|v| *v > 0)
        .ok_or_else(invalid)?;
    Ok((id, width, height))
}
