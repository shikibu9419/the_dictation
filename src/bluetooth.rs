use crate::{
    collection::u32le,
    helper::{Helper, executable},
    output::Output,
};
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::VecDeque, time::Duration};
use tokio::{process::Command, time::timeout};
pub const CONTROL: &str = "c0ef558a-2058-fabf-a140-8d5acde50b39";
pub const DATA: &str = "daad3d52-237c-90a7-b54b-8854a134d801";
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
    output: Output,
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
            output,
        })
    }
    fn observe(&mut self, event: &Value) {
        if event["type"] == "disconnected" {
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
                if event["type"] == "notification" {
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
    }
    pub async fn subscribe(&mut self, seconds: f64) -> Result<()> {
        for uuid in [CONTROL, DATA] {
            self.request(
                json!({"type":"notify","uuid":uuid,"enabled":true}),
                seconds.min(5.0),
            )
            .await?;
        }
        Ok(())
    }
    pub async fn unsubscribe(&mut self) {
        for uuid in [DATA, CONTROL] {
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
        let result=timeout(Duration::from_secs_f64(seconds.min(5.0)),async {
            let mut packet=vec![3]; packet.extend(address.to_le_bytes()); packet.extend(0u32.to_le_bytes()); packet.extend(length.to_le_bytes());
            self.request(json!({"type":"write","uuid":CONTROL,"data":STANDARD.encode(packet),"response":false}),seconds.min(5.0)).await?;
            loop {
                ensure!(self.connected,"Ring disconnected during Telesto read address=0x{address:08x}");
                if let Some(data)=response.complete()? { return Ok(data); }
                let event=match self.pending.pop_front() { Some(e)=>e,None=>self.helper.event().await? };
                self.observe(&event);
                if event["type"]=="error" { bail!("Bluetooth: {}",event["text"]); }
                if event["type"]!="notification" { continue; }
                let bytes=STANDARD.decode(event["data"].as_str().context("Missing notification payload")?)?;
                response.add(event["uuid"].as_str().unwrap_or(""),&bytes)?;
            }
        }).await;
        result.with_context(||format!("Telesto read stalled address=0x{address:08x} control_bytes={} data_bytes={} connected={}",response.control.len(),response.data.len(),self.connected))?
    }
    pub async fn state(&mut self, seconds: f64) -> Result<RingState> {
        let data = self.read(0x4003000e, 10, seconds).await?;
        ensure!(data.len() == 10, "Invalid advertisement read response");
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
