use crate::{
    Listen,
    bluetooth::{Bluetooth, RingState, advertised_state},
    collection::metadata,
    output::Output,
    recognition::{Client, Options, display_event},
};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;

struct Received {
    next: Option<u16>,
    initialized: bool,
    initial_count: Option<u8>,
    outbound: Option<mpsc::UnboundedSender<Value>>,
    output: Output,
    collecting: Option<bool>,
    inactive_since: Option<Instant>,
    raw_press_since: Option<Instant>,
}
impl Received {
    fn send(&self, event: Value) -> Result<()> {
        if let Some(tx) = &self.outbound {
            tx.send(event).context("Recognition worker closed")?;
        }
        Ok(())
    }
    fn state(&mut self, state: &RingState) -> Result<()> {
        if state.in_collection_state && self.raw_press_since.is_none() {
            self.raw_press_since = Some(Instant::now());
            self.output
                .debug("Button timing: collecting rising edge (receiver observation)");
        } else if !state.in_collection_state
            && let Some(start) = self.raw_press_since.take()
        {
            self.output.debug(format!("Button timing: collecting falling edge; observed_ms={}; source=ring_state; not physical button duration", start.elapsed().as_millis()));
        }
        self.send(json!({"type":"button_state","pressed":state.in_collection_state}))?;
        if state.in_collection_state {
            self.inactive_since = None;
        } else if self.collecting == Some(true) {
            let since = self.inactive_since.get_or_insert_with(Instant::now);
            if since.elapsed() < Duration::from_millis(250) {
                return Ok(());
            }
        }
        if self.collecting != Some(state.in_collection_state) {
            self.output.debug(format!("Recording state edge {:?} -> {}; inactive_ms={:?}; next_collection={:?}; state={state:?}", self.collecting, state.in_collection_state, self.inactive_since.map(|t| t.elapsed().as_millis()), self.next));
            // The input adapter emits the logical recording state after its
            // continuation grace period. Raw edges stay in the debug log.
            self.collecting = Some(state.in_collection_state);
        }
        self.send(json!({"type":"state","collecting":state.in_collection_state}))
    }
}
pub fn startup_start(
    start: u16,
    end: u16,
    metadata: (Option<u32>, bool, bool),
    finished_since_advertisement: bool,
) -> u16 {
    let (first, multipart, final_part) = metadata;
    if multipart
        && (!final_part || finished_since_advertisement)
        && let Some(first) = first
    {
        let first = first as u16;
        if first.wrapping_sub(start) < end.wrapping_sub(start) {
            return first;
        }
    }
    end
}
async fn download(
    ble: &mut Bluetooth,
    received: &mut Received,
    args: &Listen,
    output: &Output,
    fetch: bool,
) -> Result<RingState> {
    let mut cached = HashMap::new();
    let mut state = ble.state(args.timeout).await?;
    received.state(&state)?;
    let mut polled = Instant::now();
    let (mut start, mut end) = ble.range(args.timeout).await?;
    loop {
        if !received.initialized {
            received.next = Some(end);
            let initial_active = state.in_collection_state;
            let initial_count = state.collection_count;
            state = ble.state(args.timeout).await?;
            received.state(&state)?;
            let observed_active = initial_active || received.initial_count.is_some();
            if (state.in_collection_state || observed_active) && start != end {
                let latest = end.wrapping_sub(1);
                let raw = ble.collection(latest, args.timeout).await?;
                let baseline = received.initial_count.or(if initial_active {
                    Some(initial_count)
                } else {
                    None
                });
                let first = startup_start(
                    start,
                    end,
                    metadata(&raw)?,
                    baseline.is_some_and(|n| n != end as u8),
                );
                received.next = Some(first);
                if first != end {
                    cached.insert(latest, raw);
                }
            }
            received.initialized = true;
            received.send(json!({"type":"boundary","index":received.next.unwrap()}))?;
            output.line("BLE接続・初期化完了。録音中ならそのまま話し続けてください。");
            output.debug(format!("Startup flush: skipped existing collections {start}..{}; active recording preserved={}",received.next.unwrap(),received.next!=Some(end)));
        }
        let cursor = received
            .next
            .filter(|n| n.wrapping_sub(start) <= end.wrapping_sub(start))
            .unwrap_or(start);
        output.debug(format!(
            "Collection range: {start}..{end}; downloading from {cursor}"
        ));
        for step in 0..end.wrapping_sub(cursor) {
            let index = cursor.wrapping_add(step);
            let began = Instant::now();
            let raw = if let Some(raw) = cached.remove(&index) {
                raw
            } else {
                ble.collection(index, args.timeout).await?
            };
            output.debug(format!(
                "collection={index} transfer took {:.3}s",
                began.elapsed().as_secs_f64()
            ));
            received.send(json!({"type":"collection","index":index,"raw":STANDARD.encode(raw)}))?;
            received.next = Some(index.wrapping_add(1));
            // Keep release detection responsive while recording. Once released,
            // give the backlog most of the link instead of polling every packet.
            let poll_interval = if received.collecting == Some(true) {
                Duration::from_millis(250)
            } else {
                Duration::from_secs(1)
            };
            if polled.elapsed() >= poll_interval {
                state = ble.state(args.timeout).await?;
                received.state(&state)?;
                output.debug(format!("ring_state during transfer={state:?}"));
                polled = Instant::now();
            }
        }
        received.next = Some(end);
        received.send(json!({"type":"range","start":start,"end":end}))?;
        // The last collection may have just polled state. Do not immediately
        // spend another BLE round trip reading the same value.
        if polled.elapsed() >= Duration::from_millis(250) {
            state = ble.state(args.timeout).await?;
            received.state(&state)?;
            polled = Instant::now();
            output.debug(format!("ring_state={state:?}"));
        }
        let (new_start, new_end) = ble.range(args.timeout).await?;
        if new_end != end {
            output.debug(format!(
                "Range advanced during transfer: {end} -> {new_end}; keep downloading"
            ));
            start = new_start;
            end = new_end;
            continue;
        }
        if fetch && !state.in_collection_state {
            return Ok(state);
        }
        received.send(json!({"type":"caught_up"}))?;
        start = new_start;
        end = new_end;
        // The range read itself takes time. When data was consumed this turn,
        // immediately check again; only back off on an empty pass.
        if cursor == end {
            tokio::time::sleep(Duration::from_secs_f64(args.interval)).await;
        }
    }
}
async fn receive(
    args: Listen,
    address: String,
    fetch: bool,
    outbound: Option<mpsc::UnboundedSender<Value>>,
    output: Output,
) -> Result<()> {
    let mut ble = Bluetooth::start(output.clone()).await?;
    output.line(if outbound.is_some() {
        "音声認識の準備完了・Bluetooth待機中。リングを長押しして話してください。"
    } else {
        "Bluetooth準備完了・Bluetooth待機中。リングを長押しして話してください。"
    });
    let mut received = Received {
        next: None,
        initialized: fetch,
        initial_count: None,
        outbound,
        output: output.clone(),
        collecting: None,
        inactive_since: None,
        raw_press_since: None,
    };
    output.event(&json!({"type":"ready"}));
    let mut paired = false;
    let mut reconnect_remaining = 0u8;
    loop {
        let direct = reconnect_remaining > 0;
        let device = if direct {
            reconnect_remaining -= 1;
            output.debug(format!("Direct reconnect to known Index; attempts_left={reconnect_remaining}; resume_collection={:?}", received.next));
            // Do not reuse cached manufacturer data as a fresh button edge.
            json!({"address":address})
        } else {
            output.debug("Scanning for Index");
            ble.request(json!({"type":"find","address":address}), args.timeout)
                .await?
        };
        if device.is_null() {
            if fetch {
                bail!("Index not advertising; press ring button");
            }
            continue;
        }
        output.debug(format!("Discovered: {device}"));
        if let Some(state) = advertised_state(&device) {
            if state.in_collection_state || received.collecting != Some(true) {
                received.state(&state)?;
            }
            if !received.initialized
                && state.in_collection_state
                && received.initial_count.is_none()
            {
                received.initial_count = Some(state.collection_count);
            }
        }
        let result = async {
            let started = Instant::now();
            output.debug(format!("Connecting: {address}"));
            let pair = !paired || args.pair;
            ble.connect(
                &address,
                pair,
                if direct {
                    args.timeout.min(5.0)
                } else if pair {
                    args.timeout
                } else {
                    args.timeout.min(8.0)
                },
            )
            .await?;
            paired = true;
            output.debug(format!(
                "Connected: {address}; connection took {:.3}s",
                started.elapsed().as_secs_f64()
            ));
            ble.subscribe(args.timeout).await?;
            reconnect_remaining = 3;
            download(&mut ble, &mut received, &args, &output, fetch).await
        }
        .await;
        if result.is_err() {
            received.send(json!({"type":"connection_lost"}))?;
        }
        ble.unsubscribe().await;
        ble.disconnect().await;
        match result {
            Ok(_) => {
                if fetch {
                    ble.close().await;
                    return Ok(());
                }
            }
            Err(error) => {
                let text = format!("{error:#}");
                if fetch
                    || crate::bluetooth::encryption_rejected(&text)
                    || crate::bluetooth::pairing_removed(&text)
                    || text.contains("Telesto read failed")
                    || text.contains("Invalid")
                    || text.contains("length mismatch")
                    || text.contains("exceeds")
                    || text.contains("too large")
                    || text.contains("Truncated")
                {
                    return Err(error);
                }
                output.debug(format!(
                    "Bluetooth connection/scan failed: {error:#}; resume from collection={:?}",
                    received.next
                ));
            }
        }
        tokio::time::sleep(Duration::from_secs_f64(args.interval)).await;
    }
}
pub async fn run(args: Listen, fetch: bool, output: Output) -> Result<()> {
    let address = args
        .address
        .clone()
        .map(Ok)
        .unwrap_or_else(crate::config::load_address)?;
    let mut speech = if args.transcription(fetch) {
        Some(
            Client::start(
                Options {
                    address: address.clone(),
                    language: args.language.clone(),
                    command: if fetch { "fetch" } else { "listen" }.into(),
                    verbose: output.verbose,
                },
                output.clone(),
            )
            .await?,
        )
    } else {
        None
    };
    let outbound = speech.as_ref().map(|s| s.outbound.clone());
    let receiving = receive(args, address, fetch, outbound, output.clone());
    tokio::pin!(receiving);
    loop {
        if let Some(speech) = &mut speech {
            tokio::select! {
                result=&mut receiving=>{result?;speech.flush(&output).await?;return Ok(());}
                event=speech.events.recv()=>{display_event(event.context("Recognition worker closed")??,&output)?;}
            }
        } else {
            return receiving.await;
        }
    }
}

#[cfg(test)]
mod gui_tests {
    use super::*;
    #[test]
    fn ble_state_emits_edges_before_audio_and_preserves_worker_updates() {
        let temp = tempfile::tempdir().unwrap();
        let log = temp.path().join("events.log");
        let mut output = Output::new(false, Some(&log)).unwrap();
        output.events = true;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut received = Received {
            next: None,
            initialized: false,
            initial_count: None,
            outbound: Some(tx),
            output,
            collecting: None,
            inactive_since: None,
            raw_press_since: None,
        };
        let mut state = crate::bluetooth::advertisement(&[0, 0, 0, 0, 0, 0]).unwrap();
        received.state(&state).unwrap();
        received.state(&state).unwrap();
        state.in_collection_state = true;
        received.state(&state).unwrap();
        received.state(&state).unwrap();
        state.in_collection_state = false;
        received.inactive_since = Some(Instant::now() - Duration::from_millis(300));
        received.state(&state).unwrap();
        let diagnostics = std::fs::read_to_string(log).unwrap();
        assert_eq!(diagnostics.matches("Recording state edge").count(), 3);
        assert!(
            !diagnostics.lines().any(|line| line.starts_with('{')),
            "Raw BLE edges must not bypass logical input state events"
        );
        let mut states = vec![];
        while let Ok(value) = rx.try_recv() {
            if value["type"] == "state" {
                states.push(value["collecting"].as_bool().unwrap());
            }
        }
        assert_eq!(states, [false, false, true, true, false]);
    }
}
