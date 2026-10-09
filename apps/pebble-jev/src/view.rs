//! Floating panel: mic indicator while the ring button is held, then the
//! streamed conversation and tool results.
use crate::{
    session::{self, Entry, SessionOptions, UiEvent},
    settings::Settings,
};
use futures::{StreamExt, channel::mpsc};
use gpui::{prelude::*, *};
use gpui_component::Root;
use pebble_ui::{
    components::{
        Conversation, MicIcon, PanelSurface, RecordingDot, Spinner, ToolStatus, Transcript, Turn,
    },
    panel,
    text::measure_body_height,
    theme,
};
use std::{path::PathBuf, sync::OnceLock};
use tokio::sync::oneshot;

enum Message {
    Ui(u64, UiEvent),
    Menu(i32),
}
static EVENTS: OnceLock<mpsc::UnboundedSender<Message>> = OnceLock::new();
extern "C" fn menu_action(action: i32) {
    if let Some(tx) = EVENTS.get() {
        let _ = tx.unbounded_send(Message::Menu(action));
    }
}
const MENU: &[panel::MenuItem] = &[
    panel::MenuItem {
        title: "Pebble Jev · 準備中",
        tag: 0,
        key: "",
    },
    panel::MenuItem {
        title: "設定ファイルを開く…",
        tag: 6,
        key: ",",
    },
    panel::MenuItem {
        title: "リロード",
        tag: 2,
        key: "",
    },
    panel::MenuItem {
        title: "終了",
        tag: 3,
        key: "",
    },
];
actions!(pebble_jev, [Dismiss, Quit]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Recording(u64),
    Thinking,
    Speaking,
}

struct Session {
    generation: u64,
    /// Dropping the sender stops the session thread.
    _stop: oneshot::Sender<()>,
}

struct Jev {
    focus: FocusHandle,
    scroll: ScrollHandle,
    entries: Vec<Entry>,
    phase: Phase,
    status: String,
    error: Option<String>,
    shown: bool,
    panel_height: f32,
    panel_circular: bool,
    session: Option<Session>,
    verbose: bool,
}
impl Jev {
    fn start(&mut self, cx: &mut Context<Self>) {
        let generation = self.session.as_ref().map_or(1, |s| s.generation + 1);
        self.session.take();
        self.entries.clear();
        self.error = None;
        self.phase = Phase::Idle;
        let (stop_tx, stop) = oneshot::channel();
        let settings = match Settings::load() {
            Ok(settings) => settings,
            Err(error) => {
                self.error = Some(format!("{error:#}"));
                cx.notify();
                return;
            }
        };
        let verbose = self.verbose;
        let events = EVENTS.get().cloned().expect("event channel");
        let sink = std::sync::Arc::new(move |event: UiEvent| {
            let _ = events.unbounded_send(Message::Ui(generation, event));
        });
        std::thread::spawn(move || {
            let runtime = match tokio::runtime::Runtime::new() {
                Ok(runtime) => runtime,
                Err(error) => {
                    sink(UiEvent::Error(format!("{error:#}")));
                    return;
                }
            };
            let ui: session::UiSink = Box::new({
                let sink = sink.clone();
                move |event| sink(event)
            });
            let result = runtime.block_on(session::run(SessionOptions {
                settings,
                verbose,
                ui,
                stop,
            }));
            if let Err(error) = result {
                sink(UiEvent::Error(format!("{error:#}")));
            }
        });
        self.session = Some(Session {
            generation,
            _stop: stop_tx,
        });
        cx.notify();
    }
    fn accepts(&self, generation: u64) -> bool {
        self.session
            .as_ref()
            .is_some_and(|s| s.generation == generation)
    }
    fn ui_event(&mut self, event: UiEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            UiEvent::Status(status) => {
                self.status = status;
                panel::status(&format!("Pebble Jev · {}", self.status));
            }
            UiEvent::Recording(session) => {
                self.error = None;
                self.phase = Phase::Recording(session);
                panel::audio_state(session);
            }
            UiEvent::Level { session, level } => {
                if self.phase == Phase::Recording(session) {
                    panel::audio_level(session, level);
                }
            }
            UiEvent::Thinking => self.phase = Phase::Thinking,
            UiEvent::Speaking => self.phase = Phase::Speaking,
            UiEvent::Idle => self.phase = Phase::Idle,
            UiEvent::Discarded => {
                self.status = "短すぎる録音は送信しません".into();
                panel::status(&format!("Pebble Jev · {}", self.status));
            }
            UiEvent::Conversation(entries) => {
                self.entries = entries;
                self.scroll.scroll_to_bottom();
            }
            UiEvent::Error(error) => {
                self.phase = Phase::Idle;
                self.error = Some(error);
            }
        }
        self.update_panel(window, cx);
    }
    fn body_text(&self) -> String {
        if let Some(error) = &self.error {
            return error.clone();
        }
        if self.entries.is_empty() {
            return "話してください…".into();
        }
        self.entries
            .iter()
            .map(|entry| match entry {
                Entry::User { text, .. } | Entry::Assistant { text, .. } => text.clone(),
                Entry::ToolCall {
                    name, arguments, ..
                } => format!("{name} {arguments}\n·"),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    fn circular(&self) -> bool {
        self.error.is_none() && self.entries.is_empty()
    }
    fn visible(&self) -> bool {
        self.error.is_some() || !self.entries.is_empty() || self.phase != Phase::Idle
    }
    fn update_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let circular = self.circular();
        let body_height =
            measure_body_height(window, &self.body_text(), 22.5, 30., 487., 30., 300.);
        let height = if circular { 96. } else { body_height + 44. };
        if self.panel_height != height || self.panel_circular != circular {
            self.panel_height = height;
            self.panel_circular = circular;
            panel::resize(if circular { 96. } else { 580. }, height as f64, circular);
        }
        if !matches!(self.phase, Phase::Recording(_)) {
            panel::audio_state(0);
        }
        let visible = self.visible();
        if visible && (!self.shown || !panel::visible()) {
            let pid = panel::frontmost_pid();
            cx.on_next_frame(window, move |this, window, _| {
                if this.visible() {
                    panel::show(pid);
                    window.focus(&this.focus);
                }
            });
            panel::request_frame();
        } else if !visible && self.shown {
            panel::hide();
        }
        self.shown = visible;
        cx.notify();
    }
    fn dismiss(&mut self, _: &Dismiss, window: &mut Window, cx: &mut Context<Self>) {
        self.entries.clear();
        self.error = None;
        if !matches!(self.phase, Phase::Recording(_)) {
            self.phase = Phase::Idle;
        }
        self.update_panel(window, cx);
    }
    fn open_settings(&mut self) {
        match Settings::ensure_saved() {
            Ok(path) => {
                let _ = std::process::Command::new("open")
                    .arg("-t")
                    .arg(path)
                    .spawn();
            }
            Err(error) => self.error = Some(format!("{error:#}")),
        }
    }
}
fn turns(entries: &[Entry]) -> Vec<Turn> {
    entries
        .iter()
        .map(|entry| match entry {
            Entry::User { text, done, .. } => Turn::User {
                text: text.clone().into(),
                streaming: !done,
            },
            Entry::Assistant { text, done, .. } => Turn::Assistant {
                text: text.clone().into(),
                streaming: !done,
            },
            Entry::ToolCall {
                name,
                arguments,
                result,
                failed,
                ..
            } => Turn::ToolCall {
                name: name.clone().into(),
                arguments: arguments.clone().into(),
                result: result.clone().map(Into::into),
                status: if *failed {
                    ToolStatus::Failed
                } else if result.is_some() {
                    ToolStatus::Done
                } else {
                    ToolStatus::Running
                },
            },
        })
        .collect()
}
impl Render for Jev {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let circular = self.circular();
        let body_height =
            measure_body_height(window, &self.body_text(), 22.5, 30., 487., 30., 300.);
        let reduce_motion = panel::reduce_motion();
        let mut surface = PanelSurface::new(circular);
        surface = match self.phase {
            Phase::Recording(_) if circular => surface.indicator(MicIcon::new(reduce_motion)),
            Phase::Recording(_) => surface.indicator(RecordingDot::new(reduce_motion)),
            Phase::Thinking => surface.indicator(Spinner::new(reduce_motion)),
            Phase::Speaking | Phase::Idle => surface,
        };
        if !circular {
            surface = match &self.error {
                Some(error) => surface.body(Transcript::new(error.clone(), body_height)),
                None => surface.body(
                    Conversation::new(turns(&self.entries), body_height)
                        .scroll(self.scroll.clone()),
                ),
            };
        }
        div()
            .id("jev")
            .track_focus(&self.focus)
            .key_context("Jev")
            .on_action(cx.listener(Self::dismiss))
            .size_full()
            .child(surface)
    }
}

pub fn run(verbose: bool, _log: Option<PathBuf>) -> anyhow::Result<()> {
    let (tx, mut events) = mpsc::unbounded();
    let _ = EVENTS.set(tx);
    Application::new().run(move |cx: &mut App| {
        theme::init(cx);
        cx.bind_keys([
            KeyBinding::new("escape", Dismiss, Some("Jev")),
            KeyBinding::new("cmd-q", Quit, None),
        ]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        let bounds = Bounds::centered(None, size(px(580.), px(58.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: None,
                kind: WindowKind::Normal,
                focus: false,
                show: false,
                is_resizable: false,
                is_minimizable: false,
                window_background: WindowBackgroundAppearance::Transparent,
                ..Default::default()
            },
            |window, cx| {
                panel::setup(window, MENU, menu_action);
                let view = cx.new(|cx| {
                    let mut view = Jev {
                        focus: cx.focus_handle(),
                        scroll: ScrollHandle::new(),
                        entries: vec![],
                        phase: Phase::Idle,
                        status: "準備中".into(),
                        error: None,
                        shown: false,
                        panel_height: 0.,
                        panel_circular: false,
                        session: None,
                        verbose,
                    };
                    view.start(cx);
                    cx.spawn_in(window, async move |this, cx| {
                        while let Some(message) = events.next().await {
                            let result = this.update_in(cx, |this, window, cx| match message {
                                Message::Ui(generation, event) if this.accepts(generation) => {
                                    this.ui_event(event, window, cx)
                                }
                                Message::Ui(..) => {}
                                Message::Menu(2) => this.start(cx),
                                Message::Menu(3) => cx.quit(),
                                Message::Menu(5) => {
                                    if this.phase == Phase::Idle {
                                        this.dismiss(&Dismiss, window, cx);
                                    }
                                }
                                Message::Menu(6) => this.open_settings(),
                                Message::Menu(_) => {}
                            });
                            if result.is_err() {
                                break;
                            }
                        }
                    })
                    .detach();
                    view
                });
                cx.new(|cx| Root::new(view, window, cx))
            },
        )
        .expect("Create floating panel");
    });
    Ok(())
}
