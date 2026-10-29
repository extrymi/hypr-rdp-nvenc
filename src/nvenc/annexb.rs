//! Bounded H.264 framing/parameter checks for the fixed progressive AVC420 path.
//! This is not a replacement for decoder/hardware conformance qualification.
use super::{AccessUnit, Error, OutputCheck, MAX_OUTPUT_BYTES};

fn invalid() -> Error {
    Error::invalid_output(OutputCheck::AnnexB)
}

fn refused(check: OutputCheck) -> Error {
    Error::invalid_output(check)
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
    // PPS entropy_coding_mode_flag: only CABAC slices may end in cabac_zero_word.
    cabac: bool,
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
                        return Err(refused(OutputCheck::Geometry));
                    }
                    fresh_sps = Some((id, nal.to_vec()));
                }
                8 => {
                    if slice.is_some() || fresh_pps.is_some() {
                        return Err(invalid());
                    }
                    let mut bits = Bits::new(nal, false)?;
                    let pps_id = bits.ue()?;
                    let sps_id = bits.ue()?;
                    let cabac = bits.take(1)? == 1;
                    if pps_id > 255 || sps_id > 31 {
                        return Err(invalid());
                    }
                    fresh_pps = Some((pps_id, sps_id, cabac, nal.to_vec()));
                }
                kind @ (1 | 5) => {
                    if fresh_sps.is_some() || fresh_pps.is_some() {
                        let (sps_id, sps) = fresh_sps.take().ok_or_else(invalid)?;
                        let (pps_id, referenced_sps, cabac, pps) =
                            fresh_pps.take().ok_or_else(invalid)?;
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
                            cabac,
                        });
                    }
                    let p = parameters.as_ref().ok_or_else(invalid)?;
                    if p.width != width || p.height != height {
                        return Err(refused(OutputCheck::Geometry));
                    }
                    if p.sps_id > 31 {
                        return Err(invalid());
                    }
                    let mut bits = Bits::new(nal, p.cabac)?;
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
            return Err(refused(OutputCheck::IdrAgreement));
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
    /// `cabac_slice`: H.264 7.4.1/7.3.2.10 append a final emulation prevention
    /// byte only when a CABAC slice RBSP ends in cabac_zero_word (0x0000).
    fn new(nal: &[u8], cabac_slice: bool) -> Result<Self, Error> {
        let mut rbsp = Vec::with_capacity(nal.len());
        let mut zeros = 0;
        for (index, &byte) in nal[1..].iter().enumerate() {
            if zeros >= 2 && byte == 3 {
                match nal.get(index + 2) {
                    Some(next) if *next <= 3 => {}
                    None if cabac_slice => {}
                    _ => return Err(refused(OutputCheck::EmulationPrevention)),
                }
                zeros = 0;
                continue;
            }
            if zeros >= 2 && byte <= 2 {
                return Err(refused(OutputCheck::EmulationPrevention));
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
    let mut b = Bits::new(nal, false)?;
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
    // The encoder's fixed geometry needs only padding to the next macroblock.
    // A tiny displayed crop must not hide an oversized coded allocation from
    // the decoder; bound coded dimensions as well as the displayed dimensions.
    if w > 8192 || h > 8192 || crop_x >= 16 || crop_y >= 16 {
        return Err(refused(OutputCheck::Geometry));
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

#[cfg(test)]
mod tests {
    #[test]
    fn oversized_coded_geometry_cannot_hide_behind_a_small_display_crop() {
        // Baseline SPS fixtures describe 16384x64 and 512x64 coded pictures,
        // both cropped to 64x64. Neither can come from this fixed 64x64 path.
        for nal in [
            &b"\x67\x42\x00\x29\xf4\x00\x20\x01\x3c\x00\x3f\xc3\xa0"[..],
            &b"\x67\x42\x00\x29\xf4\x04\x04\xf0\x1c\x3a"[..],
        ] {
            assert_eq!(
                super::sps(nal),
                Err(super::Error::invalid_output(super::OutputCheck::Geometry)),
                "{nal:02x?}"
            );
        }
    }

    /// Independent NAL encapsulation from H.264 (V16) 7.4.1, step 1: insert 0x03
    /// before 0x00..=0x03 after two zero bytes, then append a final 0x03 when
    /// the RBSP ends in 0x00. It shares no code with `Bits::new`.
    fn encapsulate(header: u8, rbsp: &[u8]) -> Vec<u8> {
        let mut nal = vec![header];
        let mut zeros = 0;
        for &byte in rbsp {
            if zeros >= 2 && byte <= 3 {
                nal.push(3);
                zeros = 0;
            }
            nal.push(byte);
            zeros = if byte == 0 { zeros + 1 } else { 0 };
        }
        if rbsp.last() == Some(&0) {
            nal.push(3);
        }
        nal
    }

    #[test]
    fn final_emulation_byte_is_accepted_only_after_a_cabac_slice_zero_word() {
        // 0x80: rbsp_stop_one_bit plus alignment; then k cabac_zero_words.
        for k in 1..=3 {
            let rbsp = [&[0x88, 0x84, 0x80][..], &vec![0; 2 * k][..]].concat();
            for header in [0x65, 0x41] {
                let nal = encapsulate(header, &rbsp);
                assert!(nal.ends_with(&[0, 0, 3]));
                assert_eq!(super::Bits::new(&nal, true).unwrap().rbsp, rbsp);
                assert!(super::Bits::new(&nal, false).is_err(), "CAVLC/SPS/PPS");
            }
        }
    }

    #[test]
    fn invalid_or_truncated_emulation_sequences_stay_rejected() {
        for nal in [
            &[0x65, 0x88, 0x00, 0x00, 0x03, 0x04][..], // 0x000003 + byte > 3 (7.4.1)
            &[0x65, 0x88, 0x00, 0x00, 0x00, 0x80],     // 0x000000
            &[0x65, 0x88, 0x00, 0x00, 0x01, 0x80],     // 0x000001
            &[0x65, 0x88, 0x00, 0x00, 0x02, 0x80],     // 0x000002
            &[0x65, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00], // escaped, then raw 0x000000
            &[0x65],                                   // empty RBSP
        ] {
            assert!(super::Bits::new(nal, true).is_err(), "{nal:02x?}");
        }
        // A trailing 0x03 after a single zero is data, not an emulation byte.
        let data = super::Bits::new(&[0x65, 0x88, 0x00, 0x03], false).unwrap();
        assert_eq!(data.rbsp, [0x88, 0x00, 0x03]);
    }

    proptest::proptest! {
        #[test]
        fn encapsulated_rbsp_round_trips(
            body in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..256),
            stop in 1u8..=255,
            zero_words in 0usize..4,
        ) {
            // A slice RBSP ends in a nonzero trailing-bits byte, then cabac_zero_words.
            let rbsp = [&body[..], &[stop][..], &vec![0; 2 * zero_words][..]].concat();
            let nal = encapsulate(0x65, &rbsp);
            proptest::prop_assert_eq!(super::Bits::new(&nal, true).unwrap().rbsp, rbsp.clone());
            proptest::prop_assert_eq!(super::Bits::new(&nal, false).is_ok(), zero_words == 0);
        }
    }
}

/// VALIDATION-PLAN B3-2: an independent encoder's CABAC stream that an
/// independent decoder decodes identically with and without zero-word tails.
#[cfg(test)]
mod independent_cabac_stream {
    #[test]
    fn every_access_unit_is_accepted_and_keeps_its_zero_word_tail() {
        // libx264 High profile, 64x64, IDR + 2 P; cabac-zwK.264 appends
        // (00 00 03)*K to every slice. Provenance: tests/fixtures/h264-cabac.
        let streams: [(usize, &[u8]); 3] = [
            (
                0,
                include_bytes!("../../tests/fixtures/h264-cabac/cabac.264"),
            ),
            (
                1,
                include_bytes!("../../tests/fixtures/h264-cabac/cabac-zw1.264"),
            ),
            (
                2,
                include_bytes!("../../tests/fixtures/h264-cabac/cabac-zw2.264"),
            ),
        ];
        for (k, stream) in streams {
            let mut annexb = super::AnnexB::default();
            let mut unit = Vec::new();
            let mut frames = 0;
            for nal in super::split(stream).unwrap() {
                unit.extend_from_slice(&[0, 0, 0, 1]);
                unit.extend_from_slice(nal);
                if !matches!(nal[0] & 31, 1 | 5) {
                    continue;
                }
                let idr = nal[0] & 31 == 5;
                let output = annexb
                    .normalize(&unit, 64, 64, frames == 0, idr)
                    .unwrap_or_else(|e| panic!("k={k} frame={frames}: {e:?}"));
                assert_eq!(output.is_idr, idr, "k={k} frame={frames}");
                let slices: Vec<_> = super::split(&output.bytes)
                    .unwrap()
                    .into_iter()
                    .filter(|n| matches!(n[0] & 31, 1 | 5))
                    .collect();
                assert_eq!(slices.len(), 1, "k={k} frame={frames}");
                assert!(nal.ends_with(&[0u8, 0, 3].repeat(k)), "k={k}: fixture tail");
                assert_eq!(
                    slices[0], nal,
                    "k={k} frame={frames}: slice and zero-word tail must pass unchanged"
                );
                unit.clear();
                frames += 1;
            }
            assert_eq!(frames, 3, "k={k}");
        }
    }
}
