use crate::{
    Listen,
    bluetooth::{Bluetooth, RingState, advertised_state},
    collection::metadata,
    output::Output,
    recognition::{Client, Options, display_event},
};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use pebble_index::reception::scheduler::{Decision, Request, Scheduler};
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
    range_end: Option<u16>,
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
    fn millis(&self) -> u64 {
        self.observation_origin.elapsed().as_millis() as u64
    }
    fn range(&mut self, start: u16, end: u16) -> Result<()> {
        self.range_end = Some(end);
        self.send(json!({"type":"range","start":start,"end":end}))
    }
    fn state(&mut self, state: &RingState) -> Result<()> {
        self.button_timing
            .observe(state.in_collection_state, &self.output);
        self.send(
            json!({"type":"button_state","pressed":state.in_collection_state,"unread":self.next,
                "range_pending":self.range_end.is_some_and(|end| state.collection_count != end as u8)}),
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
    let mut state_started = received.millis();
    let mut state = received.read(ble.state(args.timeout)).await?;
    received.state(&state)?;
    let range_started = received.millis();
    let (start, end) = received.read(ble.range(args.timeout)).await?;
    if !received.initialized {
        received.next = Some(end);
        let initial_active = state.in_collection_state;
        let initial_count = state.collection_count;
        state_started = received.millis();
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
        output.line("BLE接続・初期化完了。録音中ならそのまま話し続けてください。");
        output.debug(format!("Startup flush: skipped existing collections {start}..{}; active recording preserved={}",received.next.unwrap(),received.next!=Some(end)));
    }
    received.range(start, end)?;
    // This initial R may precede the second startup S. Preserve its count hint
    // without guessing a full counter value from eight bits.
    if state.collection_count != end as u8 {
        received.send(json!({"type":"range_pending"}))?;
    }
    let mut scheduler = Scheduler::new(
        (interval * 1_000.).ceil().max(1.) as u64,
        state_started,
        range_started,
        start,
        end,
        received.next.unwrap_or(start),
        state.collection_count,
    )?;
    received.next = Some(scheduler.cursor());
    let mut caught_up = false;
    loop {
        if scheduler.caught_up() {
            if fetch && !state.in_collection_state {
                return Ok(state);
            }
            if !caught_up {
                received.send(json!({"type":"caught_up"}))?;
                caught_up = true;
            }
        } else {
            caught_up = false;
        }
        let decision = scheduler.next(received.millis())?;
        let (request, lateness) = match decision {
            Decision::WaitUntil(deadline) => {
                let delay = deadline.saturating_sub(received.millis());
                received
                    .read(async {
                        tokio::time::sleep(Duration::from_millis(delay)).await;
                        Ok(())
                    })
                    .await?;
                continue;
            }
            Decision::Read {
                request,
                deadline_lateness_ms,
            } => (request, deadline_lateness_ms),
        };
        output.debug(format!(
            "BLE schedule request={request:?} deadline_lateness_ms={lateness} next_collection={:?}",
            received.next
        ));
        match request {
            Request::State => {
                state = received.read(ble.state(args.timeout)).await?;
                scheduler.state(state.collection_count, received.millis())?;
                received.state(&state)?;
            }
            Request::Range => {
                let (start, end) = received.read(ble.range(args.timeout)).await?;
                scheduler.range(start, end)?;
                received.range(start, end)?;
                received.next = Some(scheduler.cursor());
                output.debug(format!(
                    "Collection range: {start}..{end}; downloading from {}",
                    scheduler.cursor()
                ));
                cached.retain(|index, _| index.wrapping_sub(start) < end.wrapping_sub(start));
            }
            Request::Collection(index) => {
                let began = Instant::now();
                let raw = if let Some(raw) = cached.remove(&index) {
                    raw
                } else {
                    received.read(ble.collection(index, args.timeout)).await?
                };
                // Deliver every C immediately, independent of S deadlines and
                // recognition work. Do not batch small records or delay meters.
                received
                    .send(json!({"type":"collection","index":index,"raw":STANDARD.encode(&raw)}))?;
                scheduler.collection(index)?;
                received.next = Some(scheduler.cursor());
                output.debug(format!(
                    "collection={index} transfer took {:.3}s",
                    began.elapsed().as_secs_f64()
                ));
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
        range_end: None,
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
            range_end: None,
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
            range_end: None,
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
