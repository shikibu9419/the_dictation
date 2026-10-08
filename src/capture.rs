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
    observation_origin: Instant,
    observation_sequence: u64,
    button_timing: crate::button_timing::ButtonTiming,
}
impl Received {
    fn send(&mut self, mut event: Value) -> Result<()> {
        self.observation_sequence += 1;
        event["received_ms"] = json!(self.observation_origin.elapsed().as_millis() as u64);
        event["received_seq"] = json!(self.observation_sequence);
        if let Some(tx) = &self.outbound {
            tx.send(event).context("Recognition worker closed")?;
        }
        Ok(())
    }
    async fn read<T>(
        &mut self,
        operation: impl std::future::Future<Output = Result<T>>,
    ) -> Result<T> {
        if self.outbound.is_none() {
            return operation.await;
        }
        tokio::pin!(operation);
        let mut clock = tokio::time::interval(Duration::from_millis(5));
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                result = &mut operation => return result,
                _ = clock.tick() => self.send(json!({"type":"clock"}))?,
            }
        }
    }
    fn state(&mut self, state: &RingState) -> Result<()> {
        self.button_timing
            .observe(state.in_collection_state, &self.output);
        self.send(
            json!({"type":"button_state","pressed":state.in_collection_state,"unread":self.next}),
        )?;
        if self.collecting != Some(state.in_collection_state) {
            self.output.debug(format!(
                "Collection state {:?} -> {}; next_collection={:?}; state={state:?}",
                self.collecting, state.in_collection_state, self.next
            ));
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
    interval: f64,
) -> Result<RingState> {
    let mut cached = HashMap::new();
    let mut state = received.read(ble.state(args.timeout)).await?;
    received.state(&state)?;
    let mut polled = Instant::now();
    let (mut start, mut end) = received.read(ble.range(args.timeout)).await?;
    let mut range_checked = Instant::now();
    loop {
        if !received.initialized {
            received.next = Some(end);
            let initial_active = state.in_collection_state;
            let initial_count = state.collection_count;
            state = received.read(ble.state(args.timeout)).await?;
            received.state(&state)?;
            let observed_active = initial_active || received.initial_count.is_some();
            if (state.in_collection_state || observed_active) && start != end {
                let latest = end.wrapping_sub(1);
                let raw = received.read(ble.collection(latest, args.timeout)).await?;
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
            received.send(json!({"type":"range","start":start,"end":end}))?;
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
                received.read(ble.collection(index, args.timeout)).await?
            };
            output.debug(format!(
                "collection={index} transfer took {:.3}s",
                began.elapsed().as_secs_f64()
            ));
            received
                .send(json!({"type":"collection","index":index,"raw":STANDARD.encode(&raw)}))?;
            received.next = Some(index.wrapping_add(1));
            // Small tap records are drained first. A state round trip between
            // them would become an artificial inter-tap delay. For audio,
            // continue checking release between complete protocol transactions.
            if raw.len() > 512 && polled.elapsed() >= Duration::from_millis(100) {
                state = received.read(ble.state(args.timeout)).await?;
                received.state(&state)?;
                polled = Instant::now();
            }
        }
        received.next = Some(end);
        received.send(json!({"type":"range","start":start,"end":end}))?;
        // Immediately drain anything that appeared during the previous read.
        // Do not insert a state read or fixed sleep between queued tap records.
        if cursor != end {
            let (new_start, new_end) = received.read(ble.range(args.timeout)).await?;
            range_checked = Instant::now();
            start = new_start;
            if new_end != end {
                output.debug(format!(
                    "Range advanced during transfer: {end} -> {new_end}; keep downloading"
                ));
                end = new_end;
                received.send(json!({"type":"range","start":start,"end":end}))?;
                continue;
            }
        }
        // Idle waits consist of state reads. The state includes the low byte of
        // collection count, so an unchanged count doesn't need a range read.
        // Refresh the full range periodically to catch wraparound/reset.
        loop {
            let cycle = Instant::now();
            state = received.read(ble.state(args.timeout)).await?;
            received.state(&state)?;
            polled = Instant::now();
            if state.collection_count != end as u8
                || range_checked.elapsed() >= Duration::from_secs(1)
                || fetch
            {
                let (new_start, new_end) = received.read(ble.range(args.timeout)).await?;
                range_checked = Instant::now();
                start = new_start;
                if new_end != end {
                    end = new_end;
                    received.send(json!({"type":"range","start":start,"end":end}))?;
                    break;
                }
            }
            if fetch && !state.in_collection_state {
                return Ok(state);
            }
            received.send(json!({"type":"caught_up"}))?;
            // --interval is start-to-start cadence, not extra latency appended
            // to every BLE response. Normally the response already exceeds it.
            let remaining = Duration::from_secs_f64(interval).saturating_sub(cycle.elapsed());
            if !remaining.is_zero() {
                received
                    .read(async {
                        tokio::time::sleep(remaining).await;
                        Ok(())
                    })
                    .await?;
            }
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
    let interval = args.interval.unwrap_or(
        crate::settings::Settings::load()?
            .reception
            .state_poll_interval_ms as f64
            / 1000.,
    );
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
        observation_origin: Instant::now(),
        observation_sequence: 0,
        button_timing: Default::default(),
    };
    output.event(&json!({"type":"ready"}));
    let mut paired = false;
    // Try the saved peripheral immediately; do not wait for a fresh advertisement.
    // A failed direct attempt falls back to scanning.
    let mut reconnect_remaining = 1u8;
    loop {
        let direct = reconnect_remaining > 0;
        let device = if direct {
            reconnect_remaining -= 1;
            output.debug(format!("Direct reconnect to known Index; attempts_left={reconnect_remaining}; resume_collection={:?}", received.next));
            // Do not reuse cached manufacturer data as a fresh button edge.
            json!({"address":address})
        } else {
            output.debug("Scanning for Index");
            received
                .read(ble.request(json!({"type":"find","address":address}), args.timeout))
                .await?
        };
        if device.is_null() {
            if fetch {
                bail!("Index not advertising; press ring button");
            }
            continue;
        }
        output.debug(format!("Discovered: {device}"));
        if let Some(state) = advertised_state(&device)
            && !received.initialized
            && state.in_collection_state
            && received.initial_count.is_none()
        {
            received.initial_count = Some(state.collection_count);
        }
        let result = async {
            let started = Instant::now();
            output.debug(format!("Connecting: {address}"));
            let pair = !paired || args.pair;
            received
                .read(ble.connect(
                    &address,
                    pair,
                    if direct {
                        args.timeout.min(5.0)
                    } else if pair {
                        args.timeout
                    } else {
                        args.timeout.min(8.0)
                    },
                ))
                .await?;
            paired = true;
            output.debug(format!(
                "Connected: {address}; connection took {:.3}s",
                started.elapsed().as_secs_f64()
            ));
            received.read(ble.subscribe(args.timeout)).await?;
            received.send(json!({"type":"connected"}))?;
            reconnect_remaining = 3;
            download(&mut ble, &mut received, &args, &output, fetch, interval).await
        }
        .await;
        if result.is_err() {
            received.button_timing.disconnect(&output);
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
        received
            .read(async {
                tokio::time::sleep(Duration::from_secs_f64(interval)).await;
                Ok(())
            })
            .await?;
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
    #[tokio::test]
    async fn ready_read_wins_before_clock_and_pending_read_emits_ordered_ticks() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut received = Received {
            next: None,
            initialized: true,
            initial_count: None,
            outbound: Some(tx),
            output: Output::new(false, None).unwrap(),
            collecting: None,
            observation_origin: Instant::now(),
            observation_sequence: 0,
            button_timing: Default::default(),
        };
        received.read(std::future::ready(Ok(()))).await.unwrap();
        assert!(rx.try_recv().is_err()); // Input-ready branch precedes a due timer.
        received
            .read(async {
                tokio::time::sleep(Duration::from_millis(12)).await;
                Ok(())
            })
            .await
            .unwrap();
        received
            .send(json!({"type":"collection","index":1}))
            .unwrap();
        let mut previous = 0;
        let mut clock = false;
        let mut last_time = 0;
        while let Ok(event) = rx.try_recv() {
            assert_eq!(event["received_seq"].as_u64().unwrap(), previous + 1);
            previous += 1;
            let time = event["received_ms"].as_u64().unwrap();
            assert!(time >= last_time);
            last_time = time;
            clock |= event["type"] == "clock";
        }
        assert!(clock);
    }
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
            observation_origin: Instant::now(),
            observation_sequence: 0,
            button_timing: Default::default(),
        };
        let mut state = crate::bluetooth::advertisement(&[0, 0, 0, 0, 0, 0]).unwrap();
        received.state(&state).unwrap();
        received.state(&state).unwrap();
        state.in_collection_state = true;
        received.state(&state).unwrap();
        received.state(&state).unwrap();
        state.in_collection_state = false;
        received.state(&state).unwrap();
        let diagnostics = std::fs::read_to_string(log).unwrap();
        assert_eq!(diagnostics.matches("Collection state").count(), 3);
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
