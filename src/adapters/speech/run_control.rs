//! Out-of-band execution control: the ordinary result consumer may be waiting
//! for a final result while another recording needs the model's compute slot.
use super::Reply;
use crate::{
    helper::{HelperInput, Observer},
    output::Output,
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::json;
use std::{
    future::Future,
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, watch};

#[derive(Clone, Debug)]
pub struct RunState {
    request: u64,
    pub permitted: bool,
    paused: bool,
    failure: Option<String>,
}
impl Default for RunState {
    fn default() -> Self {
        Self {
            request: 0,
            permitted: true,
            paused: false,
            failure: None,
        }
    }
}
pub trait ExecutionControl: Send + Sync {
    /// Disabling returns only once the model owner is idle or suspended at a
    /// synchronized computation boundary. It does not cancel the recording.
    fn set_permitted(&self, enabled: bool) -> Reply<'_, ()>;
    fn activity(&self) -> watch::Receiver<RunState>;
}

#[derive(Clone)]
struct ControlBinding {
    revision: u64,
    control: Option<Arc<dyn ExecutionControl>>,
}

/// A stable permit endpoint while its owning speech task replaces a failed
/// process. None is published only AFTER that process has been closed/reaped.
/// Permit callers can then treat it as quiescent and retain the desired permit
/// for the replacement. A failed permit also wakes the owner's recovery loop.
pub struct RestartableControl {
    binding: watch::Sender<ControlBinding>,
    restart: watch::Sender<u64>,
    desired: StdMutex<bool>,
    serial: Mutex<()>,
    absent: watch::Receiver<RunState>,
}
impl RestartableControl {
    pub fn new(control: Arc<dyn ExecutionControl>, restart: watch::Sender<u64>) -> Self {
        let (binding, _) = watch::channel(ControlBinding {
            revision: 0,
            control: Some(control),
        });
        let (_, absent) = watch::channel(RunState {
            permitted: false,
            paused: true,
            ..Default::default()
        });
        Self {
            binding,
            restart,
            desired: StdMutex::new(true),
            serial: Mutex::new(()),
            absent,
        }
    }
    pub fn detached(&self) {
        self.binding.send_modify(|binding| {
            binding.revision += 1;
            binding.control = None;
        });
    }
    pub async fn install(&self, control: Arc<dyn ExecutionControl>) -> Result<()> {
        // Do not lock serial here: a permit caller may be waiting for this
        // replacement while holding it. No PCM is replayed before install.
        loop {
            let requested = *self.desired.lock().unwrap();
            control.set_permitted(requested).await?;
            let desired = self.desired.lock().unwrap();
            if *desired == requested {
                self.binding.send_modify(|binding| {
                    binding.revision += 1;
                    binding.control = Some(control.clone());
                });
                return Ok(());
            }
        }
    }
}
impl ExecutionControl for RestartableControl {
    fn activity(&self) -> watch::Receiver<RunState> {
        self.binding
            .borrow()
            .control
            .as_ref()
            .map_or_else(|| self.absent.clone(), |control| control.activity())
    }
    fn set_permitted(&self, enabled: bool) -> Reply<'_, ()> {
        Box::pin(async move {
            let _serial = self.serial.lock().await;
            *self.desired.lock().unwrap() = enabled;
            let mut updates = self.binding.subscribe();
            loop {
                let binding = updates.borrow_and_update().clone();
                let Some(control) = binding.control else {
                    return Ok(());
                };
                let result = control.set_permitted(enabled).await;
                {
                    // Keep publication excluded through the restart request: a
                    // delayed error from the old child must not restart a new one.
                    let current = self.binding.borrow();
                    if current.revision != binding.revision {
                        continue; // A late ACK from the old process grants nothing.
                    }
                    if result.is_ok() {
                        return Ok(());
                    }
                    ensure!(
                        !self.restart.is_closed(),
                        "Speech permit owner closed: {}",
                        result.unwrap_err()
                    );
                    self.restart.send_modify(|generation| *generation += 1);
                }
                // The owner listens even while awaiting this very permit. Its
                // recovery path cancels that future, closes the old process,
                // and publishes a detached slot before starting a replacement.
                updates
                    .changed()
                    .await
                    .context("Speech permit binding closed")?;
            }
        })
    }
}
pub struct NativeControl {
    input: HelperInput,
    activity: watch::Receiver<RunState>,
    serial: Mutex<u64>,
    output: Output,
    mode: String,
}
impl NativeControl {
    pub fn observe() -> (watch::Receiver<RunState>, Observer) {
        let (tx, rx) = watch::channel(RunState::default());
        let observer: Observer = Box::new(move |event| {
            let failure = match event {
                Err(error) => Some(error.to_string()),
                Ok(event) if event["type"] == "error" => Some(event["text"].to_string()),
                _ => None,
            };
            if let Some(failure) = failure {
                tx.send_modify(|state| state.failure = Some(failure));
            } else if let Ok(event) = event
                && let (Some(request), Some(permitted), Some(paused)) = (
                    event["permit_request"].as_u64(),
                    event["permitted"].as_bool(),
                    event["paused"].as_bool(),
                )
            {
                if event["protocol_version"] != crate::qwen_runtime::PROTOCOL {
                    tx.send_modify(|s| {
                        s.failure = Some("Invalid Qwen execution control version".into())
                    });
                    return;
                }
                tx.send_modify(|state| {
                    if request >= state.request {
                        state.request = request;
                        state.permitted = permitted;
                        state.paused = paused;
                    }
                });
            }
        });
        (rx, observer)
    }
    pub fn new(
        input: HelperInput,
        activity: watch::Receiver<RunState>,
        output: Output,
        mode: String,
    ) -> Self {
        Self {
            input,
            activity,
            serial: Mutex::new(0),
            output,
            mode,
        }
    }
}
impl ExecutionControl for NativeControl {
    fn activity(&self) -> watch::Receiver<RunState> {
        self.activity.clone()
    }
    fn set_permitted(&self, enabled: bool) -> Reply<'_, ()> {
        Box::pin(async move {
            let started = Instant::now();
            tokio::time::timeout(Duration::from_secs(10), async {
                let mut serial = self.serial.lock().await;
                *serial += 1;
                let request = *serial;
                let mut state = self.activity.clone();
                self.input.send(&json!({"type":"permit","protocol_version":crate::qwen_runtime::PROTOCOL,"request":request,"enabled":enabled})).await?;
                loop {
                    {
                        let current = state.borrow_and_update();
                        ensure!(current.failure.is_none(), "Speech control failed: {:?}", current.failure);
                        if current.request == request && current.permitted == enabled && (enabled || current.paused) {
                            break;
                        }
                    }
                    state.changed().await.context("Speech execution control closed")?;
                }
                Ok::<_, anyhow::Error>(())
            }).await.context("Qwen did not reach a computation boundary within 10 seconds")??;
            self.output.debug(format!(
                "[{}] execution permitted={enabled}; boundary acknowledgement={:.3}s",
                self.mode,
                started.elapsed().as_secs_f64()
            ));
            Ok(())
        })
    }
}

/// One live session owns the slot while it is receiving PCM. The batch model
/// retains its in-progress window and decoder state during that time.
pub struct LivePriority {
    live: Option<Arc<dyn ExecutionControl>>,
    batch: Arc<dyn ExecutionControl>,
    held: bool,
}
impl LivePriority {
    pub async fn new(
        live: Option<Arc<dyn ExecutionControl>>,
        batch: Arc<dyn ExecutionControl>,
    ) -> Result<Self> {
        let mut priority = Self {
            live,
            batch,
            held: true,
        };
        priority.release().await?;
        Ok(priority)
    }
    pub async fn acquire(&mut self) -> Result<()> {
        if !self.held {
            self.batch.set_permitted(false).await?;
            if let Some(live) = &self.live {
                live.set_permitted(true).await?;
            }
            self.held = true;
        }
        Ok(())
    }
    pub async fn release(&mut self) -> Result<()> {
        if self.held {
            if let Some(live) = &self.live {
                live.set_permitted(false).await?;
            }
            self.batch.set_permitted(true).await?;
            self.held = false;
        }
        Ok(())
    }
    /// After replacing a worker, restore both grants even if a failure happened
    /// half way through an acquire/release operation.
    pub async fn reconcile(&mut self, active: bool) -> Result<()> {
        self.held = !active;
        if active {
            self.acquire().await
        } else {
            self.release().await
        }
    }
}

/// A deliberately paused batch must not time out while the user dictates a
/// longer next recording. The result/EOF future continues to be polled.
pub async fn active_timeout<T>(
    budget: Duration,
    mut activity: Option<watch::Receiver<RunState>>,
    work: impl Future<Output = Result<T>>,
) -> Result<T> {
    let Some(activity) = activity.as_mut() else {
        return tokio::time::timeout(budget, work)
            .await
            .context("Speech response timed out")?;
    };
    tokio::pin!(work);
    let mut remaining = budget;
    loop {
        let permitted = {
            let state = activity.borrow_and_update();
            if let Some(failure) = &state.failure {
                bail!("Speech worker failed: {failure}");
            }
            state.permitted
        };
        let began = tokio::time::Instant::now();
        let timeout = async {
            if permitted {
                tokio::time::sleep(remaining).await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        tokio::select! {
            biased;
            result = &mut work => return result,
            changed = activity.changed() => {
                changed.context("Speech activity monitor closed")?;
                if permitted { remaining = remaining.saturating_sub(began.elapsed()); }
                if let Some(failure) = &activity.borrow().failure { bail!("Speech worker failed: {failure}"); }
            }
            _ = timeout => bail!("Speech response exceeded its active execution budget"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;
    struct FailedControl(watch::Receiver<RunState>);
    impl ExecutionControl for FailedControl {
        fn activity(&self) -> watch::Receiver<RunState> {
            self.0.clone()
        }
        fn set_permitted(&self, _: bool) -> Reply<'_, ()> {
            Box::pin(async { bail!("synthetic worker exited") })
        }
    }
    #[tokio::test]
    async fn failed_permit_requests_only_its_owner_and_waits_until_closed() {
        let (_, activity) = watch::channel(RunState::default());
        let (restart, mut requests) = watch::channel(0);
        let slot = Arc::new(RestartableControl::new(
            Arc::new(FailedControl(activity.clone())),
            restart,
        ));
        let task = tokio::spawn({
            let slot = slot.clone();
            async move { slot.set_permitted(false).await }
        });
        tokio::time::timeout(Duration::from_secs(1), requests.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(*requests.borrow(), 1);
        assert!(
            !task.is_finished(),
            "A failure is not proof that computation has stopped"
        );
        slot.detached(); // The owning speech task has closed/reaped its child.
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let trace = Arc::new(StdMutex::new(vec![]));
        slot.install(Arc::new(Fake {
            name: "replacement",
            trace: trace.clone(),
            activity,
        }))
        .await
        .unwrap();
        assert_eq!(*trace.lock().unwrap(), ["replacement=false"]);
        slot.set_permitted(true).await.unwrap();
        assert_eq!(
            *trace.lock().unwrap(),
            ["replacement=false", "replacement=true"]
        );
    }

    struct SlowInstall {
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
        first: std::sync::atomic::AtomicBool,
        trace: StdMutex<Vec<bool>>,
        activity: watch::Receiver<RunState>,
    }
    impl ExecutionControl for SlowInstall {
        fn activity(&self) -> watch::Receiver<RunState> {
            self.activity.clone()
        }
        fn set_permitted(&self, permitted: bool) -> Reply<'_, ()> {
            Box::pin(async move {
                self.trace.lock().unwrap().push(permitted);
                if self.first.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    self.entered.notify_one();
                    self.release.notified().await;
                }
                Ok(())
            })
        }
    }
    #[tokio::test]
    async fn permit_changed_during_install_is_applied_before_publication() {
        let (_, activity) = watch::channel(RunState::default());
        let (restart, _requests) = watch::channel(0);
        let slot = Arc::new(RestartableControl::new(
            Arc::new(FailedControl(activity.clone())),
            restart,
        ));
        slot.detached();
        let replacement = Arc::new(SlowInstall {
            entered: Default::default(),
            release: Default::default(),
            first: std::sync::atomic::AtomicBool::new(true),
            trace: StdMutex::new(vec![]),
            activity,
        });
        let install = tokio::spawn({
            let slot = slot.clone();
            let replacement = replacement.clone();
            async move { slot.install(replacement).await }
        });
        replacement.entered.notified().await;
        slot.set_permitted(false).await.unwrap();
        replacement.release.notify_one();
        tokio::time::timeout(Duration::from_secs(1), install)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(*replacement.trace.lock().unwrap(), [true, false]);
    }
    #[tokio::test]
    async fn an_old_ack_is_reapplied_to_the_current_worker() {
        let (_, activity) = watch::channel(RunState::default());
        let old = Arc::new(SlowInstall {
            entered: Default::default(),
            release: Default::default(),
            first: std::sync::atomic::AtomicBool::new(true),
            trace: StdMutex::new(vec![]),
            activity: activity.clone(),
        });
        let (restart, requests) = watch::channel(0);
        let slot = Arc::new(RestartableControl::new(old.clone(), restart));
        let permit = tokio::spawn({
            let slot = slot.clone();
            async move { slot.set_permitted(false).await }
        });
        old.entered.notified().await;
        slot.detached();
        let trace = Arc::new(StdMutex::new(vec![]));
        slot.install(Arc::new(Fake {
            name: "new",
            trace: trace.clone(),
            activity,
        }))
        .await
        .unwrap();
        old.release.notify_one();
        tokio::time::timeout(Duration::from_secs(1), permit)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(*trace.lock().unwrap(), ["new=false", "new=false"]);
        assert_eq!(*requests.borrow(), 0);
    }
    #[tokio::test]
    async fn pause_requires_boundary_ack_even_without_a_result_consumer() {
        let output = Output::new(false, None).unwrap();
        let (activity, observer) = NativeControl::observe();
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", r#"
read line
printf '%s\n' '{"type":"status","protocol_version":2,"permit_request":1,"permitted":false,"paused":false}'
sleep 0.08
printf '%s\n' '{"type":"status","protocol_version":2,"permit_request":1,"permitted":false,"paused":true}'
cat >/dev/null
"#]);
        let mut helper = crate::helper::Helper::spawn_observed(
            command,
            output.clone(),
            "mock".into(),
            Some(observer),
        )
        .await
        .unwrap();
        let control = NativeControl::new(helper.input(), activity, output, "mock".into());
        let began = Instant::now();
        control.set_permitted(false).await.unwrap();
        assert!(began.elapsed() >= Duration::from_millis(60));
        // The control clone must not keep stdin open after Helper::close().
        tokio::time::timeout(Duration::from_secs(1), helper.close())
            .await
            .unwrap();
        assert!(control.set_permitted(true).await.is_err());
    }
    #[tokio::test]
    async fn eof_while_awaiting_pause_is_an_error_not_a_grant() {
        let output = Output::new(false, None).unwrap();
        let (activity, observer) = NativeControl::observe();
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", r#"read line; printf '%s\n' '{"type":"status","protocol_version":2,"permit_request":1,"permitted":false,"paused":false}'; exit 0"#]);
        let mut helper = crate::helper::Helper::spawn_observed(
            command,
            output.clone(),
            "mock".into(),
            Some(observer),
        )
        .await
        .unwrap();
        let control = NativeControl::new(helper.input(), activity, output, "mock".into());
        assert!(
            tokio::time::timeout(Duration::from_secs(1), control.set_permitted(false))
                .await
                .unwrap()
                .is_err()
        );
        helper.close().await;
    }
    struct Fake {
        name: &'static str,
        trace: Arc<StdMutex<Vec<String>>>,
        activity: watch::Receiver<RunState>,
    }
    impl ExecutionControl for Fake {
        fn activity(&self) -> watch::Receiver<RunState> {
            self.activity.clone()
        }
        fn set_permitted(&self, enabled: bool) -> Reply<'_, ()> {
            Box::pin(async move {
                self.trace
                    .lock()
                    .unwrap()
                    .push(format!("{}={enabled}", self.name));
                Ok(())
            })
        }
    }
    #[tokio::test]
    async fn slot_transfer_stops_previous_owner_before_resuming_next() {
        let trace = Arc::new(StdMutex::new(Vec::new()));
        let (_, rx) = watch::channel(RunState::default());
        let control = |name| {
            Arc::new(Fake {
                name,
                trace: trace.clone(),
                activity: rx.clone(),
            }) as Arc<dyn ExecutionControl>
        };
        let mut priority = LivePriority::new(Some(control("live")), control("batch"))
            .await
            .unwrap();
        priority.acquire().await.unwrap();
        priority.acquire().await.unwrap();
        priority.release().await.unwrap();
        priority.release().await.unwrap();
        assert_eq!(
            *trace.lock().unwrap(),
            [
                "live=false",
                "batch=true",
                "batch=false",
                "live=true",
                "live=false",
                "batch=true"
            ]
        );
    }
    #[tokio::test]
    async fn paused_time_is_not_charged_but_failure_is_observed() {
        let (tx, rx) = watch::channel(RunState {
            permitted: false,
            paused: true,
            ..Default::default()
        });
        let work = async {
            tokio::time::sleep(Duration::from_millis(80)).await;
            Ok(7)
        };
        assert_eq!(
            active_timeout(Duration::from_millis(10), Some(rx.clone()), work)
                .await
                .unwrap(),
            7
        );
        tx.send_modify(|s| s.permitted = true);
        assert!(
            active_timeout(
                Duration::from_millis(10),
                Some(rx.clone()),
                std::future::pending::<Result<()>>()
            )
            .await
            .is_err()
        );
        let task = tokio::spawn(active_timeout(
            Duration::from_secs(10),
            Some(rx),
            std::future::pending::<Result<()>>(),
        ));
        tx.send_modify(|s| s.failure = Some("worker died".into()));
        assert!(
            tokio::time::timeout(Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
    }
}
