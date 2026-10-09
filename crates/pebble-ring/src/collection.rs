use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub fn u32le(data: &[u8]) -> u32 {
    u32::from_le_bytes(data[..4].try_into().unwrap())
}
pub fn rice_decode(data: &[u8], bit_count: usize, parameters: u8) -> Result<Vec<i16>> {
    ensure!(
        bit_count <= data.len() * 8,
        "Compressed bit count exceeds payload"
    );
    let shift = parameters & 15;
    let limit = parameters >> 4;
    let mut position = 0;
    let mut bits = |count: usize| -> Option<u16> {
        if position + count > bit_count {
            return None;
        }
        let mut value = 0;
        for _ in 0..count {
            value = (value << 1) | ((data[position / 8] >> (7 - position % 8)) & 1) as u16;
            position += 1;
        }
        Some(value)
    };
    let mut word = 0u16;
    let mut delta = 0u16;
    let mut output = Vec::new();
    while let Some(flag) = bits(1) {
        let mut difference = 0u16;
        if flag == 0 {
            let mut magnitude = 1u16;
            let mut short = false;
            while magnitude < limit as u16 {
                match bits(1) {
                    Some(1) => {
                        short = true;
                        break;
                    }
                    Some(_) => magnitude += 1,
                    None => return Ok(output),
                }
            }
            if short {
                let Some(sign) = bits(1) else { break };
                difference = if sign == 0 {
                    magnitude
                } else {
                    (1u32 << (16 - shift)).wrapping_sub(magnitude as u32) as u16
                };
            } else {
                let Some(value) = bits((16 - shift) as usize) else {
                    break;
                };
                difference = value;
            }
        }
        delta = delta.wrapping_add(difference);
        word = word.wrapping_add(delta);
        output.push(word.wrapping_shl(shift as u32) as i16);
    }
    Ok(output)
}
pub fn records(raw: &[u8]) -> Result<BTreeMap<u8, &[u8]>> {
    ensure!(raw.len() >= 3, "Truncated collection header");
    let (declared, mut offset, expected) = if raw.len() > 3 && raw[3] == 0 {
        (
            (raw[0] as usize) | ((raw[1] as usize) << 8) | ((raw[2] as usize) << 16),
            4,
            raw.len(),
        )
    } else {
        let n = if raw[0] == 255 {
            u16::from_le_bytes([raw[1], raw[2]]) as usize
        } else {
            ((raw[0] as usize) << 16) | ((raw[1] as usize) << 8) | raw[2] as usize
        };
        (n, 3, raw.len() - 3)
    };
    ensure!(
        declared == expected,
        "Collection length mismatch: {declared} != {expected}"
    );
    let mut result = BTreeMap::new();
    while offset < raw.len() {
        let id = raw[offset];
        let width = if id == 80 || id == 81 { 4 } else { 2 };
        ensure!(
            (1..=84).contains(&id) && offset + 1 + width <= raw.len(),
            "Invalid or truncated collection record"
        );
        let length = if width == 4 {
            u32le(&raw[offset + 1..]) as usize
        } else {
            u16::from_le_bytes(raw[offset + 1..offset + 3].try_into().unwrap()) as usize
        };
        let start = offset + 1 + width;
        let end = start
            .checked_add(length)
            .context("Record length overflow")?;
        ensure!(end <= raw.len(), "Record extends beyond collection");
        ensure!(
            result.insert(id, &raw[start..end]).is_none(),
            "Duplicate collection record {id}"
        );
        offset = end;
    }
    Ok(result)
}
pub fn metadata(raw: &[u8]) -> Result<(Option<u32>, bool, bool)> {
    metadata_records(&records(raw)?)
}
fn metadata_records(records: &BTreeMap<u8, &[u8]>) -> Result<(Option<u32>, bool, bool)> {
    if let Some(p) = records.get(&82) {
        ensure!(p.len() >= 6, "Truncated audio metadata");
        Ok((Some(u32le(p)), p[4] != 0, p[5] != 0))
    } else {
        Ok((None, false, false))
    }
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Collection {
    pub samples: Option<Vec<i16>>,
    pub rate: Option<u32>,
    pub start: Option<u32>,
    pub multipart: bool,
    pub final_part: bool,
    pub buttons: Option<Vec<String>>,
    pub lifetime_count: Option<u32>,
    /// Diagnostic headers from the same validated TLV pass as the PCM.
    pub headers: BTreeMap<u8, Vec<u8>>,
}
pub fn decode(raw: &[u8]) -> Result<Collection> {
    let records = records(raw)?;
    let (start, multipart, final_part) = metadata_records(&records)?;
    let (samples, rate) = if let Some(p) = records.get(&80) {
        ensure!(p.len() >= 4 && (p.len() - 4) % 2 == 0, "Invalid PCM record");
        (
            Some(
                p[4..]
                    .chunks_exact(2)
                    .map(|b| i16::from_le_bytes([b[0], b[1]]))
                    .collect(),
            ),
            Some(u32le(p)),
        )
    } else if let Some(p) = records.get(&81) {
        ensure!(p.len() >= 9, "Truncated compressed audio record");
        (
            Some(rice_decode(&p[9..], u32le(&p[1..]) as usize, p[0])?),
            Some(u32le(&p[5..])),
        )
    } else {
        (None, None)
    };
    if let Some(rate) = rate {
        ensure!(
            (1000..=192000).contains(&rate),
            "Invalid audio sample rate: {rate}"
        );
    }
    let buttons = if let Some(p) = records.get(&83) {
        ensure!(p.len() >= 8, "Truncated button sequence");
        let pattern = u32le(p);
        let count = u32le(&p[4..]);
        ensure!(count <= 32, "Button sequence exceeds its 32-bit pattern");
        Some(
            (0..count)
                .map(|i| {
                    if pattern & (1 << i) != 0 {
                        "long".into()
                    } else {
                        "short".into()
                    }
                })
                .collect(),
        )
    } else {
        None
    };
    let lifetime_count = records
        .get(&84)
        .map(|p| -> Result<u32> {
            ensure!(p.len() >= 4, "Truncated lifetime count");
            Ok(u32le(p))
        })
        .transpose()?;
    let headers = records
        .iter()
        .filter(|(id, _)| **id != 80 && **id != 81)
        .map(|(id, bytes)| (*id, bytes.to_vec()))
        .collect();
    Ok(Collection {
        samples,
        rate,
        start,
        multipart,
        final_part,
        buttons,
        lifetime_count,
        headers,
    })
}
