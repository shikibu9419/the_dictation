//! Real connections to the Realtime API. Needs `OPENAI_API_KEY`; ignored by default.
use openai_realtime::{DEFAULT_MODEL, RealtimeClient, ServerEvent, SessionConfig};
use std::time::Duration;

#[tokio::test]
#[ignore]
async fn session_is_created_and_an_empty_commit_is_rejected() {
    let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY");
    let (client, mut events) =
        RealtimeClient::connect(&api_key, DEFAULT_MODEL, &SessionConfig::default())
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
