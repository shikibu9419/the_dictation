mod model;
mod presentation;
#[path = "../qwen_runtime.rs"]
mod qwen_runtime;
#[path = "../settings.rs"]
mod settings;
mod settings_view;

use futures::{StreamExt, channel::mpsc};
use gpui::{prelude::*, *};
use gpui_component::{
    Root, Theme, ThemeMode,
    input::{Input, InputEvent, InputState, Position},
};
use model::{Event, Model, Phase};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{
    ffi::{CString, c_void},
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::OnceLock,
    time::Duration,
};

unsafe extern "C" {
    fn index_panel_setup(view: *mut c_void, callback: extern "C" fn(i32));
    fn index_frontmost_pid() -> i32;
    fn index_panel_show(target: i32);
    fn index_panel_request_frame();
    fn index_panel_editing(editing: bool);
    fn index_panel_hide();
    fn index_panel_audio(level: f64, active: bool);
    fn index_panel_resize(width: f64, height: f64, circular: bool);
    fn index_reduce_motion() -> bool;
    fn index_panel_visible() -> bool;
    fn index_status(text: *const std::ffi::c_char);
    fn index_permission();
}
enum Message {
    Event(Event, Option<u64>),
    Menu(i32),
    Control(serde_json::Value, Option<u64>),
    End(Option<u64>),
}
fn accepts_generation(active: Option<u64>, incoming: Option<u64>) -> bool {
    active == incoming
}

static EVENTS: OnceLock<mpsc::UnboundedSender<Message>> = OnceLock::new();
extern "C" fn menu_action(action: i32) {
    if let Some(tx) = EVENTS.get() {
        let _ = tx.unbounded_send(Message::Menu(action));
    }
}
actions!(index_voice, [Paste, Dismiss, Quit]);

fn history_path() -> std::path::PathBuf {
    settings::Settings::path().with_file_name("history.json")
}
fn load_model() -> Model {
    let mut model = Model::default();
    match std::fs::read(history_path()) {
        Ok(bytes) => match serde_json::from_slice::<Vec<String>>(&bytes) {
            Ok(texts) => model.restore_history(texts),
            Err(error) => eprintln!("[GUI] Read history: {error}"),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(error) => eprintln!("[GUI] Read history: {error}"),
    }
    model
}

struct Backend {
    generation: u64,
    child: Child,
    input: Option<ChildStdin>,
}
impl Backend {
    fn send(&mut self, message: serde_json::Value) -> anyhow::Result<()> {
        let input = self
            .input
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Backend input closed"))?;
        writeln!(input, "{message}")?;
        input.flush()?;
        Ok(())
    }

    fn start(
        path: &std::path::Path,
        verbose: bool,
        log: Option<&std::path::Path>,
    ) -> anyhow::Result<Self> {
        let mut command = Command::new(path);
        if let Some(source) = std::env::var_os("INDEX_VOICE_INPUT_COMMAND") {
            command
                .args(["stream", "--gui-events", "--input-command"])
                .arg(source);
        } else if settings::Settings::load()?.input == settings::InputSource::Microphone {
            command.args(["microphone", "--gui-events"]);
        } else {
            command.args(["listen", "--gui-events", "--interval", "0.1"]);
        }
        if verbose {
            command.arg("-v");
        }
        if let Some(log) = log {
            command.arg("--log").arg(log);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let output = child.stdout.take().unwrap();
        static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let generation = GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::thread::spawn(move || read_events(BufReader::new(output), Some(generation)));
        let input = child.stdin.take();
        Ok(Self {
            child,
            input,
            generation,
        })
    }
}
impl Drop for Backend {
    fn drop(&mut self) {
        self.input.take();
        if self.child.try_wait().ok().flatten().is_none() {
            unsafe {
                libc::kill(self.child.id() as i32, libc::SIGTERM);
            }
            for _ in 0..100 {
                if self.child.try_wait().ok().flatten().is_some() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
fn read_events(reader: impl BufRead, generation: Option<u64>) {
    let tx = EVENTS.get().unwrap();
    for line in reader.lines() {
        let event = match line {
            Ok(line) => {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line)
                    && matches!(
                        value["type"].as_str(),
                        Some("paste_result" | "copy_result" | "paste_permission" | "audio_level" | "gesture" | "gesture_paste_result")
                    )
                {
                    if tx
                        .unbounded_send(Message::Control(value, generation))
                        .is_err()
                    {
                        return;
                    }
                    continue;
                }
                serde_json::from_str(&line).unwrap_or_else(|e| {
                    Event::error(format!("受信データを読み取れませんでした: {e}"))
                })
            }
            Err(e) => Event::error(format!("受信が終了しました: {e}")),
        };
        if tx
            .unbounded_send(Message::Event(event, generation))
            .is_err()
        {
            return;
        }
    }
    let _ = tx.unbounded_send(Message::End(generation));
}
struct Overlay {
    model: Model,
    focus: FocusHandle,
    backend: Option<Backend>,
    backend_path: std::path::PathBuf,
    verbose: bool,
    shown: bool,
    shown_item: Option<u64>,
    pasting: bool,
    log: Option<std::path::PathBuf>,
    scroll: ScrollHandle,
    input: Entity<InputState>,
    editing: Option<u64>,
    panel_height: f32,
    panel_circular: bool,
    presentation_revision: u64,
    presentation: settings::Presentation,
    copied: std::collections::HashSet<String>,
    gestures: settings::GestureBindings,
    gesture_request: u64,
    settings_window: Option<WindowHandle<settings_view::SettingsView>>,
    _subscriptions: Vec<Subscription>,
}
impl Overlay {
    fn save_history(&self) {
        let result = (|| -> anyhow::Result<()> {
            let path = history_path();
            let dir = path.parent().unwrap();
            std::fs::create_dir_all(dir)?;
            let mut file = tempfile::NamedTempFile::new_in(dir)?;
            serde_json::to_writer(&mut file, &self.model.history())?;
            file.flush()?;
            file.persist(path)?;
            Ok(())
        })();
        if let Err(error) = result { eprintln!("[GUI] Save history: {error:#}"); }
    }
    fn history_move(&mut self, older: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.pasting || self.editing.is_none() { return; }
        let before = self.input.update(cx, |input, cx| {
            if input.marked_text_range(window, cx).is_some()
                || input.selected_text_range(false, window, cx).is_some_and(|s| !s.range.is_empty()) {
                return None;
            }
            Some(input.cursor())
        });
        let Some(before) = before else { return };
        let editing = self.editing;
        // Let the editor perform its normal visual-line movement, then browse
        // only if the caret could not move. This also handles wrapped lines.
        cx.defer_in(window, move |this, window, cx| {
            if !this.pasting && this.editing == editing
                && this.input.read(cx).cursor() == before
                && this.model.browse(older, unsafe { index_frontmost_pid() }) {
                this.update_panel(window, cx);
            }
        });
    }

    fn panel_layout(&self, window: &mut Window) -> (bool, String, f32, f32) {
        let item = self.model.visible();
        let phase = item.map(|i| i.phase);
        let circular = presentation::surface(self.presentation, phase, self.model.error.is_some(), self.model.history_view) != presentation::Surface::Transcript;
        let text = self
            .model
            .error
            .clone()
            .or_else(|| item.filter(|i| !i.text.is_empty()).map(|i| i.text.clone()))
            .unwrap_or_else(|| {
                match phase {
                    Some(Phase::Ready) => "音声を認識できませんでした",
                    Some(Phase::Receiving) => "録音を受信中…",
                    Some(Phase::Reconnecting) => "通信切断・待機中…",
                    Some(Phase::Finalizing) => "全文を文字起こし中…",
                    _ => "話してください…",
                }
                .into()
            });
        // The editor and live label use the same width and line metrics.
        let measured = window
            .text_system()
            .shape_text(
                text.clone().into(),
                px(22.5),
                &[TextRun {
                    len: text.len(),
                    font: font(".AppleSystemUIFont"),
                    color: rgb(0xf3f6fa).into(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }],
                Some(px(487.)),
                None,
            )
            .map(|lines| {
                lines
                    .iter()
                    .map(|line| f32::from(line.size(px(30.)).height))
                    .sum::<f32>()
            })
            .unwrap_or(30.);
        let body_height = measured.clamp(30., 300.);
        let height = if circular { 96. } else { body_height + 44. };
        (circular, text, body_height, height)
    }
    fn update_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.presentation_revision = self.presentation_revision.wrapping_add(1);
        let (circular, _, _, height) = self.panel_layout(window);
        if self.panel_height != height || self.panel_circular != circular {
            self.panel_height = height;
            self.panel_circular = circular;
            unsafe { index_panel_resize(if circular { 96. } else { 580. }, height as f64, circular); }
        }

        if self.presentation.live_mode || !self.model.visible().is_some_and(|item| item.phase == Phase::Recording) {
            unsafe { index_panel_audio(0., false); }
        }
        let was_editing = self.editing;
        let editable = self
            .model
            .visible()
            .filter(|i| i.phase == Phase::Ready && (self.presentation.live_mode || self.model.history_view))
            .map(|i| (i.id, i.text.clone()));
        if let Some((id, text)) = editable {
            if self.editing != Some(id) {
                self.editing = Some(id);
                self.input.update(cx, |input, cx| {
                    let last_line = text.rsplit('\n').next().unwrap_or("");
                    let end = Position::new(
                        text.bytes().filter(|c| *c == b'\n').count() as u32,
                        last_line.len() as u32,
                    );
                    input.set_value(text, window, cx);
                    input.set_cursor_position(end, window, cx);
                    input.focus(window, cx);
                });
            }
        } else {
            self.editing = None;
        }
        if self.editing != was_editing {
            unsafe { index_panel_editing(self.editing.is_some()); }
        }
        let visible =
            !self.pasting && presentation::surface(self.presentation, self.model.visible().map(|i| i.phase), self.model.error.is_some(), self.model.history_view) != presentation::Surface::Hidden;
        let item_id = self.model.visible().map(|item| item.id);
        unsafe {
            let title = if self.model.error.is_some() {
                "Index Voice · 接続エラー"
            } else if self.model.ready {
                if settings::Settings::load()
                    .is_ok_and(|s| s.input == settings::InputSource::Microphone)
                {
                    "Index Voice · 右Optionで録音"
                } else {
                    "Index Voice · リング待機中"
                }
            } else {
                "Index Voice · 準備中"
            };
            index_status(CString::new(title).unwrap().as_ptr());
            if visible && (!self.shown || self.shown_item != item_id || !index_panel_visible()) {
                let pid = self
                    .model
                    .visible()
                    .map_or(index_frontmost_pid(), |i| i.target);
                if self.verbose {
                    eprintln!("[GUI] show item={item_id:?} target_pid={pid}");
                }
                if let (Some(backend), Some(id)) = (&mut self.backend, item_id)
                    && let Err(error) = backend.send(
                        serde_json::json!({"type":"capture_target","request":id,"target":pid}),
                    )
                {
                    eprintln!("[GUI] Capture paste target: {error:#}");
                }
                // Update has queued geometry; render must complete before showing.
                // Only then may a still-current presentation become visible.
                let revision = self.presentation_revision;
                cx.on_next_frame(window, move |this, window, cx| {
                    if this.presentation_revision != revision || this.pasting || !this.shown {
                        return;
                    }
                    index_panel_show(pid);
                    if this.editing.is_some() {
                        this.input.update(cx, |input, cx| input.focus(window, cx));
                    } else {
                        window.focus(&this.focus);
                    }
                });
                // A hidden macOS window has stopped its display link.
                // Force a frame after geometry, without making it visible.
                index_panel_request_frame();
            } else if !visible && self.shown {
                index_panel_hide();
            }
        }
        self.shown = visible;
        self.shown_item = item_id;
        cx.notify();
    }
    fn event(&mut self, event: Event, window: &mut Window, cx: &mut Context<Self>) {
        let before = self.model.visible().map(|i| (i.id, i.recording.clone(), i.phase));
        let cause = format!("type={} recording={:?} collecting={:?} mode={:?} final={:?} empty={}", event.r#type, event.recording, event.collecting, event.mode, event.r#final, event.empty);
        let completion_key = event.recording.clone();
        let completion_text = event.text.clone();
        let completed = event.r#type == "text" && event.mode.as_deref() == Some("batch") && event.r#final == Some(true);
        if event.r#type == "error" {
            self.pasting = false;
        }
        if event.r#type == "text" {
            self.scroll.scroll_to_bottom();
        }
        let target = unsafe { index_frontmost_pid() };
        if self.verbose {
            eprintln!(
                "[GUI {}] event={} recording={:?} target_pid={target}",
                chrono::Local::now().format("%H:%M:%S%.3f"),
                event.r#type,
                event.recording
            );
        }
        self.model.accept(event, target);
        let after = self.model.visible().map(|i| (i.id, i.recording.clone(), i.phase));
        if before != after || cause.ends_with("empty=true") {
            let line = format!("[{}] GUI transition: {before:?} -> {after:?}; {cause}\n", chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f"));
            if self.verbose { eprint!("{line}"); }
            if let Some(path) = &self.log {
                if let Err(error) = std::fs::OpenOptions::new().create(true).append(true).open(path).and_then(|mut file| file.write_all(line.as_bytes())) {
                    eprintln!("[GUI] Write transition log: {error}");
                }
            }
        }
        if completed {
            self.save_history();
            self.copied.retain(|key| self.model.items.iter().any(|item| item.recording.as_ref() == Some(key)));
            if let Some(key) = completion_key {
                let id = self.model.items.iter().find(|i| i.recording.as_ref() == Some(&key)).map(|i| i.id);
                if let Some(id) = id {
                    if self.copied.insert(key) {
                        let request = serde_json::json!({"type":"copy", "request":id, "text":completion_text.unwrap_or_default()});
                        if let Some(backend) = &mut self.backend {
                            if let Err(error) = backend.send(request) {
                                self.model.error = Some(format!("コピー要求を送信できませんでした: {error}"));
                            }
                        }
                    }
                    if !self.presentation.live_mode || !self.presentation.final_text || self.model.items.iter().any(|i| i.id == id && i.text.trim().is_empty()) { self.model.dismiss_id(id); }
                }
            }
        }
        self.update_panel(window, cx);
    }
    fn gesture(&mut self, event: &serde_json::Value, window: &mut Window, cx: &mut Context<Self>) {
        let Some(action) = event["gesture"].as_str().and_then(|name| self.gestures.action(name)) else { return; };
        let target = unsafe { index_frontmost_pid() };
        match action {
            settings::GestureAction::History => {
                self.model.open_history(target);
                self.update_panel(window, cx);
            }
            settings::GestureAction::Paste => {
                self.gesture_request += 1;
                if let Some(backend) = &mut self.backend {
                    if let Err(error) = backend.send(serde_json::json!({"type":"paste_current",
                        "request":self.gesture_request, "target":target})) {
                        eprintln!("[GUI] Gesture paste failed: {error}");
                    }
                }
            }
        }
    }
    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match settings::Settings::load() {
            Ok(settings) => { self.presentation = settings.presentation; self.gestures = settings.gestures; },
            Err(error) => { self.event(Event::error(format!("設定を読み込めませんでした: {error}")), window, cx); return; }
        }
        self.copied.clear();
        match Backend::start(&self.backend_path, self.verbose, self.log.as_deref()) {
            Ok(backend) => self.backend = Some(backend),
            Err(e) => self.event(
                Event::error(format!("受信を開始できませんでした: {e:#}")),
                window,
                cx,
            ),
        }
    }
    fn dismiss(&mut self, _: &Dismiss, window: &mut Window, cx: &mut Context<Self>) {
        if self.pasting {
            return;
        }
        if self.editing.is_some() && self.input.update(cx, |input, cx| {
            if input.marked_text_range(window, cx).is_some() {
                input.unmark_text(window, cx);
                true
            } else { false }
        }) { return; }
        if let (Some(backend), Some(item)) = (&mut self.backend, self.model.visible()) {
            let _ = backend.send(serde_json::json!({"type":"forget_target","request":item.id}));
        }
        self.model.dismiss();
        self.update_panel(window, cx);
    }
    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if self.verbose {
            eprintln!("[GUI] paste requested; busy={}", self.pasting);
        }
        if self.pasting {
            return;
        }
        if self.editing.is_some()
            && self.input.update(cx, |input, cx| {
                input.marked_text_range(window, cx).is_some()
            })
        {
            return;
        }
        let Some((text, target)) = self.model.paste() else {
            return;
        };
        let id = self.model.visible().unwrap().id;
        let message = serde_json::json!({"type":"paste","request":id,"target":target,"text":text});
        let result = self
            .backend
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Backend unavailable"))
            .and_then(|backend| backend.send(message));
        if let Err(error) = result {
            eprintln!("[GUI] Paste request failed: {error:#}");
            return;
        }
        self.pasting = true;
        self.model.dismiss_id(id);
        self.update_panel(window, cx);
        cx.notify();
    }
}
fn microphone() -> impl IntoElement {
    div().size(px(24.)).rounded_full().with_animation(
        "microphone-glow", Animation::new(Duration::from_millis(1067)).repeat(),
        |d, delta| {
            let alpha = if unsafe { index_reduce_motion() } { 0.35 } else {
                0.12 + 0.28 * (1. - (delta * std::f32::consts::TAU).cos()) / 2.
            };
            d.shadow(vec![BoxShadow { color: rgba(0xff334b00 | (alpha * 255.) as u32).into(),
                offset: point(px(0.), px(0.)), blur_radius: px(7.), spread_radius: px(0.) }])
             .child(canvas(|_, _, _| (), |bounds, _, window, _| {
                let center = bounds.center();
                let mut path = PathBuilder::stroke(px(2.2));
                let p = |x: f32, y: f32| center + point(px(x), px(y));
                path.move_to(p(-4., -7.));
                path.cubic_bezier_to(p(4., -7.), p(-4., -12.), p(4., -12.));
                path.line_to(p(4., 1.));
                path.cubic_bezier_to(p(-4., 1.), p(4., 6.), p(-4., 6.));
                path.close();
                path.move_to(p(-7., -1.));
                path.line_to(p(-7., 1.));
                path.cubic_bezier_to(p(7., 1.), p(-7., 10.), p(7., 10.));
                path.line_to(p(7., -1.));
                path.move_to(p(0., 7.)); path.line_to(p(0., 11.));
                path.move_to(p(-4., 11.)); path.line_to(p(4., 11.));
                if let Ok(path) = path.build() { window.paint_path(path, rgb(0xff334b)); }
             }).size_full())
        },
    )
}

impl Render for Overlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let item = self.model.visible();
        let phase = item.map(|i| i.phase);
        let circular = presentation::surface(self.presentation, phase, self.model.error.is_some(), self.model.history_view) != presentation::Surface::Transcript;
        let recording = phase == Some(Phase::Recording);
        let busy = matches!(phase, Some(Phase::Receiving | Phase::Reconnecting | Phase::Finalizing));
        let editable = phase == Some(Phase::Ready) && self.model.error.is_none();
        let (_, text, body_height, _) = self.panel_layout(window);
        div()
            .id("dictation")
            .track_focus(&self.focus)
            .key_context("Dictation")
            .capture_action(cx.listener(|this, _: &gpui_component::input::MoveUp, window, cx| this.history_move(true, window, cx)))
            .capture_action(cx.listener(|this, _: &gpui_component::input::MoveDown, window, cx| this.history_move(false, window, cx)))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::dismiss))
            .size_full()
            .p(px(if circular { 20. } else { 10. }))
            .font_family(".AppleSystemUIFont")
            .text_size(px(22.5))
            .text_color(rgb(0xf3f6fa))
            .child(
                div()
                    .size_full()
                    .flex()
                    .items_start()
                    .py(px(if circular { 0. } else { 12. }))
                    .px(px(if circular { 0. } else { 16. }))
                    .gap(px(if circular { 0. } else { 10. }))
                    .when(circular, |d| d.items_center().justify_center())
                    .rounded(px(if circular { 28. } else { 18. }))
                    .bg(rgba(0x48484818))
                    .child(
                        div()
                            .flex_none()
                            .w(px(21.))
                            .h(px(30.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(recording && !circular, |d| {
                                d.child(
                                    div()
                                        .size(px(15.))
                                        .rounded_full()
                                        .bg(rgb(0xff334b))
                                        .with_animation(
                                            "recording-glow",
                                            Animation::new(Duration::from_millis(1067)).repeat(),
                                            |d, delta| {
                                                let alpha = if unsafe { index_reduce_motion() } {
                                                    0.65
                                                } else {
                                                    0.12 + 0.53
                                                        * (1.
                                                            - (delta * std::f32::consts::TAU).cos())
                                                        / 2.
                                                };
                                                d.shadow(vec![BoxShadow {
                                                    color: rgba(0xff334b00 | (alpha * 255.) as u32)
                                                        .into(),
                                                    offset: point(px(0.), px(0.)),
                                                    blur_radius: px(4.),
                                                    spread_radius: px(1.5),
                                                }])
                                            },
                                        ),
                                )
                            })
                            .when(recording && circular, |d| d.child(microphone()))
                            .when(busy, |d| {
                                d.child(div().size(px(21.)).with_animation(
                                    "processing",
                                    Animation::new(Duration::from_millis(850)).repeat(),
                                    |d, delta| {
                                        d.child(
                                            canvas(
                                                |_, _, _| (),
                                                move |bounds, _, window, _| {
                                                    for n in 0..48 {
                                                        let mut path = PathBuilder::stroke(px(3.));
                                                        for step in 0..=2 {
                                                            let angle = (delta
                                                                + (n as f32 + step as f32 / 2.)
                                                                    / 56.)
                                                                * std::f32::consts::TAU;
                                                            let p = bounds.center()
                                                                + point(
                                                                    px(angle.cos() * 8.25),
                                                                    px(angle.sin() * 8.25),
                                                                );
                                                            if step == 0 {
                                                                path.move_to(p);
                                                            } else {
                                                                path.line_to(p);
                                                            }
                                                        }
                                                        if let Ok(path) = path.build() {
                                                            let alpha = (20.
                                                                + 235. * (n as f32 / 47.).powf(1.5))
                                                                as u32;
                                                            window.paint_path(
                                                                path,
                                                                rgba(0xe5e5e500 | alpha),
                                                            );
                                                        }
                                                    }
                                                },
                                            )
                                            .size_full(),
                                        )
                                    },
                                ))
                            }),
                    )
                    .when(!circular, |d| d.child(
                        div()
                            .w(px(497.))
                            .flex_none()
                            .flex()
                            .flex_col()
                            .when(editable, |d| {
                                d.child(
                                    Input::new(&self.input)
                                        .appearance(false)
                                        .bordered(false)
                                        .focus_bordered(false)
                                        .h(px(body_height))
                                        .p_0()
                                        .text_size(px(22.5))
                                        .line_height(px(30.)),
                                )
                            })
                            .when(!editable, |d| {
                                d.child(
                                    div()
                                        .id("transcript")
                                        .w(px(487.))
                                        .h(px(body_height))
                                        .overflow_y_scroll()
                                        .track_scroll(&self.scroll)
                                        .line_height(px(30.))
                                        .child(text),
                                )
                            }),
                    )),
            )
    }
}

fn main() -> anyhow::Result<()> {
    struct Logger;
    impl log::Log for Logger {
        fn enabled(&self, _: &log::Metadata<'_>) -> bool {
            true
        }
        fn log(&self, record: &log::Record<'_>) {
            eprintln!("[GUI {}] {}", record.level(), record.args());
        }
        fn flush(&self) {}
    }
    let _ = log::set_logger(&Logger);
    log::set_max_level(log::LevelFilter::Warn);
    let args: Vec<_> = std::env::args().collect();
    let preview = args.iter().any(|a| a == "--preview");
    let open_settings = args.iter().any(|a| a == "--settings");
    let verbose = args.iter().any(|a| a == "--verbose");
    let log = args
        .iter()
        .position(|a| a == "--log")
        .and_then(|i| args.get(i + 1))
        .map(std::path::PathBuf::from);
    let backend_path = args
        .iter()
        .position(|a| a == "--backend")
        .and_then(|i| args.get(i + 1))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            let exe = std::env::current_exe().unwrap();
            let sibling = exe.with_file_name("pebble-index");
            if sibling.exists() {
                sibling
            } else {
                exe.parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join("Helpers/pebble-index")
            }
        });
    let (tx, mut events) = mpsc::unbounded();
    let _ = EVENTS.set(tx);
    Application::new().run(move |cx: &mut App| {
        gpui_component::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        Theme::global_mut(cx).background = transparent_black();
        Theme::global_mut(cx).selection = rgba(0x62ddff40).into();
        Theme::global_mut(cx).caret = rgb(0x8bedff).into();
        cx.bind_keys([KeyBinding::new("ctrl-h", gpui_component::input::Backspace, Some("Input")), KeyBinding::new("ctrl-p", gpui_component::input::MoveUp, Some("Input")), KeyBinding::new("ctrl-n", gpui_component::input::MoveDown, Some("Input")), KeyBinding::new("shift-enter", gpui_component::input::Enter { secondary: true }, Some("Input")), KeyBinding::new("enter", Paste, Some("Input")), KeyBinding::new("escape", Dismiss, Some("Input")), KeyBinding::new("enter", Paste, Some("Dictation")), KeyBinding::new("escape", Dismiss, Some("Dictation")), KeyBinding::new("cmd-q", Quit, None)]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        let bounds = Bounds::centered(None, size(px(580.), px(58.)), cx);
        let mut overlay = None;
        let _handle = cx.open_window(WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), titlebar: None, kind: WindowKind::Normal,
            focus: false, show: false, is_resizable: false, is_minimizable: false, window_background: WindowBackgroundAppearance::Transparent, ..Default::default() }, |window, cx| {
            if let RawWindowHandle::AppKit(handle) = HasWindowHandle::window_handle(window).unwrap().as_raw() { unsafe { index_panel_setup(handle.ns_view.as_ptr(), menu_action);  } }
            let input = cx.new(|cx| InputState::new(window, cx).multi_line(true).rows(1));
            let view = cx.new(|cx| {
                let subscription = cx.subscribe_in(&input, window, |this: &mut Overlay, input, event, window, cx| {
                    if matches!(event, InputEvent::Change | InputEvent::PressEnter { .. }) {
                        if let Some(id) = this.editing { this.model.edit(id, input.read(cx).value().to_string());
                            if !input.update(cx, |input, cx| input.marked_text_range(window, cx).is_some()) { this.save_history(); } }
                        cx.notify();
                    }
                });
                let mut view = Overlay { model: load_model(), focus: cx.focus_handle(), backend: None, backend_path, verbose, shown: false, shown_item: None, pasting: false, log, scroll: ScrollHandle::new(), input, editing: None, panel_height: 0., panel_circular: false, presentation_revision: 0, presentation: settings::Settings::load().map(|s| s.presentation).unwrap_or_default(), copied: Default::default(), gestures: settings::Settings::load().map(|s| s.gestures).unwrap_or_default(), gesture_request: 1_000_000_000, settings_window: None, _subscriptions: vec![subscription] };
                if !preview { view.start(window, cx); }
                cx.spawn_in(window, async move |this, cx| {
                    while let Some(message) = events.next().await {
                        if this.update_in(cx, |this, window, cx| match message {
                            Message::Event(e, generation) if accepts_generation(this.backend.as_ref().map(|b| b.generation), generation) => this.event(e, window, cx),
                            Message::End(generation) if accepts_generation(this.backend.as_ref().map(|b| b.generation), generation) => {
                                this.backend.take();
                                let reason = this.model.error.clone().unwrap_or_else(|| "受信が終了しました。メニューからリロードしてください。".into());
                                this.event(Event::error(reason), window, cx);
                            }
                            Message::Menu(1) => { if let Some(backend) = &mut this.backend { let _ = backend.send(serde_json::json!({"type":"permission"})); } else { unsafe { index_permission(); } } },
                            Message::Control(event, generation) if accepts_generation(this.backend.as_ref().map(|b| b.generation), generation) => {
                                if event["type"] == "gesture" { this.gesture(&event, window, cx); }
                                if event["type"] == "audio_level" {
                                    let active = !this.presentation.live_mode && this.model.visible().is_some_and(|item|
                                        item.phase == Phase::Recording && (item.recording.is_none() || item.recording.as_deref() == event["recording"].as_str()));
                                    unsafe { index_panel_audio(event["level"].as_f64().unwrap_or(0.), active); }
                                }
                                if event["type"] == "paste_result" {
                                    this.pasting = false;
                                    this.update_panel(window, cx);
                                }
                                if event["type"] == "copy_result" && event["success"] == false {
                                    this.model.error = Some(event["text"].as_str().unwrap_or("コピーできませんでした").into());
                                    this.update_panel(window, cx);
                                }
                                if event["success"] == false || event["allowed"] == false {
                                    let reason = event["text"].as_str().unwrap_or("自動貼り付けできませんでした");
                                    eprintln!("[GUI] {reason}");
                                    if let Ok(title) = CString::new(reason) { unsafe { index_status(title.as_ptr()); } }
                                }
                            },
                            Message::Menu(3) => cx.quit(),
                            Message::Menu(8) => { if !this.pasting { this.model.open_history(unsafe { index_frontmost_pid() }); this.update_panel(window,cx); } },
                            Message::Menu(4) => { if this.editing.is_some() { this.input.update(cx, |input, cx| input.focus(window, cx)); } else { window.focus(&this.focus); } cx.notify(); },
                            Message::Menu(6) => {
                                let existing = this.settings_window.is_some_and(|handle| handle.update(cx, |_, window, _| window.activate_window()).is_ok());
                                if !existing { match settings_view::open(this.backend_path.clone(),cx.entity().downgrade(),cx) { Ok(handle)=>this.settings_window=Some(handle),Err(e)=>eprintln!("Settings: {e:#}") } }
                            },
                            Message::Menu(2 | 7) => {
                                this.backend.take(); this.pasting=false; this.model=load_model(); this.update_panel(window,cx); this.start(window,cx);
                            },
                            Message::Menu(5) => {
                                // Outside clicks may change the user's focus, but must not
                                // dismiss a recording whose result is still arriving.
                                if !this.model.visible().is_some_and(|item| matches!(item.phase, Phase::Recording | Phase::Receiving | Phase::Reconnecting | Phase::Finalizing)) {
                                    this.dismiss(&Dismiss, window, cx);
                                }
                            },
                            _ => {},
                        }).is_err() { break; }
                    }
                }).detach();
                view
            });
            overlay = Some(view.clone());
            cx.new(|cx| Root::new(view, window, cx))
        }).expect("Create floating panel");
        cx.on_app_quit(move |cx| { if let Some(view) = &overlay { view.update(cx, |view, _| { view.backend.take(); }); } async {} }).detach();
        if open_settings { menu_action(6); }
        if preview { std::thread::spawn(|| read_events(std::io::stdin().lock(), None)); }
    });
    Ok(())
}

#[cfg(test)]
mod connection_tests {
    use super::accepts_generation;
    #[test]
    fn old_exit_and_text_events_cannot_touch_restarted_backend() {
        assert!(accepts_generation(Some(2), Some(2)));
        assert!(!accepts_generation(Some(2), Some(1)));
        assert!(!accepts_generation(None, Some(1)));
        assert!(!accepts_generation(Some(2), None));
        assert!(accepts_generation(None, None));
    }
}
