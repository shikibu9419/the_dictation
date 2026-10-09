use anyhow::Result;
use std::{
    fs::{File, OpenOptions},
    io::{self, IsTerminal, Write},
    path::Path,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
pub struct Output {
    inner: Arc<Mutex<State>>,
    pub verbose: bool,
    pub events: bool,
    pub log_path: Option<std::path::PathBuf>,
}
struct State {
    file: Option<File>,
    terminal: bool,
    last: String,
    visible: bool,
}
impl Output {
    pub fn new(verbose: bool, log: Option<&Path>) -> Result<Self> {
        Ok(Self {
            verbose: verbose || log.is_some(),
            events: false,
            log_path: log.map(Path::to_path_buf),
            inner: Arc::new(Mutex::new(State {
                file: log
                    .map(|p| OpenOptions::new().create(true).append(true).open(p))
                    .transpose()?,
                terminal: log.is_none() && io::stdout().is_terminal(),
                last: String::new(),
                visible: false,
            })),
        })
    }
    pub fn write(&self, text: &str, stderr: bool) {
        let mut state = self.inner.lock().unwrap();
        let mut stream: Box<dyn Write> = if stderr {
            Box::new(io::stderr())
        } else {
            Box::new(io::stdout())
        };
        let _ = stream.write_all(text.as_bytes());
        let _ = stream.flush();
        if let Some(file) = &mut state.file {
            let _ = file.write_all(text.as_bytes());
            let _ = file.flush();
        }
    }
    pub fn line(&self, text: impl AsRef<str>) {
        self.write(&format!("{}\n", text.as_ref()), self.events);
    }
    pub fn event(&self, event: &serde_json::Value) {
        if self.events {
            self.write(&format!("{event}\n"), false);
        }
    }
    pub fn error(&self, text: impl AsRef<str>) {
        self.write(&format!("{}\n", text.as_ref()), true);
    }
    pub fn debug(&self, text: impl AsRef<str>) {
        if self.verbose {
            self.error(format!(
                "[{}] {}",
                chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f"),
                text.as_ref()
            ));
        }
    }
    pub fn transcript(&self, text: &str, final_result: bool) {
        if self.events {
            return;
        }
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let rendered = {
            let mut state = self.inner.lock().unwrap();
            let rendered = if state.terminal && final_result {
                let mut result = if state.visible {
                    "\n".into()
                } else {
                    String::new()
                };
                if !text.is_empty() {
                    result += &format!("{text}\n");
                }
                state.visible = false;
                result
            } else if state.terminal && (!text.is_empty() || state.visible) {
                state.visible = !text.is_empty();
                format!("\r\x1b[2K{text}")
            } else if !state.terminal && !text.is_empty() && (final_result || text != state.last) {
                format!("{text}\n")
            } else {
                String::new()
            };
            state.last = if final_result { String::new() } else { text };
            rendered
        };
        if !rendered.is_empty() {
            self.write(&rendered, false);
        }
    }
    pub fn close(&self) {
        let visible = {
            let mut s = self.inner.lock().unwrap();
            let v = s.visible;
            s.visible = false;
            v
        };
        if visible {
            self.line("");
        }
    }
}
