use crate::{bluetooth::Bluetooth, output::Output};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

pub trait Transport {
    async fn request(&mut self, message: Value, seconds: f64) -> Result<Value>;
}
impl Transport for Bluetooth {
    async fn request(&mut self, message: Value, seconds: f64) -> Result<Value> {
        Bluetooth::request(self, message, seconds).await
    }
}

pub fn normalize_address(address: &str) -> Result<String> {
    Ok(uuid::Uuid::parse_str(address)
        .context("Invalid ring UUID")?
        .to_string()
        .to_uppercase())
}

pub async fn pair(
    ble: &mut impl Transport,
    explicit: Option<String>,
    saved: Option<String>,
    seconds: f64,
    output: &Output,
) -> Result<String> {
    let address = if let Some(address) = explicit.or(saved) {
        normalize_address(&address)?
    } else {
        output.error("探索中です。見つからない場合はリングのボタンを押してください。");
        let devices = ble
            .request(json!({"type":"scan","first":true}), seconds)
            .await?;
        let devices = devices.as_array().context("Invalid scan response")?;
        ensure!(
            devices.len() == 1,
            "No Index found; press the ring button and retry pair"
        );
        normalize_address(devices[0]["address"].as_str().context("Missing address")?)?
    };
    Ok(address)
}

#[cfg(test)]
mod tests {
    use super::*;
    const UUID: &str = "B727FE94-D092-6484-A27E-A94795942322";
    #[derive(Default)]
    struct Fake {
        calls: Vec<String>,
        fail: bool,
    }
    impl Transport for Fake {
        async fn request(&mut self, message: Value, _: f64) -> Result<Value> {
            let kind = message["type"].as_str().unwrap();
            self.calls.push(kind.into());
            assert_eq!(kind, "scan");
            assert_eq!(message["first"], true);
            ensure!(!self.fail, "Scan failed");
            Ok(json!([{"address":UUID}]))
        }
    }
    #[tokio::test]
    async fn first_pair_and_repeated_pair_return_same_device() {
        let out = Output::new(false, None).unwrap();
        let mut ble = Fake::default();
        let first = pair(&mut ble, None, None, 30.0, &out).await.unwrap();
        assert_eq!(ble.calls, ["scan"]);
        ble.calls.clear();
        let second = pair(&mut ble, None, Some(first.clone()), 30.0, &out)
            .await
            .unwrap();
        assert_eq!(first, second);
        assert!(ble.calls.is_empty());
    }
    #[tokio::test]
    async fn explicit_uuid_does_not_use_bluetooth() {
        let out = Output::new(false, None).unwrap();
        let mut ble = Fake {
            fail: true,
            ..Fake::default()
        };
        assert_eq!(
            pair(&mut ble, Some(UUID.to_lowercase()), None, 30.0, &out)
                .await
                .unwrap(),
            UUID
        );
        assert!(ble.calls.is_empty());
    }
    #[tokio::test]
    async fn explicit_device_overrides_saved_selection() {
        let out = Output::new(false, None).unwrap();
        let mut ble = Fake::default();
        pair(
            &mut ble,
            Some(UUID.into()),
            Some("other".into()),
            30.0,
            &out,
        )
        .await
        .unwrap();
        assert!(ble.calls.is_empty());
    }
    #[tokio::test]
    async fn failed_scan_does_not_select_a_device() {
        let out = Output::new(false, None).unwrap();
        let mut ble = Fake {
            fail: true,
            ..Fake::default()
        };
        assert!(pair(&mut ble, None, None, 30.0, &out).await.is_err());
    }
}
