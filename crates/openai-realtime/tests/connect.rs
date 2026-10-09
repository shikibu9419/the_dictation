//! Real connections to the Realtime API. Needs `OPENAI_API_KEY`; ignored by default.
use openai_realtime::{
    DEFAULT_MODEL, RealtimeClient, ServerEvent, SessionConfig, Transcription, function_tool,
};
use serde_json::json;
use std::time::Duration;

fn api_key() -> String {
    std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY")
}

#[tokio::test]
#[ignore]
async fn session_is_created_and_an_empty_commit_is_rejected() {
    let (client, mut events) =
        RealtimeClient::connect(&api_key(), DEFAULT_MODEL, &SessionConfig::default())
            .await
            .expect("connect");
    let mut created = false;
    let mut updated = false;
    let mut cleared = false;
    // Less than 100 ms of audio: the client clears instead of committing.
    client.append_audio(&[0i16; 240]).unwrap();
    assert!(!client.commit_and_respond().unwrap());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !(created && updated && cleared) {
        let event = tokio::time::timeout_at(deadline, events.recv())
            .await
            .expect("events before deadline")
            .expect("stream open");
        match event {
            ServerEvent::SessionCreated { .. } => created = true,
            ServerEvent::SessionUpdated { session } => {
                assert_eq!(session["audio"]["input"]["format"]["rate"], 24000);
                assert!(session["audio"]["input"]["turn_detection"].is_null());
                updated = true;
            }
            ServerEvent::InputAudioBufferCleared => cleared = true,
            ServerEvent::Error { error } => panic!("server error: {error:?}"),
            _ => {}
        }
    }
}

/// One spoken turn synthesized with macOS `say`: the user transcript, a
/// function call answered by the test, and the spoken reply must all arrive.
#[tokio::test]
#[ignore]
async fn spoken_request_streams_transcript_tool_call_and_audio() {
    let dir = tempfile::tempdir().unwrap();
    let wav = dir.path().join("request.wav");
    let status = std::process::Command::new("say")
        .args(["-v", "Kyoko", "-o"])
        .arg(&wav)
        .args(["--data-format=LEI16@24000", "牛乳を買うをTODOに追加して"])
        .status()
        .expect("run say");
    assert!(status.success(), "say failed");
    let pcm: Vec<i16> = hound::WavReader::open(&wav)
        .unwrap()
        .samples::<i16>()
        .map(Result::unwrap)
        .collect();
    assert!(pcm.len() > 24_000, "speech shorter than a second");

    let config = SessionConfig {
        instructions: "簡潔な日本語で答えてください。TODOの追加は必ず add_todo を呼んでください。"
            .into(),
        transcription: Some(Transcription {
            model: "gpt-4o-mini-transcribe".into(),
            language: Some("ja".into()),
        }),
        tools: vec![function_tool(
            "add_todo",
            "TODOリストに項目を追加する",
            json!({"type":"object","properties":{"title":{"type":"string"}},"required":["title"]}),
        )],
        ..SessionConfig::default()
    };
    let (client, mut events) = RealtimeClient::connect(&api_key(), DEFAULT_MODEL, &config)
        .await
        .expect("connect");
    for chunk in pcm.chunks(2400) {
        client.append_audio(chunk).unwrap();
    }
    assert!(client.commit_and_respond().unwrap());

    let mut user_transcript = String::new();
    let mut assistant_transcript = String::new();
    let mut audio_samples = 0usize;
    let mut tool_call: Option<(String, String)> = None;
    let mut responses_done = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let event = tokio::time::timeout_at(deadline, events.recv())
            .await
            .expect("events before deadline")
            .expect("stream open");
        match event {
            ServerEvent::InputAudioTranscriptionCompleted { transcript, .. } => {
                user_transcript = transcript;
            }
            ServerEvent::ResponseOutputAudioTranscriptDelta { delta, .. } => {
                assistant_transcript.push_str(&delta);
            }
            ServerEvent::ResponseOutputAudioDelta { delta, .. } => {
                audio_samples += delta.len() * 3 / 4 / 2;
            }
            ServerEvent::ResponseFunctionCallArgumentsDone {
                call_id,
                name,
                arguments,
                ..
            } => {
                tool_call = Some((name.clone().unwrap_or_default(), arguments));
                client
                    .tool_output(&call_id, r#"{"ok":true,"id":1}"#)
                    .unwrap();
            }
            ServerEvent::ResponseDone { response } => {
                responses_done += 1;
                let called_tool = response
                    .output
                    .iter()
                    .any(|item| item.r#type == "function_call");
                if called_tool {
                    client.respond().unwrap();
                } else {
                    break;
                }
            }
            ServerEvent::Error { error } => panic!("server error: {error:?}"),
            _ => {}
        }
    }
    eprintln!("user: {user_transcript}");
    eprintln!("tool: {tool_call:?}");
    eprintln!(
        "assistant: {assistant_transcript} ({audio_samples} samples, {responses_done} responses)"
    );
    assert!(!user_transcript.is_empty(), "no user transcript");
    assert!(!assistant_transcript.is_empty(), "no assistant transcript");
    assert!(audio_samples > 2400, "no assistant audio");
    let (name, arguments) = tool_call.expect("the model should call add_todo");
    assert_eq!(name, "add_todo");
    assert!(arguments.contains("牛乳"), "arguments: {arguments}");
}
