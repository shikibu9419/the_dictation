//! Bounded, nonblocking process mailboxes and cancellation-safe JSONL framing.
use anyhow::{Result, anyhow, bail};
use serde_json::Value;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt},
    sync::Notify,
};

pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_LOG_LINE_BYTES: usize = 256 * 1024;
pub const MAX_QUEUE_ITEMS: usize = 4096;
pub const MAX_QUEUE_BYTES: usize = 256 * 1024 * 1024;

/// Conservative payload accounting, including metadata and referenced PCM.
pub trait Weight {
    fn queued_bytes(&self) -> usize;
}
impl Weight for Value {
    fn queued_bytes(&self) -> usize {
        let payload = match self {
            Value::String(s) => s.capacity(),
            Value::Array(values) => values.iter().fold(
                values
                    .capacity()
                    .saturating_sub(values.len())
                    .saturating_mul(std::mem::size_of::<Value>()),
                |n, v| n.saturating_add(v.queued_bytes()),
            ),
            Value::Object(values) => values.iter().fold(0usize, |n, (k, v)| {
                n.saturating_add(128)
                    .saturating_add(k.capacity())
                    .saturating_add(v.queued_bytes())
            }),
            _ => 0,
        };
        std::mem::size_of::<Self>().saturating_add(payload)
    }
}
impl<T: Weight> Weight for Result<T> {
    fn queued_bytes(&self) -> usize {
        match self {
            Ok(value) => value.queued_bytes(),
            Err(error) => error.to_string().len().saturating_add(256),
        }
    }
}
struct State<T> {
    queue: VecDeque<(T, usize)>,
    bytes: usize,
    senders: usize,
    receiving: bool,
    failure: Option<String>,
}
struct Shared<T> {
    state: Mutex<State<T>>,
    ready: Notify,
    name: &'static str,
    items: usize,
    bytes: usize,
}
pub struct Sender<T>(Arc<Shared<T>>);
pub struct Receiver<T>(Arc<Shared<T>>);

pub fn channel<T>(name: &'static str, items: usize, bytes: usize) -> (Sender<T>, Receiver<T>) {
    assert!(items > 0 && bytes > 0);
    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            queue: VecDeque::new(),
            bytes: 0,
            senders: 1,
            receiving: true,
            failure: None,
        }),
        ready: Notify::new(),
        name,
        items,
        bytes,
    });
    (Sender(shared.clone()), Receiver(shared))
}
pub fn process_channel<T>(name: &'static str) -> (Sender<T>, Receiver<T>) {
    channel(name, MAX_QUEUE_ITEMS, MAX_QUEUE_BYTES)
}
impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        self.0.state.lock().unwrap().senders += 1;
        Self(self.0.clone())
    }
}
impl<T> Sender<T> {
    /// A terminal error has its own slot, so even a full mailbox reports it.
    pub fn fail(&self, error: impl std::fmt::Display) {
        self.0
            .state
            .lock()
            .unwrap()
            .failure
            .get_or_insert_with(|| format!("{}: {error}", self.0.name));
        self.0.ready.notify_one();
    }
}
impl<T: Weight> Sender<T> {
    pub fn send(&self, value: T) -> Result<()> {
        let bytes = value.queued_bytes();
        let mut state = self.0.state.lock().unwrap();
        if let Some(error) = &state.failure {
            bail!("{error}");
        }
        if !state.receiving {
            bail!("{} receiver closed", self.0.name);
        }
        if state.queue.len() >= self.0.items || bytes > self.0.bytes.saturating_sub(state.bytes) {
            let error = format!(
                "{} queue capacity exceeded: queued_items={}/{} queued_bytes={}/{} incoming_bytes={bytes}; input was not accepted",
                self.0.name,
                state.queue.len(),
                self.0.items,
                state.bytes,
                self.0.bytes
            );
            state.failure = Some(error.clone());
            drop(state);
            self.0.ready.notify_one();
            bail!("{error}");
        }
        state.bytes += bytes;
        state.queue.push_back((value, bytes));
        drop(state);
        self.0.ready.notify_one();
        Ok(())
    }
}
impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        self.0.state.lock().unwrap().senders -= 1;
        self.0.ready.notify_one();
    }
}
impl<T> Receiver<T> {
    pub async fn recv(&mut self) -> Result<Option<T>> {
        loop {
            // There is one receiver; notify_one retains a permit across this check.
            let ready = self.0.ready.notified();
            {
                let mut state = self.0.state.lock().unwrap();
                if let Some(error) = &state.failure {
                    bail!("{error}");
                }
                if let Some((value, bytes)) = state.queue.pop_front() {
                    state.bytes -= bytes;
                    return Ok(Some(value));
                }
                if state.senders == 0 {
                    return Ok(None);
                }
            }
            ready.await;
        }
    }
    pub fn try_recv(&mut self) -> Result<T> {
        let mut state = self.0.state.lock().unwrap();
        if let Some(error) = &state.failure {
            bail!("{error}");
        }
        let (value, bytes) = state
            .queue
            .pop_front()
            .ok_or_else(|| anyhow!("{} queue empty", self.0.name))?;
        state.bytes -= bytes;
        Ok(value)
    }
}
impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap();
        state.receiving = false;
        state.queue.clear();
        state.bytes = 0;
    }
}

pub struct Lines<R> {
    reader: R,
    partial: Vec<u8>,
    limit: usize,
    failed: bool,
    split: bool,
}
impl<R: AsyncBufRead + Unpin> Lines<R> {
    pub fn new(reader: R, limit: usize) -> Self {
        Self {
            reader,
            partial: Vec::new(),
            limit,
            failed: false,
            split: false,
        }
    }
    /// Diagnostic streams can be emitted in bounded fragments without losing
    /// a long line or treating it as a malformed protocol response.
    pub fn log_chunks(reader: R, limit: usize) -> Self {
        assert!(limit > 0);
        Self {
            split: true,
            ..Self::new(reader, limit)
        }
    }
    pub async fn next_bytes(&mut self) -> Result<Option<Vec<u8>>> {
        if self.failed {
            bail!("IPC line reader already failed");
        }
        loop {
            let chunk = self.reader.fill_buf().await?;
            if chunk.is_empty() {
                return Ok((!self.partial.is_empty()).then(|| std::mem::take(&mut self.partial)));
            }
            let end = chunk.iter().position(|b| *b == b'\n');
            let size = end.unwrap_or(chunk.len());
            if size > self.limit.saturating_sub(self.partial.len()) {
                if self.split {
                    let remaining = self.limit - self.partial.len();
                    self.partial.extend_from_slice(&chunk[..remaining]);
                    self.reader.consume(remaining);
                    return Ok(Some(std::mem::take(&mut self.partial)));
                }
                self.failed = true;
                bail!("IPC line exceeds {} bytes", self.limit);
            }
            self.partial.extend_from_slice(&chunk[..size]);
            self.reader.consume(size + usize::from(end.is_some()));
            if end.is_some() {
                if self.partial.last() == Some(&b'\r') {
                    self.partial.pop();
                }
                return Ok(Some(std::mem::take(&mut self.partial)));
            }
        }
    }
    pub async fn next_line(&mut self) -> Result<Option<String>> {
        self.next_bytes()
            .await?
            .map(String::from_utf8)
            .transpose()
            .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncWriteExt, BufReader};
    #[tokio::test]
    async fn item_overflow_is_terminal_and_cannot_hide_behind_queued_success() {
        let (tx, mut rx) = channel("audio", 2, 10000);
        tx.send(Value::Null).unwrap();
        tx.send(Value::Null).unwrap();
        assert!(
            tx.send(Value::Null)
                .unwrap_err()
                .to_string()
                .contains("queued_items=2/2")
        );
        assert!(
            rx.recv()
                .await
                .unwrap_err()
                .to_string()
                .contains("capacity exceeded")
        );
        assert!(tx.send(Value::Null).is_err());
    }
    #[tokio::test]
    async fn accounting_releases_capacity_and_cloned_senders_preserve_eof_order() {
        let unit = Value::Null.queued_bytes();
        let (tx, mut rx) = channel("audio", 5, unit);
        let other = tx.clone();
        tx.send(Value::Null).unwrap();
        assert_eq!(rx.recv().await.unwrap(), Some(Value::Null));
        other.send(Value::Null).unwrap();
        drop(tx);
        drop(other);
        assert_eq!(rx.recv().await.unwrap(), Some(Value::Null));
        assert_eq!(rx.recv().await.unwrap(), None);
    }
    #[tokio::test]
    async fn byte_overflow_and_explicit_failure_wake_the_receiver() {
        let (tx, mut rx) = channel("audio", 100, 1);
        assert!(
            tx.send(Value::Null)
                .unwrap_err()
                .to_string()
                .contains("incoming_bytes")
        );
        assert!(rx.recv().await.is_err());
        let (tx, mut rx) = channel::<Value>("events", 1, 1);
        let waiter = tokio::spawn(async move { rx.recv().await });
        tx.fail("helper exited");
        assert!(
            waiter
                .await
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("helper exited")
        );
    }
    #[tokio::test]
    async fn cancelled_read_keeps_partial_line_and_all_following_frames() {
        let (mut tx, reader) = tokio::io::duplex(64);
        let mut lines = Lines::new(BufReader::new(reader), 8);
        tx.write_all(b"abc").await.unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), lines.next_line())
                .await
                .is_err()
        );
        tx.write_all(b"def\r\n\nx").await.unwrap();
        drop(tx);
        assert_eq!(lines.next_line().await.unwrap().as_deref(), Some("abcdef"));
        assert_eq!(lines.next_line().await.unwrap().as_deref(), Some(""));
        assert_eq!(lines.next_line().await.unwrap().as_deref(), Some("x"));
        assert_eq!(lines.next_line().await.unwrap(), None);
    }
    #[tokio::test]
    async fn huge_unterminated_line_is_rejected_before_allocation_and_utf8_is_explicit() {
        let mut lines = Lines::new(&b"12345678901234567890"[..], 8);
        assert!(
            lines
                .next_line()
                .await
                .unwrap_err()
                .to_string()
                .contains("8 bytes")
        );
        assert!(lines.partial.len() <= 8);
        assert!(lines.next_bytes().await.is_err());
        let mut lines = Lines::new(&b"\xff\n"[..], 8);
        assert_eq!(lines.next_bytes().await.unwrap(), Some(vec![255]));
        let mut lines = Lines::new(&b"\xff\n"[..], 8);
        assert!(lines.next_line().await.is_err());
    }
    #[tokio::test]
    async fn diagnostics_are_split_without_discarding_a_long_unterminated_line() {
        let mut lines = Lines::log_chunks(&b"12345678901234567890\nabcdefghi"[..], 8);
        let mut chunks = vec![];
        while let Some(chunk) = lines.next_bytes().await.unwrap() {
            assert!(chunk.len() <= 8);
            chunks.push(chunk);
        }
        assert_eq!(chunks.concat(), b"12345678901234567890abcdefghi");
    }
}
