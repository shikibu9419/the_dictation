use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Event {
    pub r#type: String,
    pub collecting: Option<bool>,
    pub recording: Option<String>,
    pub text: Option<String>,
    pub mode: Option<String>,
    pub r#final: Option<bool>,
}
impl Event {
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            r#type: "error".into(),
            text: Some(text.into()),
            collecting: None,
            recording: None,
            mode: None,
            r#final: None,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Phase {
    Recording,
    Receiving,
    Finalizing,
    Ready,
    Failed,
}
#[derive(Debug)]
pub struct Item {
    pub id: u64,
    pub recording: Option<String>,
    pub target: i32,
    pub text: String,
    pub phase: Phase,
    dismissed: bool,
}
impl Item {
    fn in_history(&self) -> bool {
        self.phase == Phase::Ready && !self.text.trim().is_empty()
    }
}
#[derive(Default)]
pub struct Model {
    pub items: Vec<Item>,
    pub error: Option<String>,
    pub ready: bool,
    collecting: bool,
    active: Option<u64>,
    capturing: Option<u64>,
    next: u64,
}
impl Model {
    pub fn has_inflight(&self) -> bool {
        self.items.iter().any(|item| {
            matches!(
                item.phase,
                Phase::Recording | Phase::Receiving | Phase::Finalizing
            )
        })
    }

    pub fn visible(&self) -> Option<&Item> {
        self.items
            .iter()
            .find(|i| Some(i.id) == self.active && !i.dismissed)
    }
    fn add(&mut self, recording: Option<String>, phase: Phase, target: i32) {
        self.next += 1;
        self.items.push(Item {
            id: self.next,
            recording,
            target,
            phase,
            text: String::new(),
            dismissed: false,
        });
        self.active = Some(self.next);
    }
    pub fn accept(&mut self, e: Event, target: i32) {
        match e.r#type.as_str() {
            "ready" => self.ready = true,
            "state" => {
                if let Some(value) = e.collecting
                    && value != self.collecting
                {
                    self.collecting = value;
                    if value {
                        self.error = None;
                        // A BLE state edge has no recording identity. Keep the
                        // current transcript until audio confirms the next one.
                        let previous = self.visible().filter(|i| !i.text.is_empty() && matches!(i.phase, Phase::Recording | Phase::Receiving | Phase::Finalizing)).map(|i| i.id);
                        self.add(None, Phase::Recording, target);
                        self.capturing = self.active;
                        if previous.is_some() { self.active = previous; }
                    } else if let Some(item) =
                        self.items.iter_mut().find(|i| Some(i.id) == self.capturing)
                        && item.phase == Phase::Recording
                    {
                        item.phase = Phase::Receiving;
                    }
                }
            }
            "recording" => {
                if let Some(key) = e.recording
                    && !self
                        .items
                        .iter()
                        .any(|i| i.recording.as_ref() == Some(&key))
                {
                    if let Some(item) = self.items.iter_mut().find(|i| i.recording.is_none() && matches!(i.phase, Phase::Recording | Phase::Receiving)) {
                        item.recording = Some(key);
                        if !item.dismissed { self.active = Some(item.id); }
                    } else {
                        self.add(
                            Some(key),
                            if self.collecting {
                                Phase::Recording
                            } else {
                                Phase::Receiving
                            },
                            target,
                        );
                    }
                }
            }
            "text" | "finalizing" | "discarded" => {
                let Some(key) = e.recording else { return };
                let Some(item) = self
                    .items
                    .iter_mut()
                    .find(|i| i.recording.as_ref() == Some(&key))
                else {
                    return;
                };
                match e.r#type.as_str() {
                    "discarded" => {
                        item.phase = Phase::Failed;
                        item.text = e.text.unwrap_or_default();
                    }
                    "finalizing" if matches!(item.phase, Phase::Recording | Phase::Receiving) => {
                        item.phase = Phase::Finalizing;
                    }
                    "finalizing" => {},
                    _ if e.mode.as_deref() == Some("batch") && e.r#final == Some(true) => {
                        item.phase = Phase::Ready;
                        item.text = e.text.unwrap_or_default();
                    }
                    _ if e.mode.as_deref() == Some("live")
                        && e.r#final != Some(true)
                        && item.phase == Phase::Recording
                        && !item.dismissed =>
                    {
                        item.text = e.text.unwrap_or_default()
                    }
                    _ if e.mode.as_deref() == Some("batch")
                        && matches!(item.phase, Phase::Recording | Phase::Receiving | Phase::Finalizing) =>
                    {
                        item.phase = Phase::Finalizing;
                        if let Some(text) = e.text.filter(|text| !text.is_empty()) {
                            item.text = text;
                        }
                    }
                    _ => {}
                }
                self.prune();
            }
            "error" => {
                self.ready = false;
                self.error = e.text;
                self.collecting = false;
                for item in &mut self.items {
                    if matches!(
                        item.phase,
                        Phase::Recording | Phase::Receiving | Phase::Finalizing
                    ) {
                        item.phase = Phase::Failed;
                    }
                }
                self.prune();
            }
            _ => {}
        }
    }
    pub fn dismiss(&mut self) {
        self.error = None;
        for item in &mut self.items { item.dismissed = true; }
        self.active = None;
        self.prune();
    }
    pub fn dismiss_id(&mut self, id: u64) {
        if let Some(item) = self.items.iter_mut().find(|i| i.id == id) {
            item.dismissed = true;
        }
        if self.active == Some(id) {
            self.active = None;
        }
        self.prune();
    }
    fn prune(&mut self) {
        self.items
            .retain(|i| !(i.dismissed && (i.phase == Phase::Failed || (i.phase == Phase::Ready && !i.in_history()))));
        let mut excess = self.items.iter().filter(|i| i.in_history()).count().saturating_sub(200);
        self.items.retain(|i| {
            if excess > 0 && i.in_history() && Some(i.id) != self.active {
                excess -= 1;
                false
            } else { true }
        });
    }
    pub fn history(&self) -> Vec<String> {
        self.items.iter().filter(|i| i.in_history()).map(|i| i.text.clone()).collect()
    }
    pub fn restore_history(&mut self, texts: Vec<String>) {
        for text in texts.into_iter().filter(|text| !text.trim().is_empty()).rev().take(200).collect::<Vec<_>>().into_iter().rev() {
            self.add(None, Phase::Ready, 0);
            let item = self.items.last_mut().unwrap();
            item.text = text;
            item.dismissed = true;
        }
        self.active = None;
    }
    pub fn browse(&mut self, older: bool, target: i32) -> bool {
        let current = self.active;
        let candidate = if older {
            self.items.iter().rev().find(|i| i.in_history() && current.is_none_or(|id| i.id < id))
        } else {
            self.items.iter().find(|i| i.in_history() && current.is_some_and(|id| i.id > id))
        }.map(|i| i.id);
        let Some(id) = candidate else { return false };
        self.active = Some(id);
        let item = self.items.iter_mut().find(|i| i.id == id).unwrap();
        item.dismissed = false;
        item.target = target;
        true
    }
    pub fn open_history(&mut self, target: i32) {
        self.active = None;
        self.browse(true, target);
    }
    pub fn edit(&mut self, id: u64, text: String) {
        if let Some(item) = self
            .items
            .iter_mut()
            .find(|i| i.id == id && i.phase == Phase::Ready)
        {
            item.text = text;
        }
    }
    pub fn paste(&self) -> Option<(&str, i32)> {
        let i = self.visible()?;
        (self.error.is_none() && i.phase == Phase::Ready && !i.text.trim().is_empty())
            .then_some((&i.text, i.target))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    fn send(m: &mut Model, e: Value) {
        m.accept(serde_json::from_value(e).unwrap(), 42);
    }
    fn state(m: &mut Model, value: bool) {
        send(m, json!({"type":"state","collecting":value}));
    }
    fn begin(m: &mut Model, key: &str) {
        send(m, json!({"type":"recording","recording":key}));
    }
    fn text(m: &mut Model, key: &str, text: &str, mode: &str, final_result: bool) {
        send(
            m,
            json!({"type":"text","recording":key,"text":text,"mode":mode,"final":final_result}),
        );
    }
    #[test]
    fn ble_edges_live_release_and_full_result() {
        let mut m = Model::default();
        state(&mut m, false);
        assert!(m.visible().is_none());
        state(&mut m, true);
        state(&mut m, true);
        assert_eq!(m.items.len(), 1);
        assert_eq!(m.visible().unwrap().phase, Phase::Recording);
        begin(&mut m, "one");
        text(&mut m, "one", "途中", "live", false);
        assert_eq!(m.visible().unwrap().text, "途中");
        assert!(m.paste().is_none());
        state(&mut m, false);
        text(&mut m, "one", "遅延", "live", false);
        assert_eq!(m.visible().unwrap().text, "途中");
        assert_eq!(m.visible().unwrap().phase, Phase::Receiving);
        send(&mut m, json!({"type":"finalizing","recording":"one"}));
        assert_eq!(m.visible().unwrap().phase, Phase::Finalizing);
        text(&mut m, "one", "最終全文", "batch", true);
        assert_eq!(m.paste(), Some(("最終全文", 42)));
        m.dismiss();
        assert!(m.visible().is_none());
        assert_eq!(m.history(), vec!["最終全文"]);
    }
    #[test]
    fn consecutive_recordings_keep_previous_final_and_target() {
        let mut m = Model::default();
        state(&mut m, true);
        begin(&mut m, "one");
        state(&mut m, false);
        m.accept(
            serde_json::from_value(json!({"type":"state","collecting":true})).unwrap(),
            99,
        );
        begin(&mut m, "two");
        text(&mut m, "one", "一番", "batch", true);
        assert_eq!(m.visible().unwrap().recording.as_deref(), Some("two"));
        state(&mut m, false);
        text(&mut m, "two", "二番", "batch", true);
        assert_eq!(m.paste(), Some(("二番", 99)));
        m.dismiss();
        assert!(m.paste().is_none());
        assert_eq!(m.history(), vec!["一番", "二番"]);
    }
    #[test]
    fn escape_suppresses_late_results_and_empty_final_cannot_paste() {
        let mut m = Model::default();
        state(&mut m, true);
        m.dismiss();
        begin(&mut m, "one");
        text(&mut m, "one", "hidden", "batch", true);
        assert!(m.visible().is_none());
        state(&mut m, false);
        begin(&mut m, "two");
        text(&mut m, "two", "", "batch", true);
        assert_eq!(m.visible().unwrap().phase, Phase::Ready);
        assert!(m.paste().is_none());
    }
    #[test]
    fn errors_and_unknown_recordings_do_not_allow_paste() {
        let mut m = Model::default();
        text(&mut m, "unknown", "stale", "batch", true);
        assert!(m.visible().is_none());
        begin(&mut m, "one");
        text(&mut m, "one", "ready", "batch", true);
        m.accept(Event::error("disconnected"), 42);
        assert!(m.paste().is_none());
        assert_eq!(m.error.as_deref(), Some("disconnected"));
    }
    #[test]
    fn edits_apply_only_to_final_result_and_survive_other_events() {
        let mut m = Model::default();
        state(&mut m, true);
        begin(&mut m, "one");
        let id = m.visible().unwrap().id;
        m.edit(id, "premature".into());
        assert!(m.visible().unwrap().text.is_empty());
        state(&mut m, false);
        text(&mut m, "one", "original", "batch", true);
        m.edit(id, "修正した本文\n二行目".into());
        text(&mut m, "one", "late", "live", false);
        send(&mut m, json!({"type":"ready"}));
        assert_eq!(m.paste(), Some(("修正した本文\n二行目", 42)));
        m.edit(id, String::new());
        assert!(m.paste().is_none());
    }
    #[test]
    fn fatal_error_finishes_all_pending_phases_and_allows_dismissal() {
        for phase in [Phase::Recording, Phase::Receiving, Phase::Finalizing] {
            let mut m = Model::default();
            state(&mut m, true);
            begin(&mut m, "one");
            m.items[0].phase = phase;
            m.accept(Event::error("backend stopped"), 42);
            assert!(!m.has_inflight());
            assert_eq!(m.visible().unwrap().phase, Phase::Failed);
            m.dismiss();
            assert!(m.items.is_empty());
            state(&mut m, true);
            assert_eq!(m.visible().unwrap().phase, Phase::Recording);
        }
    }
    #[test]
    fn fatal_error_prunes_hidden_work_but_preserves_completed_text() {
        let mut m = Model::default();
        begin(&mut m, "ready");
        text(&mut m, "ready", "keep", "batch", true);
        state(&mut m, true);
        begin(&mut m, "pending");
        m.dismiss();
        m.accept(Event::error("backend stopped"), 42);
        assert!(!m.has_inflight());
        assert_eq!(m.items.len(), 1);
        assert_eq!(m.items[0].phase, Phase::Ready);
        assert_eq!(m.items[0].text, "keep");
    }
}

#[cfg(test)]
mod settings_switch_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn dismissed_recording_remains_inflight_until_final() {
        let mut model = Model::default();
        model.accept(
            serde_json::from_value(json!({"type":"state","collecting":true})).unwrap(),
            42,
        );
        model.accept(
            serde_json::from_value(json!({"type":"recording","recording":"mic-1"})).unwrap(),
            42,
        );
        model.dismiss();
        assert!(model.has_inflight());
        assert!(model.visible().is_none());
        model.accept(
            serde_json::from_value(json!({"type":"state","collecting":false})).unwrap(),
            42,
        );
        assert!(model.has_inflight());
        model.accept(serde_json::from_value(json!({"type":"text","recording":"mic-1","mode":"batch","final":true,"text":"result"})).unwrap(),42);
        assert!(!model.has_inflight());
        assert!(model.visible().is_none());
    }
}
