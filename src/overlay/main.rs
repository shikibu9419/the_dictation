mod model;
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
    fn index_panel_hide();
    fn index_panel_resize(height: f64);
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
                        Some("paste_result" | "paste_permission")
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
    settings_window: Option<WindowHandle<settings_view::SettingsView>>,
    _subscriptions: Vec<Subscription>,
}
impl Overlay {
    fn update_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let editable = self
            .model
            .visible()
            .filter(|i| i.phase == Phase::Ready)
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
        let visible =
            !self.pasting && (self.model.visible().is_some() || self.model.error.is_some());
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
                index_panel_show(pid);
                window.focus(&self.focus);
            } else if !visible && self.shown {
                index_panel_hide();
            }
        }
        self.shown = visible;
        self.shown_item = item_id;
        cx.notify();
    }
    fn event(&mut self, event: Event, window: &mut Window, cx: &mut Context<Self>) {
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
        self.update_panel(window, cx);
    }
    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
impl Render for Overlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let item = self.model.visible();
        let phase = item.map(|i| i.phase);
        let recording = phase == Some(Phase::Recording);
        let busy = matches!(phase, Some(Phase::Receiving | Phase::Finalizing));
        let editable = phase == Some(Phase::Ready) && self.model.error.is_none();
        let text = self
            .model
            .error
            .clone()
            .or_else(|| item.filter(|i| !i.text.is_empty()).map(|i| i.text.clone()))
            .unwrap_or_else(|| {
                match phase {
                    Some(Phase::Ready) => "音声を認識できませんでした",
                    Some(Phase::Receiving) => "録音を受信中…",
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
        let height = body_height + 44.;
        if self.panel_height != height {
            self.panel_height = height;
            unsafe {
                index_panel_resize(height as f64);
            }
        }
        div()
            .id("dictation")
            .track_focus(&self.focus)
            .key_context("Dictation")
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::dismiss))
            .size_full()
            .p(px(10.))
            .font_family(".AppleSystemUIFont")
            .text_size(px(22.5))
            .text_color(rgb(0xf3f6fa))
            .child(
                div()
                    .size_full()
                    .flex()
                    .items_start()
                    .py(px(12.))
                    .px(px(16.))
                    .gap(px(10.))
                    .rounded(px(18.))
                    .bg(rgba(0x48484818))
                    .child(
                        div()
                            .flex_none()
                            .w(px(21.))
                            .h(px(30.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(recording, |d| {
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
                    .child(
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
                    ),
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
        let _handle = cx.open_window(WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), titlebar: None, kind: WindowKind::PopUp,
            focus: false, show: false, is_resizable: false, is_minimizable: false, window_background: WindowBackgroundAppearance::Transparent, ..Default::default() }, |window, cx| {
            if let RawWindowHandle::AppKit(handle) = HasWindowHandle::window_handle(window).unwrap().as_raw() { unsafe { index_panel_setup(handle.ns_view.as_ptr(), menu_action);  } }
            let input = cx.new(|cx| InputState::new(window, cx).multi_line(true).rows(1));
            let view = cx.new(|cx| {
                let subscription = cx.subscribe_in(&input, window, |this: &mut Overlay, input, event, _, cx| {
                    if matches!(event, InputEvent::Change | InputEvent::PressEnter { .. }) {
                        if let Some(id) = this.editing { this.model.edit(id, input.read(cx).value().to_string()); }
                        cx.notify();
                    }
                });
                let mut view = Overlay { model: Model::default(), focus: cx.focus_handle(), backend: None, backend_path, verbose, shown: false, shown_item: None, pasting: false, log, scroll: ScrollHandle::new(), input, editing: None, panel_height: 0., settings_window: None, _subscriptions: vec![subscription] };
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
                                if event["type"] == "paste_result" {
                                    this.pasting = false;
                                    this.update_panel(window, cx);
                                }
                                if event["success"] == false || event["allowed"] == false {
                                    let reason = event["text"].as_str().unwrap_or("自動貼り付けできませんでした");
                                    eprintln!("[GUI] {reason}");
                                    if let Ok(title) = CString::new(reason) { unsafe { index_status(title.as_ptr()); } }
                                }
                            },
                            Message::Menu(3) => cx.quit(),
                            Message::Menu(4) => { if this.editing.is_some() { this.input.update(cx, |input, cx| input.focus(window, cx)); } else { window.focus(&this.focus); } cx.notify(); },
                            Message::Menu(6) => {
                                let existing = this.settings_window.is_some_and(|handle| handle.update(cx, |_, window, _| window.activate_window()).is_ok());
                                if !existing { match settings_view::open(this.backend_path.clone(),cx.entity().downgrade(),cx) { Ok(handle)=>this.settings_window=Some(handle),Err(e)=>eprintln!("Settings: {e:#}") } }
                            },
                            Message::Menu(2 | 7) => {
                                this.backend.take(); this.pasting=false; this.model=Model::default(); this.update_panel(window,cx); this.start(window,cx);
                            },
                            Message::Menu(5) => {
                                // Outside clicks may change the user's focus, but must not
                                // dismiss a recording whose result is still arriving.
                                if !this.model.visible().is_some_and(|item| matches!(item.phase, Phase::Recording | Phase::Receiving | Phase::Finalizing)) {
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
