//! Execute a reception-owned cancellation intent against its exact recording.
use super::*;
use tokio::sync::watch;

pub(super) struct Cancellation {
    pending: Vec<String>,
    cancelled: HashSet<String>,
    changed: watch::Sender<u64>,
}
impl Default for Cancellation {
    fn default() -> Self {
        Self {
            pending: vec![],
            cancelled: HashSet::new(),
            changed: watch::channel(0).0,
        }
    }
}
impl Cancellation {
    pub fn begin(&mut self, key: &str) {
        if !self.is_cancelled(key) && !self.pending.iter().any(|k| k == key) {
            self.pending.push(key.into());
        }
    }
    pub fn finish(&mut self, key: &str) {
        self.pending.retain(|k| k != key);
    }
    pub fn retire(&mut self, key: &str) {
        self.finish(key);
        self.cancelled.remove(key);
    }
    pub fn is_cancelled(&self, key: &str) -> bool {
        self.cancelled.contains(key)
    }
    pub fn cancel(&mut self, key: &str) -> bool {
        if !self.pending.iter().any(|pending| pending == key) {
            return false;
        }
        self.finish(key);
        self.cancelled.insert(key.into());
        self.changed
            .send_modify(|generation| *generation = generation.wrapping_add(1));
        true
    }
    pub async fn wait(lifecycle: Arc<Mutex<Lifecycle>>, key: String) {
        let mut changed = lifecycle.lock().unwrap().cancellation.changed.subscribe();
        loop {
            if lifecycle.lock().unwrap().cancellation.is_cancelled(&key) {
                return;
            }
            if changed.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
}

#[derive(Debug)]
pub(super) struct UserCancelled;
impl std::fmt::Display for UserCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Recognition cancelled by single tap")
    }
}
impl std::error::Error for UserCancelled {}
