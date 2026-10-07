use crate::{
    EVENTS, Message,
    settings::{InputSource, Settings, SpeechModel},
};
use gpui::{prelude::*, *};
use std::path::PathBuf;

pub struct SettingsView {
    settings: Settings,
    owner: WeakEntity<crate::Overlay>,
    backend: PathBuf,
    error: Option<String>,
    downloading: bool,
}
impl SettingsView {
    fn choose_input(&mut self, input: InputSource, cx: &mut Context<Self>) {
        self.settings.input = input;
        self.error = None;
        cx.notify();
    }
    fn choose_speech(&mut self, speech: SpeechModel, cx: &mut Context<Self>) {
        self.settings.speech = speech;
        self.error = None;
        cx.notify();
    }
    fn download(&mut self, cx: &mut Context<Self>) {
        if self.downloading {
            return;
        }
        self.downloading = true;
        self.error = None;
        let backend = self.backend.clone();
        let setup_command = "setup-qwen";
        cx.spawn(async move |this, cx| {
            let (send, receive) = futures::channel::oneshot::channel();
            std::thread::spawn(move || {
                let result = std::process::Command::new(backend)
                    .arg(setup_command)
                    .output()
                    .map_err(|e| e.to_string())
                    .and_then(|o| {
                        if o.status.success() {
                            Ok(())
                        } else {
                            Err(String::from_utf8_lossy(&o.stderr)
                                .chars()
                                .rev()
                                .take(1000)
                                .collect::<String>()
                                .chars()
                                .rev()
                                .collect())
                        }
                    });
                let _ = send.send(result);
            });
            let result = receive.await.unwrap_or_else(|e| Err(e.to_string()));
            let _ = this.update(cx, |this, cx| {
                this.downloading = false;
                this.error = result.err();
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.downloading {
            return;
        }
        if self.owner.upgrade().is_some_and(|owner| {
            let overlay = owner.read(cx);
            overlay.pasting || overlay.model.has_inflight() || overlay.model.visible().is_some()
        }) {
            self.error =
                Some("録音と認識を終え、結果を貼り付けるか閉じてから切り替えてください。".into());
            cx.notify();
            return;
        }
        match self.settings.save() {
            Ok(()) => {
                if let Some(tx) = EVENTS.get() {
                    let _ = tx.unbounded_send(Message::Menu(7));
                }
                window.remove_window();
            }
            Err(e) => {
                self.error = Some(format!("{e:#}"));
                cx.notify();
            }
        }
    }
}
fn choice(id: &'static str, label: &'static str, selected: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex_1()
        .px(px(14.))
        .py(px(12.))
        .rounded(px(10.))
        .cursor_pointer()
        .bg(if selected {
            rgb(0x484848)
        } else {
            rgb(0x262626)
        })
        .text_color(if selected {
            rgb(0xf4f4f4)
        } else {
            rgb(0xa2a2a2)
        })
        .child(label)
}
impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(rgb(0x1b1b1b))
            .text_color(rgb(0xeeeeee))
            .font_family(".AppleSystemUIFont")
            .text_size(px(14.))
            .p(px(28.))
            .flex()
            .flex_col()
            .gap(px(18.))
            .child(div().text_size(px(24.)).child("音声入力"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child("入力デバイス")
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .child(
                                choice(
                                    "input-index",
                                    "Pebble Index",
                                    self.settings.input == InputSource::Index,
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| this.choose_input(InputSource::Index, cx),
                                )),
                            )
                            .child(
                                choice(
                                    "input-mic",
                                    "PCマイク · 右Option",
                                    self.settings.input == InputSource::Microphone,
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| this.choose_input(InputSource::Microphone, cx),
                                )),
                            ),
                    )
                    .child(
                        div()
                            .text_color(rgb(0xa2a2a2))
                            .text_size(px(12.))
                            .child("PCマイクは右Optionを押している間に録音します。"),
                    ),
            )
            .child(
                div().flex().flex_col().gap(px(8.)).child("音声認識").child(
                    div()
                        .flex()
                        .gap(px(8.))
                        .child(
                            choice(
                                "engine-apple",
                                "SpeechAnalyzer",
                                self.settings.speech == SpeechModel::Apple,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.choose_speech(SpeechModel::Apple, cx)
                            })),
                        )
                        .child(
                            choice(
                                "engine-on-device",
                                "On Device",
                                self.settings.speech == SpeechModel::OnDevice,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.choose_speech(SpeechModel::OnDevice, cx)
                            })),
                        ),
                ),
            )
            .when(self.settings.speech == SpeechModel::OnDevice, |d| {
                d.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(rgb(0xa2a2a2))
                                .child("Qwen3-ASR 1.7B · MLX。初回に約2.2 GBを取得します。"),
                        )
                        .child(
                            div()
                                .id("download-model")
                                .cursor_pointer()
                                .py(px(8.))
                                .child(if self.downloading {
                                    "ダウンロード中…"
                                } else if Settings::qwen_ready() {
                                    "モデルを再確認"
                                } else {
                                    "モデルをダウンロード"
                                })
                                .on_click(cx.listener(|this, _, _, cx| this.download(cx))),
                        ),
                )
            })
            .when_some(self.error.clone(), |d, error| {
                d.child(
                    div()
                        .text_size(px(12.))
                        .text_color(rgb(0xdddddd))
                        .child(error),
                )
            })
            .child(div().flex_1())
            .child(
                div()
                    .id("save-settings")
                    .cursor_pointer()
                    .rounded(px(10.))
                    .px(px(16.))
                    .py(px(12.))
                    .bg(rgb(0x484848))
                    .child("保存して切り替える")
                    .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
            )
    }
}
pub fn open(
    backend: PathBuf,
    owner: WeakEntity<crate::Overlay>,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<SettingsView>> {
    let loaded = Settings::load();
    let (settings, error) = match loaded {
        Ok(s) => (s, None),
        Err(e) => (Settings::default(), Some(format!("{e:#}"))),
    };
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(520.), px(460.)),
                cx,
            ))),
            titlebar: Some(TitlebarOptions {
                title: Some("Index Voice · 設定".into()),
                ..Default::default()
            }),
            is_resizable: false,
            ..Default::default()
        },
        |_, cx| {
            cx.new(|_| SettingsView {
                settings,
                owner,
                backend,
                error,
                downloading: false,
            })
        },
    )
}
