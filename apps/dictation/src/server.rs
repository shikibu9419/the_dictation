use crate::{Serve, audio_file::FileRecognizer};
use anyhow::Result;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Multipart, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use pebble_core::{config::save_json, output::Output};
use serde_json::{Value, json};
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use subtle::ConstantTimeEq;
use tokio::sync::mpsc;
const MAX_BODY: usize = 16 * 1024 * 1024;
#[derive(Clone)]
struct ServerState {
    directory: PathBuf,
    token: String,
    jobs: mpsc::UnboundedSender<PathBuf>,
    output: Output,
}
type Reply = (StatusCode, Json<Value>);
fn reply(code: StatusCode, value: Value) -> Reply {
    (code, Json(value))
}
fn display(record: &Value, heading: &str, output: &Output) {
    let mut lines = vec![
        format!(
            "\n--- {heading}: {} ---",
            record["id"].as_str().unwrap_or("")
        ),
        format!("Status: {}", record["status"].as_str().unwrap_or("")),
        format!(
            "Recorded at: {}",
            record["recorded_at"].as_str().unwrap_or("")
        ),
        format!("Trigger: {}", record["trigger"].as_str().unwrap_or("None")),
    ];
    if let Some(text) = record["phone_transcription"].as_str() {
        lines.push(format!(
            "Phone transcription:\n{}",
            if text.is_empty() { "(empty)" } else { text }
        ));
    }
    if record.get("text").is_some() && record["text"] != record["phone_transcription"] {
        let text = record["text"].as_str().unwrap_or("");
        lines.push(format!(
            "PC transcription:\n{}",
            if text.is_empty() { "(empty)" } else { text }
        ));
    }
    if let Some(error) = record["error"].as_str() {
        lines.push(format!("Error: {error}"));
    }
    output.line(lines.join("\n"));
}
async fn webhook(
    state: State<Arc<ServerState>>,
    headers: HeaderMap,
    body: Result<Multipart, axum::extract::multipart::MultipartRejection>,
) -> Reply {
    tokio::time::timeout(
        std::time::Duration::from_secs(30),
        receive_webhook(state, headers, body),
    )
    .await
    .unwrap_or_else(|_| {
        reply(
            StatusCode::REQUEST_TIMEOUT,
            json!({"error":"request body timed out"}),
        )
    })
}
async fn receive_webhook(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    body: Result<Multipart, axum::extract::multipart::MultipartRejection>,
) -> Reply {
    let auth = headers
        .get("Authorization")
        .map(|h| h.as_bytes())
        .unwrap_or_default();
    let expected = format!("Bearer {}", state.token);
    if !bool::from(auth.ct_eq(expected.as_bytes())) {
        return reply(
            StatusCode::UNAUTHORIZED,
            json!({"error":"invalid Authorization header"}),
        );
    }
    let size = headers
        .get("Content-Length")
        .and_then(|s| s.to_str().ok())
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);
    if size == 0 || size > MAX_BODY {
        return reply(
            StatusCode::PAYLOAD_TOO_LARGE,
            json!({"error":"body must be between 1 byte and 16 MiB"}),
        );
    }
    if !headers
        .get("Content-Type")
        .and_then(|s| s.to_str().ok())
        .is_some_and(|s| s.to_lowercase().starts_with("multipart/form-data"))
    {
        return reply(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            json!({"error":"expected multipart/form-data"}),
        );
    }
    let mut body = match body {
        Ok(body) => body,
        Err(e) => return reply(StatusCode::BAD_REQUEST, json!({"error":e.to_string()})),
    };
    let mut fields = HashMap::new();
    loop {
        match body.next_field().await {
            Ok(Some(field)) => {
                let name = field.name().unwrap_or("").to_owned();
                if fields.contains_key(&name) {
                    return reply(
                        StatusCode::BAD_REQUEST,
                        json!({"error":"Duplicate multipart field"}),
                    );
                }
                match field.bytes().await {
                    Ok(value) => {
                        fields.insert(name, value);
                    }
                    Err(e) => {
                        return reply(StatusCode::BAD_REQUEST, json!({"error":e.to_string()}));
                    }
                }
            }
            Ok(None) => break,
            Err(e) => return reply(StatusCode::BAD_REQUEST, json!({"error":e.to_string()})),
        }
    }
    let result = (|| -> Result<Reply> {
        let text = |key: &str| -> Result<Option<String>> {
            Ok(fields
                .get(key)
                .map(|b| std::str::from_utf8(b).map(str::to_owned))
                .transpose()?)
        };
        if fields.get("test").is_some_and(|b| b.as_ref() == b"true") {
            state.output.line(format!(
                "\n--- Webhook test received ---\n{}",
                text("transcription")?.unwrap_or_default()
            ));
            return Ok(reply(StatusCode::OK, json!({"status":"test received"})));
        }
        let audio = fields.get("audio").filter(|b| !b.is_empty());
        let transcript = text("transcription")?;
        anyhow::ensure!(
            audio.is_some() || transcript.is_some(),
            "Expected audio or transcription field"
        );
        let id = uuid::Uuid::new_v4().simple().to_string();
        let path = state.directory.join(format!("{id}.json"));
        let status = if audio.is_some() {
            "queued"
        } else {
            "complete"
        };
        let mut record = json!({"id":id,"recorded_at":text("recordedAt")?.unwrap_or_default(),"trigger":headers.get("X-Index-Trigger").and_then(|h|h.to_str().ok()),"phone_transcription":transcript,"status":status});
        if let Some(audio) = audio {
            std::fs::write(path.with_extension("m4a"), audio)?;
        } else {
            record["text"] = record["phone_transcription"].clone();
            std::fs::write(
                path.with_extension("txt"),
                format!("{}\n", record["text"].as_str().unwrap_or("")),
            )?;
        }
        save_json(&path, &record)?;
        display(&record, "Webhook received", &state.output);
        if let Some(audio) = audio {
            state.output.line(format!(
                "Audio: {} bytes → {}\nWaiting for PC transcription…",
                audio.len(),
                path.with_extension("m4a").display()
            ));
            state.jobs.send(path)?;
        }
        Ok(reply(
            if audio.is_some() {
                StatusCode::ACCEPTED
            } else {
                StatusCode::OK
            },
            json!({"id":id,"status":status}),
        ))
    })();
    result.unwrap_or_else(|e| {
        reply(
            if e.downcast_ref::<std::io::Error>().is_some() {
                StatusCode::INTERNAL_SERVER_ERROR
            } else {
                StatusCode::BAD_REQUEST
            },
            json!({"error":e.to_string()}),
        )
    })
}
async fn jobs(mut jobs: mpsc::UnboundedReceiver<PathBuf>, language: String, output: Output) {
    let mut recognizer = None;
    while let Some(path) = jobs.recv().await {
        let result = async {
            let mut record: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
            if recognizer.is_none() {
                recognizer = Some(FileRecognizer::start(&language, output.clone()).await?);
            }
            let value = recognizer
                .as_mut()
                .unwrap()
                .recognize(&path.with_extension("m4a"), None, None)
                .await?;
            for (key, value) in value.as_object().unwrap() {
                record[key] = value.clone();
            }
            record["status"] = json!("complete");
            std::fs::write(
                path.with_extension("txt"),
                format!("{}\n", record["text"].as_str().unwrap_or("")),
            )?;
            save_json(&path, &record)?;
            display(&record, "Transcription complete", &output);
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if let Err(error) = result {
            let saved = (|| -> Result<()> {
                let mut record: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
                record["status"] = json!("failed");
                record["error"] = json!(error.to_string());
                save_json(&path, &record)?;
                display(&record, "Transcription failed", &output);
                Ok(())
            })();
            if let Err(e) = saved {
                output.error(format!("Failed to record transcription error: {e:#}"));
            }
            recognizer = None;
        }
    }
}
pub async fn serve(args: Serve, output: Output) -> Result<()> {
    std::fs::create_dir_all(&args.output)?;
    let directory = args.output.canonicalize()?;
    let token = std::env::var("INDEX_WEBHOOK_TOKEN")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            )
        });
    let (tx, rx) = mpsc::unbounded_channel();
    let state = Arc::new(ServerState {
        directory: directory.clone(),
        token: token.clone(),
        jobs: tx.clone(),
        output: output.clone(),
    });
    let app = router(state);
    let listener = tokio::net::TcpListener::bind((args.host.as_str(), args.port)).await?;
    let mut paths = std::fs::read_dir(&directory)?
        .map(|r| r.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.sort();
    for path in paths {
        if path.extension().is_some_and(|e| e == "json") {
            let record: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
            if record["status"] == "queued" {
                tx.send(path)?;
            }
        }
    }
    let worker = tokio::spawn(jobs(rx, args.language, output.clone()));
    struct Abort(tokio::task::JoinHandle<()>);
    impl Drop for Abort {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _worker = Abort(worker);
    output.line(format!(
        "Listening on http://{}:{}/webhook",
        args.host,
        listener.local_addr()?.port()
    ));
    output.line(format!("App header: Authorization: Bearer {token}"));
    output.line(format!(
        "Recordings and transcripts: {}",
        directory.display()
    ));
    axum::serve(listener, app).await?;
    Ok(())
}

fn router(state: Arc<ServerState>) -> Router {
    Router::new()
        .route("/health", get(|| async { Json(json!({"status":"ok"})) }))
        .route("/webhook", post(webhook))
        .fallback(|| async { reply(StatusCode::NOT_FOUND, json!({"error":"not found"})) })
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;
    fn make() -> (tempfile::TempDir, Router, mpsc::UnboundedReceiver<PathBuf>) {
        let directory = tempfile::tempdir().unwrap();
        let (jobs, rx) = mpsc::unbounded_channel();
        let app = router(Arc::new(ServerState {
            directory: directory.path().to_owned(),
            token: "test-secret".into(),
            jobs,
            output: Output::new(false, None).unwrap(),
        }));
        (directory, app, rx)
    }
    fn request(fields: &[(&str, &str)]) -> Request<Body> {
        let body = fields
            .iter()
            .map(|(k, v)| {
                format!("--test\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n")
            })
            .collect::<String>()
            + "--test--\r\n";
        Request::post("/webhook")
            .header("Authorization", "Bearer test-secret")
            .header("Content-Type", "multipart/form-data; boundary=test")
            .header("Content-Length", body.len())
            .body(Body::from(body))
            .unwrap()
    }
    #[tokio::test]
    async fn text_webhook_saves_and_displays_without_audio_engine() {
        let (temp, app, mut rx) = make();
        let response = app
            .oneshot(request(&[
                ("transcription", "こんにちは"),
                ("recordedAt", "2026-10-01"),
            ]))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let data = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let result: Value = serde_json::from_slice(&data).unwrap();
        let path = temp
            .path()
            .join(format!("{}.json", result["id"].as_str().unwrap()));
        let record: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(record["text"], "こんにちは");
        assert_eq!(record["status"], "complete");
        assert_eq!(
            std::fs::read_to_string(path.with_extension("txt")).unwrap(),
            "こんにちは\n"
        );
        assert!(rx.try_recv().is_err());
    }
    #[tokio::test]
    async fn audio_webhook_returns_accepted_and_queues_recording() {
        let (_temp, app, mut rx) = make();
        let response = app
            .oneshot(request(&[
                ("audio", "audio-bytes"),
                ("transcription", "phone text"),
            ]))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let path = rx.try_recv().unwrap();
        assert_eq!(
            std::fs::read(path.with_extension("m4a")).unwrap(),
            b"audio-bytes"
        );
    }
    #[tokio::test]
    async fn auth_size_content_type_and_duplicate_validation() {
        let (_temp, app, _rx) = make();
        let mut req = request(&[("transcription", "hello")]);
        req.headers_mut().remove("Authorization");
        assert_eq!(
            app.clone().oneshot(req).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
        let mut req = request(&[("transcription", "hello")]);
        req.headers_mut().insert(
            "Content-Length",
            (MAX_BODY + 1).to_string().parse().unwrap(),
        );
        assert_eq!(
            app.clone().oneshot(req).await.unwrap().status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        let mut req = request(&[("transcription", "hello")]);
        req.headers_mut()
            .insert("Content-Type", "text/plain".parse().unwrap());
        assert_eq!(
            app.clone().oneshot(req).await.unwrap().status(),
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
        assert_eq!(
            app.oneshot(request(&[
                ("transcription", "hello"),
                ("transcription", "again")
            ]))
            .await
            .unwrap()
            .status(),
            StatusCode::BAD_REQUEST
        );
    }
    #[tokio::test]
    async fn test_webhook_and_health_do_not_persist() {
        let (temp, app, _rx) = make();
        assert_eq!(
            app.clone()
                .oneshot(request(&[("test", "true")]))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            app.oneshot(Request::get("/health").body(Body::empty()).unwrap())
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    }
}
