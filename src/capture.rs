use crate::{
    Listen,
    bluetooth::{Bluetooth, RingState, advertised_state},
    collection::metadata,
    output::Output,
    recognition::{Client, Options, display_event},
};
use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use pebble_index::ipc;
use pebble_index::reception::connection::{ConnectionPolicy, WatchReason};
use pebble_index::reception::scheduler::{Decision, Request, Scheduler};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

struct Received {
    next: Option<u16>,
    initialized: bool,
    initial_count: Option<u8>,
    outbound: Option<ipc::Sender<Value>>,
    output: Output,
    collecting: Option<bool>,
    range_end: Option<u16>,
    last_state: Option<RingState>,
    last_advertisement: Option<String>,
    connection: ConnectionPolicy,
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
        if self.range_end.is_some_and(|old| old != end) {
            self.connection.activity(self.millis());
        }
        self.range_end = Some(end);
        self.send(json!({"type":"range","start":start,"end":end}))
    }
    fn state(&mut self, state: &RingState) -> Result<()> {
        if state.in_collection_state
            || self
                .last_state
                .as_ref()
                .is_some_and(|old| old.collection_count != state.collection_count)
        {
            self.connection.activity(self.millis());
        }
        self.last_state = Some(state.clone());
        self.last_advertisement = Some(state.advertisement_signature());
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
    fn progress(&mut self) {
        if let Some(duration) = self.connection.progress(self.millis()) {
            self.output.debug(format!(
                "BLE communication recovered recovery_ms={duration} next_collection={:?}",
                self.next
            ));
        }
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
enum DownloadEnd {
    Fetched,
    Idle,
}

async fn download(
    ble: &mut Bluetooth,
    received: &mut Received,
    args: &Listen,
    output: &Output,
    fetch: bool,
    interval: f64,
) -> Result<DownloadEnd> {
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
            received.progress();
            if fetch && !state.in_collection_state {
                return Ok(DownloadEnd::Fetched);
            }
            if !fetch
                && received
                    .connection
                    .idle(received.millis(), state.in_collection_state, true)
            {
                output.debug(
                    "BLE advertisement wait reason=idle idle_ms=3600000; retained receive cursor",
                );
                return Ok(DownloadEnd::Idle);
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
                received.connection.activity(received.millis());
                received.progress();
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
    outbound: Option<ipc::Sender<Value>>,
    output: Output,
) -> Result<()> {
    let ble = Bluetooth::prepare(output.clone()).await?;
    receive_with_bridge(args, address, fetch, outbound, output, ble).await
}
async fn receive_with_bridge(
    args: Listen,
    address: String,
    fetch: bool,
    outbound: Option<ipc::Sender<Value>>,
    output: Output,
    mut ble: Bluetooth,
) -> Result<()> {
    let interval = match args.interval {
        Some(interval) => interval,
        None => {
            crate::settings::Settings::load()?
                .reception
                .state_poll_interval_ms as f64
                / 1000.
        }
    };
    output.line(if outbound.is_some() {
        "音声認識の準備完了・Bluetooth待機中。リングを長押しして話してください。"
    } else {
        "Bluetooth接続待機を開始します。リングを長押しして話してください。"
    });
    let mut received = Received {
        next: None,
        initialized: fetch,
        initial_count: None,
        outbound,
        output: output.clone(),
        collecting: None,
        range_end: None,
        last_state: None,
        last_advertisement: None,
        connection: ConnectionPolicy::default(),
        observation_origin: Instant::now(),
        observation_sequence: 0,
        button_timing: Default::default(),
    };
    output.event(&json!({"type":"ready"}));
    let mut paired = false;
    let mut last_mode = None;
    loop {
        let mode = received.connection.mode(received.millis());
        if mode != last_mode {
            output.debug(format!(
                "BLE connection policy mode={mode:?} next_collection={:?}",
                received.next
            ));
            last_mode = mode;
        }
        let mut stage = "helper_start";
        let result: Result<DownloadEnd> = async {
            received.read(ble.ensure_ready()).await?;
            if let Some(reason) = mode {
                stage = "advertisement_wait";
                let request = json!({"type":"find","address":address,"watch":true,
                    "baseline":received.last_advertisement,
                    "retry_interval_ms": if reason == WatchReason::Failure {30_000} else {0}});
                let device = received.read(ble.request(request, args.timeout)).await?;
                if device.is_null() {
                    return Ok(DownloadEnd::Idle);
                }
                output.debug(format!("BLE reconnect hint: {device}"));
                received.connection.resume_hint(received.millis());
                if let Some(state) = advertised_state(&device) {
                    received.last_advertisement = Some(state.advertisement_signature());
                    if !received.initialized
                        && state.in_collection_state
                        && received.initial_count.is_none()
                    {
                        received.initial_count = Some(state.collection_count);
                    }
                }
            }
            stage = "connect";
            let started = Instant::now();
            output.debug(format!(
                "Connecting: {address}; resume_collection={:?}",
                received.next
            ));
            let pair = !paired || args.pair;
            received
                .read(ble.connect(&address, pair, args.timeout.min(5.0)))
                .await?;
            paired = true;
            output.debug(format!(
                "Connected: {address}; connection took {:.3}s",
                started.elapsed().as_secs_f64()
            ));
            stage = "subscribe";
            received.read(ble.subscribe(args.timeout)).await?;
            received.send(json!({"type":"connected"}))?;
            stage = "receive";
            download(&mut ble, &mut received, &args, &output, fetch, interval).await
        }
        .await;
        // A watch timeout keeps native discovery running, without a disconnect
        // command or artificial scan gap between successive waits.
        if stage == "advertisement_wait" && result.is_ok() {
            continue;
        }
        if result.is_err() {
            received.connection.failed(received.millis());
            received.button_timing.disconnect(&output);
        }
        received.send(json!({"type":"connection_lost"}))?;
        ble.unsubscribe().await;
        ble.disconnect().await;
        match result {
            Ok(DownloadEnd::Fetched) => {
                ble.close().await;
                return Ok(());
            }
            Ok(DownloadEnd::Idle) => continue,
            Err(error) => {
                let text = format!("{error:#}");
                output.debug(format!(
                    "BLE failure stage={stage} error={error:#}; resume_collection={:?}",
                    received.next
                ));
                let helper_fault = error
                    .downcast_ref::<crate::bluetooth::TransportFault>()
                    .is_some();
                if fetch
                    || (!helper_fault
                        && (crate::bluetooth::encryption_rejected(&text)
                            || crate::bluetooth::pairing_removed(&text)
                            || text.contains("Invalid")
                            || text.contains("length mismatch")
                            || text.contains("exceeds")
                            || text.contains("too large")
                            || text.contains("Truncated")))
                {
                    return Err(error);
                }
            }
        }
        let delay_ms = received.connection.retry_delay_ms();
        output.debug(format!(
            "BLE retry delay_ms={delay_ms} mode={:?}",
            received.connection.mode(received.millis())
        ));
        received
            .read(async {
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
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
                event=speech.events.recv()=>{display_event(event?.context("Recognition worker closed")??,&output)?;}
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
    async fn bridge_exit_during_audio_reconnects_at_the_retained_cursor_without_startup_flush() {
        use std::os::unix::fs::PermissionsExt;
        fn read_packet(address: u32, size: u32) -> String {
            let mut bytes = vec![3];
            bytes.extend(address.to_le_bytes());
            bytes.extend(0u32.to_le_bytes());
            bytes.extend(size.to_le_bytes());
            STANDARD.encode(bytes)
        }
        fn reply_header(size: usize) -> String {
            STANDARD.encode(
                [
                    0u32.to_le_bytes(),
                    0u32.to_le_bytes(),
                    (size as u32).to_le_bytes(),
                ]
                .concat(),
            )
        }
        fn raw(final_part: bool, samples: &[i16]) -> Vec<u8> {
            let mut body = vec![80];
            body.extend((4 + samples.len() as u32 * 2).to_le_bytes());
            body.extend(9997u32.to_le_bytes());
            for sample in samples {
                body.extend(sample.to_le_bytes());
            }
            body.extend([82, 6, 0]);
            body.extend(1u32.to_le_bytes());
            body.extend([1, final_part as u8]);
            let mut raw = (body.len() as u32 + 4).to_le_bytes().to_vec();
            raw.extend(body);
            raw
        }
        let first = raw(false, &[1, 2]);
        let last = raw(true, &[3, 4, 5]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bridge");
        let mut script = r#"#!/bin/sh
first=0
if mkdir "${0}.first" 2>/dev/null; then first=1; fi
exec /usr/bin/awk -v first="$first" -v logfile="${0}.log" '
function data(header, payload) {
  printf "{\"type\":\"notification\",\"uuid\":\"__CONTROL__\",\"data\":\"%s\"}\n", header
  printf "{\"type\":\"notification\",\"uuid\":\"__DATA__\",\"data\":\"%s\"}\n", payload
  fflush()
}
BEGIN { print "{\"type\":\"ready\"}"; fflush() }
{ print first "\t" $0 >> logfile; fflush(logfile)
  id=$0; sub(/.*"id":/, "", id); sub(/[^0-9].*/, "", id)
  value=($0 ~ /"type":"inspect"/) ? "[]" : "null"
  printf "{\"type\":\"reply\",\"id\":%s,\"value\":%s}\n", id, value; fflush()
  if (index($0, "__READ_S__")) data("__HEADER_S__", first ? "__STATE_ACTIVE__" : "__STATE_DONE__")
  if (index($0, "__READ_R__")) data("__HEADER_R__", first ? "__RANGE_INITIAL__" : "__RANGE_DONE__")
  if (index($0, "__READ_C1__")) { data("__HEADER_C1__", "__C1__"); exit 9 }
  if (index($0, "__READ_C2__")) data("__HEADER_C2__", "__C2__")
}
'
"#
        .to_owned();
        let read_first = read_packet(0x40020001, 0);
        let read_last = read_packet(0x40020002, 0);
        for (key, value) in [
            ("__CONTROL__", crate::bluetooth::CONTROL.into()),
            ("__DATA__", crate::bluetooth::DATA.into()),
            ("__READ_S__", read_packet(0x4003000e, 10)),
            ("__READ_R__", read_packet(0x40030005, 4)),
            ("__READ_C1__", read_first.clone()),
            ("__READ_C2__", read_last.clone()),
            ("__HEADER_S__", reply_header(10)),
            ("__HEADER_R__", reply_header(4)),
            ("__HEADER_C1__", reply_header(first.len())),
            ("__HEADER_C2__", reply_header(last.len())),
            (
                "__STATE_ACTIVE__",
                STANDARD.encode([0, 0, 255, 255, 1, 0, 0, 0, 2, 32]),
            ),
            (
                "__STATE_DONE__",
                STANDARD.encode([0, 0, 255, 255, 1, 0, 0, 0, 3, 0]),
            ),
            ("__RANGE_INITIAL__", STANDARD.encode([1, 0, 2, 0])),
            ("__RANGE_DONE__", STANDARD.encode([1, 0, 3, 0])),
            ("__C1__", STANDARD.encode(&first)),
            ("__C2__", STANDARD.encode(&last)),
        ] {
            script = script.replace(key, &value);
        }
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let output = Output::new(false, None).unwrap();
        let ble = Bluetooth::with_program(path.clone(), output.clone());
        let args = Listen {
            address: Some("synthetic".into()),
            timeout: 1.,
            interval: Some(0.05),
            pair: false,
            transcribe: false,
            no_transcribe: false,
            language: "ja_JP".into(),
        };
        let (tx, mut rx) = ipc::process_channel("test capture");
        let receiving = receive_with_bridge(args, "synthetic".into(), false, Some(tx), output, ble);
        tokio::pin!(receiving);
        let events = tokio::select! {
            result = &mut receiving => panic!("receiver exited: {result:?}"),
            events = tokio::time::timeout(Duration::from_secs(8), async {
                let mut events = vec![];
                loop {
                    let event = rx.recv().await.unwrap().unwrap();
                    let done = event["type"] == "collection" && event["index"] == 2;
                    events.push(event);
                    if done { return events; }
                }
            }) => events.unwrap(),
        };
        assert_eq!(events.iter().filter(|v| v["type"] == "boundary").count(), 1);
        assert!(events.iter().any(|v| v["type"] == "connection_lost"));
        let chunks: Vec<_> = events
            .iter()
            .filter(|v| v["type"] == "collection")
            .collect();
        assert_eq!(chunks.len(), 2);
        assert_eq!(
            STANDARD.decode(chunks[0]["raw"].as_str().unwrap()).unwrap(),
            first
        );
        assert_eq!(
            STANDARD.decode(chunks[1]["raw"].as_str().unwrap()).unwrap(),
            last
        );
        let log = std::fs::read_to_string(path.with_extension("log")).unwrap();
        assert_eq!(
            log.lines()
                .filter(|line| line.contains(&read_first))
                .count(),
            1
        );
        assert_eq!(
            log.lines().filter(|line| line.contains(&read_last)).count(),
            1
        );
    }
    #[tokio::test]
    async fn ready_read_wins_before_clock_and_pending_read_emits_ordered_ticks() {
        let (tx, mut rx) = ipc::process_channel("capture events");
        let mut received = Received {
            next: None,
            initialized: true,
            initial_count: None,
            outbound: Some(tx),
            output: Output::new(false, None).unwrap(),
            collecting: None,
            range_end: None,
            last_state: None,
            last_advertisement: None,
            connection: ConnectionPolicy::default(),
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
        let (tx, mut rx) = ipc::process_channel("capture events");
        let mut received = Received {
            next: None,
            initialized: false,
            initial_count: None,
            outbound: Some(tx),
            output,
            collecting: None,
            range_end: None,
            last_state: None,
            last_advertisement: None,
            connection: ConnectionPolicy::default(),
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
