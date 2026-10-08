use super::*;
use crate::{
    collection::{decode, rice_decode},
    filter::Filter,
    recordings::Recordings,
};

fn collection(key: u32, final_part: bool, samples: &[i16]) -> Vec<u8> {
    let mut pcm = 9997u32.to_le_bytes().to_vec();
    for sample in samples {
        pcm.extend(sample.to_le_bytes());
    }
    let mut records = vec![80];
    records.extend((pcm.len() as u32).to_le_bytes());
    records.extend(pcm);
    records.push(82);
    records.extend(6u16.to_le_bytes());
    records.extend(key.to_le_bytes());
    records.extend([1, final_part as u8]);
    let mut raw = ((records.len() + 4) as u32).to_le_bytes().to_vec();
    raw.extend(records);
    raw
}
#[test]
fn startup_preserves_first_active_recording() {
    assert_eq!(
        capture::startup_start(204, 683, (Some(682), true, false), false),
        682
    );
}
#[test]
fn startup_skips_old_final_recording() {
    assert_eq!(
        capture::startup_start(204, 683, (Some(682), true, true), false),
        683
    );
}
#[test]
fn startup_keeps_recording_released_while_connecting() {
    assert_eq!(
        capture::startup_start(206, 685, (Some(682), true, true), true),
        682
    );
}
#[test]
fn startup_preserves_wrapped_recording() {
    assert_eq!(
        capture::startup_start(65530, 2, (Some(65535), true, false), false),
        65535
    );
}
#[test]
fn startup_does_not_seek_before_retained_range() {
    assert_eq!(
        capture::startup_start(100, 200, (Some(99), true, false), false),
        200
    );
}
#[test]
fn all_collection_header_formats() {
    let raw = collection(123, true, &[-32768, -1, 0, 32767]);
    let records = &raw[4..];
    let mut be = vec![
        (records.len() >> 16) as u8,
        (records.len() >> 8) as u8,
        records.len() as u8,
    ];
    be.extend(records);
    let mut ff = vec![255];
    ff.extend((records.len() as u16).to_le_bytes());
    ff.extend(records);
    for input in [&raw, &be, &ff] {
        let item = decode(input).unwrap();
        assert_eq!(item.samples, Some(vec![-32768, -1, 0, 32767]));
        assert_eq!(item.start, Some(123));
        assert!(item.final_part);
    }
}
#[test]
fn truncated_and_malformed_collections_rejected() {
    let raw = collection(1, false, &[1, 2]);
    for length in 0..raw.len() {
        assert!(decode(&raw[..length]).is_err());
    }
    let mut bad = raw;
    bad[4] = 255;
    assert!(decode(&bad).is_err());
}
#[test]
fn rice_partial_codes_and_invalid_bit_count() {
    assert_eq!(rice_decode(&[0xff], 8, 0).unwrap(), vec![0; 8]);
    assert!(rice_decode(&[0], 9, 0).is_err());
    assert!(rice_decode(&[0], 8, 0).unwrap().is_empty());
}
#[test]
fn rice_negative_difference_wraps_at_large_shift() {
    assert_eq!(rice_decode(&[0b00011000], 5, 0xff).unwrap(), vec![-32768]);
}
#[test]
fn filter_keeps_state_across_chunks() {
    let samples: Vec<i16> = (0..20000).map(|n| ((n * 379) % 65536) as i16).collect();
    let expected = Filter::new(9997).process(&samples);
    let mut filter = Filter::new(9997);
    let actual: Vec<i16> = samples
        .chunks(253)
        .flat_map(|s| filter.process(s))
        .collect();
    assert_eq!(expected, actual);
}
#[test]
fn thousand_chunks_survive_retention_and_index_wrap() {
    let out = Output::new(false, None).unwrap();
    let mut recordings = Recordings::default();
    let mut all = Vec::new();
    let first = 65530u16;
    for n in 0..1000 {
        let index = first.wrapping_add(n);
        let events = recordings
            .add(
                index,
                &collection(first as u32, n == 999, &[n as i16]),
                &out,
            )
            .unwrap()
            .parts;
        for event in events {
            all.extend(event.samples.iter().copied());
            assert_eq!(event.final_part, n == 999);
        }
        assert!(
            recordings
                .retain(index.wrapping_sub(478), index.wrapping_add(1), &out)
                .unwrap()
                .lost
                .is_empty()
        );
    }
    assert_eq!(all, (0..1000).map(|n| n as i16).collect::<Vec<_>>());
}
#[test]
fn duplicates_and_out_of_order_chunks_are_not_repeated() {
    let out = Output::new(false, None).unwrap();
    let mut recordings = Recordings::default();
    let mut all = Vec::new();
    for n in [1, 0, 1, 2, 3] {
        for part in recordings
            .add(n, &collection(0, n == 3, &[n as i16]), &out)
            .unwrap()
            .parts
        {
            all.extend(part.samples.iter().copied());
        }
    }
    assert_eq!(all, vec![0, 1, 2, 3]);
}
#[test]
fn startup_boundary_skips_only_previous_recordings() {
    let out = Output::new(false, None).unwrap();
    let mut recordings = Recordings::default();
    recordings.reset(20).unwrap();
    assert!(
        recordings
            .add(19, &collection(19, true, &[1]), &out)
            .unwrap()
            .parts
            .is_empty()
    );
    assert_eq!(
        recordings
            .add(20, &collection(20, true, &[2]), &out)
            .unwrap()
            .parts[0]
            .samples
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![2]
    );
}
#[test]
fn lock_rejects_second_owner_and_releases() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("bluetooth.lock");
    let first = config::BluetoothLock::at(&path).unwrap();
    assert!(config::BluetoothLock::at(&path).is_err());
    drop(first);
    let second = config::BluetoothLock::at(&path).unwrap();
    drop(second);
    assert!(path.exists());
}
#[test]
fn log_contains_stdout_stderr_and_enables_debug() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("test.log");
    let output = Output::new(false, Some(&path)).unwrap();
    assert!(output.verbose);
    output.line("transcript");
    output.error("diagnostic");
    output.debug("debug marker");
    let content = std::fs::read_to_string(path).unwrap();
    for text in ["transcript", "diagnostic", "debug marker"] {
        assert!(content.contains(text));
    }
}
#[test]
fn final_transcript_printed_even_when_same_as_partial() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("test.log");
    let output = Output::new(false, Some(&path)).unwrap();
    output.transcript(" hello   world ", false);
    output.transcript("hello world", false);
    output.transcript("hello world", true);
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        "hello world\nhello world\n"
    );
}
#[test]
fn saved_address_format_compatible() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("device.json");
    config::save_json(&path, &json!({"address":"example"})).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(path).unwrap()).unwrap()["address"],
        "example"
    );
}
#[test]
fn manufacturer_payload_variants_match() {
    let raw = [255, 255, 185, 216, 248, 96];
    let prefixed = [0, 0, 255, 255, 185, 216, 248, 96];
    let a = bluetooth::advertisement(&raw).unwrap();
    let b = bluetooth::advertisement(&prefixed).unwrap();
    assert_eq!(a.fingerprint, 3636068351);
    assert!(a.in_collection_state);
    assert_eq!(
        serde_json::to_value(a).unwrap(),
        serde_json::to_value(b).unwrap()
    );
    assert!(bluetooth::advertisement(&[1]).is_err());
}
