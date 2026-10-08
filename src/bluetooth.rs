use crate::{
    collection::u32le,
    helper::{Helper, executable},
    output::Output,
};
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};
use tokio::{process::Command, time::timeout};
pub const CONTROL: &str = "c0ef558a-2058-fabf-a140-8d5acde50b39";
pub const DATA: &str = "daad3d52-237c-90a7-b54b-8854a134d801";
pub const SYSTEM_INPUT: &str = "1d1f4039-23f5-33b2-c24e-704351f20585";
pub const MAX_SIZE: usize = 655360;
pub fn pairing_removed(message: &str) -> bool {
    message
        .to_lowercase()
        .contains("peer removed pairing information")
}
pub fn encryption_rejected(message: &str) -> bool {
    let message = message.to_lowercase();
    [
        "insufficient encryption",
        "encryption is insufficient",
        "cbatterrordomain code=15",
    ]
    .iter()
    .any(|text| message.contains(text))
}

#[derive(Default)]
struct Response {
    control: Vec<u8>,
    data: Vec<u8>,
}
impl Response {
    fn add(&mut self, uuid: &str, bytes: &[u8]) -> Result<()> {
        if uuid == CONTROL {
            self.control.extend_from_slice(bytes);
        } else if uuid == DATA {
            self.data.extend_from_slice(bytes);
        }
        ensure!(
            self.control.len() <= 12 && self.data.len() <= MAX_SIZE,
            "Telesto notification exceeds bounds"
        );
        Ok(())
    }
    fn complete(&self) -> Result<Option<Vec<u8>>> {
        if self.control.len() != 12 {
            return Ok(None);
        }
        let error = u32le(&self.control);
        let info = u32le(&self.control[4..]);
        let size = u32le(&self.control[8..]) as usize;
        ensure!(error == 0, "Telesto read failed: code={error}, info={info}");
        ensure!(size <= MAX_SIZE, "Telesto response too large");
        Ok((self.data.len() >= size).then(|| self.data[..size].to_vec()))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RingState {
    pub fingerprint: u32,
    pub collection_count: u8,
    pub needs_servicing: bool,
    pub in_collection_state: bool,
    pub is_moving: bool,
    pub has_debug_info: bool,
    pub is_dark: bool,
}
impl RingState {
    pub fn advertisement_signature(&self) -> String {
        format!(
            "{:08x}:{}:{}:{}",
            self.fingerprint,
            self.collection_count,
            u8::from(self.in_collection_state),
            u8::from(self.is_moving)
        )
    }
}
pub fn advertisement(data: &[u8]) -> Result<RingState> {
    let data = if data.len() == 8 { &data[2..] } else { data };
    ensure!(
        data.len() == 6,
        "Index manufacturer payload must be six or eight bytes"
    );
    let f = data[5];
    Ok(RingState {
        fingerprint: u32le(data),
        collection_count: data[4],
        needs_servicing: f & 64 != 0,
        in_collection_state: f & 32 != 0,
        is_moving: f & 128 != 0,
        has_debug_info: f & 16 != 0,
        is_dark: f & 8 != 0,
    })
}
pub fn advertised_state(device: &Value) -> Option<RingState> {
    device["manufacturer_data"]
        .as_object()?
        .values()
        .find_map(|v| {
            let hex = v.as_str()?;
            let bytes = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
                .collect::<Option<Vec<_>>>()?;
            advertisement(&bytes).ok()
        })
}
pub struct Bluetooth {
    helper: Helper,
    next: u64,
    pending: VecDeque<Value>,
    pub connected: bool,
    auxiliary_subscribed: bool,
    output: Output,
    metrics: ReadMetrics,
    connected_since: Option<Instant>,
}

struct ReadMetrics {
    origin: Instant,
    reported: Instant,
    attempts: [u64; 4], // S, R, C, other. Include failed attempts.
    errors: [u64; 4],
    elapsed_ms: [f64; 4],
    bytes: u64,
}
impl Default for ReadMetrics {
    fn default() -> Self {
        Self {
            origin: Instant::now(),
            reported: Instant::now(),
            attempts: [0; 4],
            errors: [0; 4],
            elapsed_ms: [0.; 4],
            bytes: 0,
        }
    }
}
impl ReadMetrics {
    fn record(&mut self, address: u32, elapsed: Duration, result: &Result<Vec<u8>>) {
        let kind = match address {
            0x4003000e => 0,
            0x40030005 => 1,
            n if n & 0xffff0000 == 0x40020000 => 2,
            _ => 3,
        };
        self.attempts[kind] += 1;
        self.errors[kind] += u64::from(result.is_err());
        self.elapsed_ms[kind] += elapsed.as_secs_f64() * 1_000.;
        self.bytes += result.as_ref().map_or(0, |bytes| bytes.len() as u64);
    }
    fn report(&mut self, output: &Output, force: bool) {
        if force || self.reported.elapsed() >= Duration::from_secs(60) {
            output.debug(format!(
                "BLE read totals elapsed_s={:.1} requests_S_R_C_other={:?} errors_S_R_C_other={:?} duration_ms_S_R_C_other={:?} response_bytes={}",
                self.origin.elapsed().as_secs_f64(), self.attempts, self.errors, self.elapsed_ms, self.bytes,
            ));
            self.reported = Instant::now();
        }
    }
}
impl Bluetooth {
    pub async fn start(output: Output) -> Result<Self> {
        let exe = executable(
            "Bluetooth",
            include_str!("../native/Bluetooth.swift"),
            &output,
        )
        .await?;
        let mut helper =
            Helper::spawn(Command::new(exe), output.clone(), "bluetooth".into()).await?;
        let event = timeout(Duration::from_secs(30), helper.event()).await??;
        ensure!(
            event["type"] == "ready",
            "Bluetooth initialization failed: {event}"
        );
        Ok(Self {
            helper,
            next: 0,
            pending: VecDeque::new(),
            connected: false,
            auxiliary_subscribed: false,
            output,
            metrics: ReadMetrics::default(),
            connected_since: None,
        })
    }
    fn observe(&mut self, event: &Value) {
        if event["type"] == "notification" && event["uuid"] == SYSTEM_INPUT {
            // Protocol semantics are not known. Preserve evidence, never turn
            // arbitrary bytes into a button edge or a Telesto response.
            self.output.debug(format!("BLE auxiliary push: {event}"));
        }
        if event["type"] == "disconnected" {
            self.output.debug(format!("BLE disconnect detail: {event}"));
            self.connected = false;
        }
    }
    pub async fn request(&mut self, mut message: Value, seconds: f64) -> Result<Value> {
        self.next += 1;
        let id = self.next;
        message["id"] = json!(id);
        message["timeout"] = json!(seconds);
        let operation = message["type"].as_str().unwrap_or("").to_owned();
        self.output.debug(format!(
            "Bluetooth command id={id} type={operation} timeout={seconds:.3}s"
        ));
        timeout(Duration::from_secs_f64(seconds + 1.0), async {
            self.helper.send(&message).await?;
            loop {
                let event = self.helper.event().await?;
                self.observe(&event);
                if event["type"] == "reply" && event["id"].as_u64() == Some(id) {
                    self.output
                        .debug(format!("Bluetooth reply id={id} type={operation}"));
                    if let Some(error) = event.get("error") {
                        let message = error.as_str().unwrap_or("Bluetooth error");
                        if pairing_removed(message) {
                            bail!("{operation}: {message}. Mac側の古いペアリング情報を解除してください。システム設定 → Bluetooth → Pebble Index の登録を解除し、pebble-index を再起動してください。UUIDの再登録は不要です。");
                        }
                        bail!(
                            "{operation}: {}",
                            message
                        );
                    }
                    return Ok(event["value"].clone());
                }
                if event["type"] == "error" {
                    bail!("Bluetooth: {}", event["text"]);
                }
                if event["type"] == "notification" && (event["uuid"] == CONTROL || event["uuid"] == DATA) {
                    self.pending.push_back(event);
                }
            }
        })
        .await
        .with_context(|| {
            format!("Bluetooth helper did not acknowledge command id={id} type={operation}")
        })?
    }
    pub async fn connect(&mut self, address: &str, pair: bool, seconds: f64) -> Result<()> {
        let attempts = if pair { 3 } else { 1 };
        for attempt in 0..attempts {
            let result = async {
                self.request(json!({"type":"connect","address":address}), seconds)
                    .await?;
                self.connected = true;
                self.connected_since = Some(Instant::now());
                if pair {
                    self.request(
                        json!({"type":"write","uuid":DATA,"data":"AA==","response":true}),
                        seconds,
                    )
                    .await?;
                }
                Ok::<_, anyhow::Error>(())
            }
            .await;
            match result {
                Ok(()) => return Ok(()),
                Err(error) if encryption_rejected(&format!("{error:#}")) => {
                    self.disconnect().await;
                    if attempt + 1 == attempts {
                        return Err(error.context("Index encryption was rejected. Remove the existing phone pairing before pairing this Mac"));
                    }
                }
                Err(error) => return Err(error),
            }
        }
        unreachable!()
    }
    pub async fn disconnect(&mut self) {
        if let Err(error) = self.request(json!({"type":"disconnect"}), 3.0).await {
            self.output
                .debug(format!("Bluetooth disconnect: {error:#}"));
        }
        self.connected = false;
        self.pending.clear();
        if let Some(since) = self.connected_since.take() {
            self.output.debug(format!(
                "BLE connection duration_s={:.3}",
                since.elapsed().as_secs_f64()
            ));
        }
        self.metrics.report(&self.output, true);
    }
    pub async fn subscribe(&mut self, seconds: f64) -> Result<()> {
        for uuid in [CONTROL, DATA] {
            self.request(
                json!({"type":"notify","uuid":uuid,"enabled":true}),
                seconds.min(5.0),
            )
            .await?;
        }
        let services = self
            .request(json!({"type":"inspect"}), seconds.min(5.0))
            .await?;
        self.output.debug(format!("BLE services: {services}"));
        let auxiliary_notify = services.as_array().into_iter().flatten().any(|service| {
            service["characteristics"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|c| {
                    c["uuid"] == SYSTEM_INPUT
                        && c["properties"]
                            .as_array()
                            .is_some_and(|p| p.iter().any(|v| v == "notify" || v == "indicate"))
                })
        });
        self.auxiliary_subscribed = false;
        if auxiliary_notify {
            match self
                .request(
                    json!({"type":"notify","uuid":SYSTEM_INPUT,"enabled":true}),
                    seconds.min(5.0),
                )
                .await
            {
                Ok(_) => {
                    self.auxiliary_subscribed = true;
                    self.output.debug("Auxiliary notifications subscribed for observation; button push format remains unknown");
                }
                Err(error) => self.output.debug(format!(
                    "Auxiliary notification subscription unavailable: {error:#}"
                )),
            }
        }
        Ok(())
    }
    pub async fn unsubscribe(&mut self) {
        for uuid in [SYSTEM_INPUT, DATA, CONTROL] {
            if uuid == SYSTEM_INPUT && !self.auxiliary_subscribed {
                continue;
            }
            if !self.connected {
                break;
            }
            if let Err(error) = self
                .request(json!({"type":"notify","uuid":uuid,"enabled":false}), 1.0)
                .await
            {
                self.output
                    .debug(format!("Stop notification failed: {error:#}"));
            }
        }
        self.pending.clear();
    }
    pub async fn read(&mut self, address: u32, length: u32, seconds: f64) -> Result<Vec<u8>> {
        self.pending.clear();
        let mut response = Response::default();
        let started = Instant::now();
        let mut first_notification = None;
        let result=timeout(Duration::from_secs_f64(seconds.min(5.0)),async {
            let mut packet=vec![3]; packet.extend(address.to_le_bytes()); packet.extend(0u32.to_le_bytes()); packet.extend(length.to_le_bytes());
            self.request(json!({"type":"write","uuid":CONTROL,"data":STANDARD.encode(packet),"response":false}),seconds.min(5.0)).await?;
            loop {
                ensure!(self.connected,"Ring disconnected during Telesto read address=0x{address:08x}");
                if let Some(data)=response.complete()? {
                    self.output.debug(format!("BLE read address=0x{address:08x} bytes={} first_notification_ms={:?} total_ms={:.1}", data.len(), first_notification, started.elapsed().as_secs_f64()*1000.0));
                    return Ok(data);
                }
                let event=match self.pending.pop_front() { Some(e)=>e,None=>self.helper.event().await? };
                self.observe(&event);
                if event["type"]=="error" { bail!("Bluetooth: {}",event["text"]); }
                if event["type"]!="notification" { continue; }
                if event["uuid"] != CONTROL && event["uuid"] != DATA { continue; }
                first_notification.get_or_insert_with(|| started.elapsed().as_secs_f64()*1000.0);
                let bytes=STANDARD.decode(event["data"].as_str().context("Missing notification payload")?)?;
                response.add(event["uuid"].as_str().unwrap_or(""),&bytes)?;
            }
        }).await;
        let result = result.with_context(||format!("Telesto read stalled address=0x{address:08x} control_bytes={} data_bytes={} connected={}",response.control.len(),response.data.len(),self.connected)).and_then(|r| r);
        self.metrics.record(address, started.elapsed(), &result);
        self.metrics.report(&self.output, false);
        if let Err(error) = &result {
            self.output.debug(format!(
                "BLE read failed address=0x{address:08x} elapsed_ms={:.1} error={error:#}",
                started.elapsed().as_secs_f64() * 1_000.
            ));
        }
        result
    }
    pub async fn state(&mut self, seconds: f64) -> Result<RingState> {
        let data = self.read(0x4003000e, 10, seconds).await?;
        ensure!(data.len() == 10, "Invalid advertisement read response");
        self.output.debug(format!(
            "Ring state raw={}",
            data.iter().map(|b| format!("{b:02x}")).collect::<String>()
        ));
        advertisement(&data[2..])
    }
    pub async fn range(&mut self, seconds: f64) -> Result<(u16, u16)> {
        let data = self.read(0x40030005, 4, seconds).await?;
        ensure!(data.len() == 4, "Invalid collection range response");
        let start = u16::from_le_bytes(data[..2].try_into().unwrap());
        let end = u16::from_le_bytes(data[2..].try_into().unwrap());
        ensure!(
            end.wrapping_sub(start) <= 512,
            "Collection range exceeds native capacity"
        );
        Ok((start, end))
    }
    pub async fn collection(&mut self, index: u16, seconds: f64) -> Result<Vec<u8>> {
        self.read(0x40020000 | index as u32, 0, seconds).await
    }
    pub async fn close(&mut self) {
        self.disconnect().await;
        self.helper.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn advertisement_baseline_matches_the_native_watch_filter() {
        let state = advertisement(&[1, 0, 0, 0, 2, 0xa0]).unwrap();
        assert_eq!(state.advertisement_signature(), "00000001:2:1:1");
    }
    #[test]
    fn request_metrics_include_failed_reads_and_keep_s_r_c_separate() {
        let mut metrics = ReadMetrics::default();
        metrics.record(0x4003000e, Duration::from_millis(10), &Ok(vec![0; 10]));
        metrics.record(0x40030005, Duration::from_millis(20), &Ok(vec![0; 4]));
        metrics.record(
            0x4002ffff,
            Duration::from_millis(30),
            &Err(anyhow::anyhow!("disconnected")),
        );
        assert_eq!(metrics.attempts, [1, 1, 1, 0]);
        assert_eq!(metrics.errors, [0, 0, 1, 0]);
        assert_eq!(metrics.elapsed_ms, [10., 20., 30., 0.]);
        assert_eq!(metrics.bytes, 14);
    }
    fn header(size: u32) -> Vec<u8> {
        [0u32.to_le_bytes(), 0u32.to_le_bytes(), size.to_le_bytes()].concat()
    }
    #[test]
    fn fragmented_control_and_early_data() {
        let mut response = Response::default();
        response.add(DATA, b"abc").unwrap();
        assert!(response.complete().unwrap().is_none());
        for fragment in header(5).chunks(2) {
            response.add(CONTROL, fragment).unwrap();
        }
        assert!(response.complete().unwrap().is_none());
        response.add(DATA, b"de").unwrap();
        assert_eq!(response.complete().unwrap().unwrap(), b"abcde");
    }
    #[test]
    fn late_control_after_maximum_data() {
        let mut response = Response::default();
        let bytes = vec![42; MAX_SIZE];
        for chunk in bytes.chunks(174) {
            response.add(DATA, chunk).unwrap();
        }
        response.add(CONTROL, &header(MAX_SIZE as u32)).unwrap();
        assert_eq!(response.complete().unwrap(), Some(bytes));
    }
    #[test]
    fn oversized_error_and_empty_responses() {
        let mut response = Response::default();
        response.add(CONTROL, &header(0)).unwrap();
        assert_eq!(response.complete().unwrap(), Some(vec![]));
        assert!(response.add(CONTROL, &[1]).is_err());
        let mut response = Response::default();
        response.add(CONTROL, &header(MAX_SIZE as u32 + 1)).unwrap();
        assert!(response.complete().is_err());
        let mut response = Response::default();
        let mut control = header(0);
        control[0] = 1;
        response.add(CONTROL, &control).unwrap();
        assert!(response.complete().is_err());
    }
}
