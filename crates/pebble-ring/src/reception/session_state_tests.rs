// Boundary tests deliberately use a 50 ms policy; production defaults are tested separately.
fn short_window() -> SessionState {
    SessionState::new(Reception {
        tap_sequence_grace_ms: 50,
        ..Reception::default()
    })
    .unwrap()
}

use super::session_state::*;
use super::{button_detector::Press, config::Reception};

fn state(m: &mut SessionState, at: u64, active: bool, unread: u64) -> Transition {
    m.observe(at, Observation::Collecting { active, unread })
        .unwrap()
}
#[allow(clippy::too_many_arguments)] // Explicit wire observations make replay cases readable.
fn source(
    m: &mut SessionState,
    at: u64,
    name: &str,
    first: u64,
    last: u64,
    press: Option<Press>,
    final_seen: bool,
    complete: bool,
) -> Transition {
    m.observe(
        at,
        Observation::Source(SourceObservation {
            id: name.into(),
            first_collection: first,
            last_collection: last,
            classification: press,
            final_seen,
            complete,
        }),
    )
    .unwrap()
}
fn audio(
    m: &mut SessionState,
    at: u64,
    name: &str,
    index: u64,
    press: Option<Press>,
    complete: bool,
) -> Transition {
    source(m, at, name, index, index, press, complete, complete)
}
fn gestures(t: &Transition) -> Vec<Gesture> {
    t.actions
        .iter()
        .filter_map(|a| match a {
            Action::Gesture(g) => Some(g.gesture),
            _ => None,
        })
        .collect()
}
fn batches(t: &Transition) -> Vec<(SessionId, Vec<String>)> {
    t.actions
        .iter()
        .filter_map(|a| match a {
            Action::Recognize { session, sources } => Some((*session, sources.clone())),
            _ => None,
        })
        .collect()
}
fn start_long(m: &mut SessionState) -> SessionId {
    let id = state(m, 0, true, 1).snapshot.session_id.unwrap();
    audio(m, 20, "a", 1, Some(Press::Long), false);
    assert_eq!(m.tick(50).unwrap().snapshot.app_state, AppState::Recording);
    id
}

#[test]
fn counter_discontinuity_cannot_merge_holds_or_fire_an_old_pending_tap() {
    let mut m = short_window();
    audio(&mut m, 0, "tap", 1, Some(Press::Short), true);
    state(&mut m, 20, true, 2); // Accepted prefix, still no classified audio.
    let t = m
        .observe(25, Observation::Discontinuity { unread: 65537 })
        .unwrap();
    assert!(t.sessions.is_empty());
    assert!(gestures(&t).is_empty());
    let new = state(&mut m, 30, true, 65537).snapshot.session_id.unwrap();
    assert_eq!(m.snapshot().prefix, None);
    audio(&mut m, 40, "new", 65537, Some(Press::Long), true);
    let t = m.tick(90).unwrap();
    assert_eq!(gestures(&t), [Gesture::LongHold]);
    assert_eq!(batches(&t), [(new, vec!["new".into()])]);
}

#[test]
fn discontinuity_keeps_submitted_and_complete_audio_but_fails_incomplete_audio() {
    let mut m = short_window();
    audio(&mut m, 0, "submitted", 1, Some(Press::Long), true);
    let submitted = batches(&m.tick(50).unwrap())[0].0;
    audio(&mut m, 51, "ready", 2, Some(Press::Long), true);
    let ready = m.source_session("ready").unwrap();
    state(&mut m, 52, true, 3);
    audio(&mut m, 53, "incomplete", 3, None, false);
    let incomplete = m.source_session("incomplete").unwrap();
    let t = m
        .observe(54, Observation::Discontinuity { unread: 65537 })
        .unwrap();
    assert!(t.sessions.iter().any(|s| s.id == submitted && !s.failed));
    assert!(t.sessions.iter().any(|s| s.id == ready && !s.failed));
    assert!(
        t.sessions
            .iter()
            .any(|s| s.id == incomplete && s.failed && !s.live)
    );
    assert!(batches(&t).is_empty());
    assert_eq!(
        batches(&m.tick(101).unwrap()),
        [(ready, vec!["ready".into()])]
    );
    let t = m.observe(102, Observation::Recognized(submitted)).unwrap();
    assert!(!t.sessions.iter().any(|s| s.id == submitted));
    assert!(t.sessions.iter().any(|s| s.id == ready));
}

#[test]
fn losing_a_parent_detaches_its_provisional_child_before_new_audio_arrives() {
    let mut m = short_window();
    let parent = start_long(&mut m);
    state(&mut m, 100, false, 2);
    state(&mut m, 120, true, 2);
    let t = m.observe(125, Observation::Lost("a".into())).unwrap();
    let child = t.snapshot.session_id.unwrap();
    assert_ne!(child, parent);
    audio(&mut m, 130, "child", 2, Some(Press::Long), true);
    let t = m.tick(180).unwrap();
    assert_eq!(batches(&t), [(child, vec!["child".into()])]);
    assert!(t.sessions.iter().any(|s| s.id == parent && s.failed));
}

#[test]
fn a_new_lost_source_emits_no_live_or_batch_even_with_short_metadata() {
    let mut m = short_window();
    state(&mut m, 0, true, 3); // First available C; the prefix is already gone.
    let t = m
        .observe(
            60,
            Observation::LostSource(SourceObservation {
                id: "lost".into(),
                first_collection: 1,
                last_collection: 3,
                classification: Some(Press::Short),
                final_seen: true,
                complete: false,
            }),
        )
        .unwrap();
    assert!(t.sessions[0].failed);
    assert!(!t.sessions[0].live);
    assert!(gestures(&t).is_empty());
    assert!(batches(&t).is_empty());
    let failed = t.sessions[0].id;
    assert_eq!(t.sessions.len(), 1);
    let t = state(&mut m, 100, true, 4);
    assert_eq!(t.sessions.len(), 1);
    assert_eq!(t.sessions[0].id, failed);
    state(&mut m, 110, false, 4);
    assert_ne!(
        state(&mut m, 120, true, 4).snapshot.session_id,
        Some(failed)
    );
}

#[test]
fn thirty_ms_short_never_shows_or_recognizes_and_hook_fires_once() {
    let mut m = short_window();
    state(&mut m, 0, true, 1);
    state(&mut m, 30, false, 1);
    let t = audio(&mut m, 40, "a", 1, Some(Press::Short), true);
    assert!(t.sessions.is_empty());
    assert!(batches(&t).is_empty());
    assert_eq!(t.snapshot.gesture_state, GestureState::SinglePending);
    assert!(gestures(&m.tick(89).unwrap()).is_empty());
    assert_eq!(gestures(&m.tick(90).unwrap()), [Gesture::SinglePush]);
    assert!(gestures(&m.tick(200).unwrap()).is_empty());
    assert_eq!(
        m.snapshot().last_completed_gesture.unwrap().gesture,
        Gesture::SinglePush
    );
}
#[test]
fn repeated_true_does_not_extend_fifty_ms_display_delay() {
    let mut m = short_window();
    state(&mut m, 0, true, 1);
    state(&mut m, 25, true, 1);
    assert_eq!(m.tick(49).unwrap().snapshot.app_state, AppState::Idle);
    assert_eq!(m.tick(50).unwrap().snapshot.app_state, AppState::Recording);
}
#[test]
fn two_short_finals_fire_double_immediately_without_single() {
    let mut m = short_window();
    audio(&mut m, 0, "a", 1, Some(Press::Short), true);
    let t = audio(&mut m, 30, "b", 2, Some(Press::Short), true);
    assert_eq!(gestures(&t), [Gesture::DoublePush]);
    assert!(t.sessions.is_empty());
    assert!(gestures(&m.tick(500).unwrap()).is_empty());
}
#[test]
fn second_true_within_tap_window_keeps_prefix_until_late_classification() {
    let mut m = short_window();
    audio(&mut m, 0, "a", 1, Some(Press::Short), true);
    assert_eq!(
        state(&mut m, 30, true, 2).snapshot.prefix,
        Some(Gesture::SinglePush)
    );
    assert!(gestures(&m.tick(80).unwrap()).is_empty());
    let t = audio(&mut m, 150, "b", 2, Some(Press::Short), true);
    assert_eq!(gestures(&t), [Gesture::DoublePush]);
    assert!(batches(&t).is_empty());
}
#[test]
fn single_then_hold_excludes_the_first_short_from_audio() {
    let mut m = short_window();
    audio(&mut m, 0, "a", 1, Some(Press::Short), true);
    state(&mut m, 30, true, 2);
    audio(&mut m, 60, "b", 2, Some(Press::Long), false);
    m.tick(80).unwrap();
    audio(&mut m, 200, "b", 2, Some(Press::Long), true);
    let t = m.tick(250).unwrap();
    assert_eq!(gestures(&t), [Gesture::SingleThenHold]);
    assert_eq!(batches(&t)[0].1, ["b"]);
}
#[test]
fn false_jitter_with_same_source_keeps_the_live_session() {
    let mut m = short_window();
    let id = start_long(&mut m);
    state(&mut m, 100, false, 2);
    let t = state(&mut m, 130, true, 2);
    assert_eq!(t.snapshot.session_id, Some(id));
    assert_eq!(t.sessions.len(), 1);
    assert!(t.sessions[0].live);
    let t = source(&mut m, 140, "a", 1, 2, Some(Press::Long), false, false);
    assert_eq!(t.snapshot.session_id, Some(id));
    assert_eq!(t.snapshot.gesture_state, GestureState::Holding);
    assert_eq!(t.sessions.len(), 1);
}
#[test]
fn final_then_new_long_within_grace_joins_sources_without_changing_final_markers() {
    let mut m = short_window();
    let id = start_long(&mut m);
    audio(&mut m, 100, "a", 1, Some(Press::Long), true);
    state(&mut m, 130, true, 2);
    let t = audio(&mut m, 140, "b", 2, Some(Press::Long), false);
    assert_eq!(t.snapshot.session_id, Some(id));
    assert_eq!(t.sessions[0].sources, ["a", "b"]);
    assert!(batches(&t).is_empty());
    audio(&mut m, 200, "b", 2, Some(Press::Long), true);
    let t = m.tick(250).unwrap();
    assert_eq!(batches(&t), [(id, vec!["a".into(), "b".into()])]);
}
#[test]
fn resumed_short_is_removed_from_long_audio_and_has_its_own_tap_hook() {
    let mut m = short_window();
    let id = start_long(&mut m);
    audio(&mut m, 100, "a", 1, Some(Press::Long), true);
    state(&mut m, 130, true, 2);
    let provisional = audio(&mut m, 140, "b", 2, None, false);
    assert_eq!(provisional.sessions[0].sources, ["a", "b"]);
    let t = audio(&mut m, 200, "b", 2, Some(Press::Short), true);
    assert_eq!(batches(&t), [(id, vec!["a".into()])]);
    assert_eq!(t.sessions[0].sources, ["a"]);
    assert_eq!(gestures(&m.tick(250).unwrap()), [Gesture::SinglePush]);
}
#[test]
fn expired_grace_stops_live_but_keeps_waiting_for_gapless_final() {
    let mut m = short_window();
    start_long(&mut m);
    state(&mut m, 100, false, 2);
    let t = m.tick(150).unwrap();
    assert_eq!(t.snapshot.app_state, AppState::Dictating);
    assert!(!t.sessions[0].live);
    assert!(batches(&t).is_empty());
    let t = source(&mut m, 200, "a", 1, 3, Some(Press::Long), true, false);
    assert!(batches(&t).is_empty());
    let t = source(&mut m, 210, "a", 1, 3, Some(Press::Long), true, true);
    assert_eq!(batches(&t).len(), 1);
}
#[test]
fn unfinished_same_source_can_resume_after_grace_without_duplicate_session() {
    let mut m = short_window();
    let id = start_long(&mut m);
    state(&mut m, 100, false, 2);
    m.tick(150).unwrap();
    state(&mut m, 300, true, 2);
    let t = source(&mut m, 310, "a", 1, 2, Some(Press::Long), false, false);
    assert_eq!(t.snapshot.session_id, Some(id));
    assert_eq!(t.sessions.len(), 1);
    assert_eq!(t.snapshot.app_state, AppState::Recording);
}
#[test]
fn repeated_false_and_late_final_do_not_extend_first_release_deadline() {
    let mut m = short_window();
    start_long(&mut m);
    state(&mut m, 100, false, 2);
    assert_eq!(
        state(&mut m, 120, false, 2).snapshot.resume_deadline,
        Some(150)
    );
    assert_eq!(
        audio(&mut m, 140, "a", 1, Some(Press::Long), true)
            .snapshot
            .resume_deadline,
        Some(150)
    );
    assert_eq!(batches(&m.tick(150).unwrap()).len(), 1);
}
#[test]
fn old_final_and_old_batch_completion_cannot_end_new_holding() {
    let mut m = short_window();
    let old = start_long(&mut m);
    state(&mut m, 100, false, 2);
    m.tick(150).unwrap();
    state(&mut m, 200, true, 2);
    let t = audio(&mut m, 210, "b", 2, Some(Press::Long), false);
    let new = t.snapshot.session_id.unwrap();
    assert_ne!(old, new);
    m.tick(250).unwrap();
    let t = audio(&mut m, 270, "a", 1, Some(Press::Long), true);
    assert_eq!(batches(&t), [(old, vec!["a".into()])]);
    assert_eq!(t.snapshot.session_id, Some(new));
    assert_eq!(t.snapshot.app_state, AppState::Recording);
    let t = m.observe(280, Observation::Recognized(old)).unwrap();
    assert_eq!(t.snapshot.session_id, Some(new));
    assert_eq!(t.snapshot.app_state, AppState::Recording);
}
#[test]
fn cached_true_at_final_does_not_resume_without_a_new_state_observation() {
    let mut m = short_window();
    start_long(&mut m);
    let t = audio(&mut m, 100, "a", 1, Some(Press::Long), true);
    assert!(!t.snapshot.collecting);
    assert_eq!(batches(&m.tick(150).unwrap()).len(), 1);
    assert_eq!(m.tick(200).unwrap().snapshot.app_state, AppState::Dictating);
}
#[test]
fn known_unparsed_collections_freeze_tap_watermark_without_unbounded_extension() {
    let mut m = short_window();
    audio(&mut m, 0, "a", 1, Some(Press::Short), true);
    m.observe(
        40,
        Observation::Watermark {
            known_end: 3,
            processed_end: 2,
        },
    )
    .unwrap();
    assert!(gestures(&m.tick(50).unwrap()).is_empty());
    // Arriving late but already in the frozen receive range: still Double.
    assert_eq!(
        gestures(&audio(&mut m, 100, "b", 2, Some(Press::Short), true)),
        [Gesture::DoublePush]
    );

    let mut m = short_window();
    audio(&mut m, 0, "a", 1, Some(Press::Short), true);
    m.observe(
        40,
        Observation::Watermark {
            known_end: 3,
            processed_end: 2,
        },
    )
    .unwrap();
    m.tick(50).unwrap();
    m.observe(
        60,
        Observation::Watermark {
            known_end: 100,
            processed_end: 3,
        },
    )
    .unwrap();
    assert_eq!(gestures(&m.tick(60).unwrap()), [Gesture::SinglePush]);
}

#[test]
fn state_count_hint_holds_single_until_range_and_known_metadata_arrive() {
    let mut m = short_window();
    audio(&mut m, 0, "a", 1, Some(Press::Short), true);
    m.observe(40, Observation::RangePending(true)).unwrap();
    assert!(gestures(&m.tick(50).unwrap()).is_empty());
    assert!(gestures(&m.tick(300).unwrap()).is_empty());
    m.observe(
        310,
        Observation::Watermark {
            known_end: 3,
            processed_end: 2,
        },
    )
    .unwrap();
    m.observe(310, Observation::RangePending(false)).unwrap();
    assert!(gestures(&m.tick(400).unwrap()).is_empty());
    assert_eq!(
        gestures(&audio(&mut m, 500, "b", 2, Some(Press::Short), true)),
        [Gesture::DoublePush]
    );
}

#[test]
fn resolving_an_empty_hint_releases_single_and_later_hints_cannot_extend_it() {
    let mut m = short_window();
    audio(&mut m, 0, "a", 1, Some(Press::Short), true);
    m.observe(40, Observation::RangePending(true)).unwrap();
    m.tick(50).unwrap();
    m.observe(
        100,
        Observation::Watermark {
            known_end: 2,
            processed_end: 2,
        },
    )
    .unwrap();
    m.observe(100, Observation::RangePending(false)).unwrap();
    // Both observations at the same instant precede a tick. The first resolved
    // fence remains final even when a new count hint is received.
    let t = m.observe(100, Observation::RangePending(true)).unwrap();
    assert_eq!(gestures(&t), [Gesture::SinglePush]);
    assert!(gestures(&m.tick(200).unwrap()).is_empty());
}

#[test]
fn known_range_is_enough_and_does_not_wait_for_an_additional_refresh() {
    let mut m = short_window();
    audio(&mut m, 0, "a", 1, Some(Press::Short), true);
    m.observe(
        40,
        Observation::Watermark {
            known_end: 3,
            processed_end: 2,
        },
    )
    .unwrap();
    m.observe(40, Observation::RangePending(true)).unwrap();
    m.tick(50).unwrap();
    assert_eq!(
        gestures(&audio(&mut m, 100, "b", 2, Some(Press::Short), true)),
        [Gesture::DoublePush]
    );
}
#[test]
fn completed_long_without_s_gets_full_batch_without_recording_flash() {
    let mut m = short_window();
    let t = audio(&mut m, 0, "a", 1, Some(Press::Long), true);
    assert!(!t.sessions[0].visible);
    let t = m.tick(50).unwrap();
    assert_eq!(batches(&t)[0].1, ["a"]);
    assert!(!t.sessions[0].visible);
}
#[test]
fn long_classification_is_not_demoted_by_later_short_history() {
    let mut m = short_window();
    start_long(&mut m);
    audio(&mut m, 100, "a", 1, Some(Press::Short), true);
    let t = m.tick(150).unwrap();
    assert_eq!(gestures(&t), [Gesture::LongHold]);
    assert_eq!(batches(&t).len(), 1);
}
#[test]
fn disconnect_is_not_release_or_five_second_cancellation() {
    let mut m = short_window();
    let id = start_long(&mut m);
    m.observe(100, Observation::Connected(false)).unwrap();
    let t = m.tick(60_000).unwrap();
    assert_eq!(t.snapshot.session_id, Some(id));
    assert!(!t.snapshot.connected);
    assert!(t.snapshot.collecting);
    assert!(t.actions.is_empty());
    assert_eq!(t.sessions[0].sources, ["a"]);
    m.observe(60_010, Observation::Connected(true)).unwrap();
    source(&mut m, 60_020, "a", 1, 2, Some(Press::Long), true, true);
    assert_eq!(batches(&m.tick(60_070).unwrap()).len(), 1);
}
#[test]
fn settings_are_snapshotted_per_candidate() {
    let mut m = short_window();
    state(&mut m, 0, true, 1);
    m.configure(Reception {
        hold_ui_delay_ms: 10,
        ..Reception::default()
    })
    .unwrap();
    assert_eq!(m.tick(10).unwrap().snapshot.app_state, AppState::Idle);
    audio(&mut m, 20, "a", 1, Some(Press::Short), true);
    state(&mut m, 30, true, 2);
    assert_eq!(m.tick(40).unwrap().snapshot.app_state, AppState::Recording);
}
#[test]
fn input_at_ui_deadline_wins_and_stale_deadlines_cannot_show_short() {
    let mut m = short_window();
    state(&mut m, 0, true, 1);
    state(&mut m, 50, false, 1);
    audio(&mut m, 50, "a", 1, Some(Press::Short), true);
    assert_eq!(m.tick(50).unwrap().snapshot.app_state, AppState::Idle);
    assert!(m.tick(80).unwrap().sessions.is_empty());
}
#[test]
fn third_and_fourth_taps_form_pairs_and_hooks_are_not_repeated() {
    let mut m = short_window();
    for (i, name) in ["a", "b", "c", "d"].into_iter().enumerate() {
        let t = audio(
            &mut m,
            i as u64 * 20,
            name,
            i as u64,
            Some(Press::Short),
            true,
        );
        assert_eq!(
            gestures(&t),
            if i % 2 == 0 {
                vec![]
            } else {
                vec![Gesture::DoublePush]
            }
        );
    }
    assert!(gestures(&m.tick(200).unwrap()).is_empty());
}
#[test]
fn unknown_complete_audio_is_processed_independently_without_fake_gesture() {
    let mut m = short_window();
    start_long(&mut m);
    audio(&mut m, 100, "a", 1, Some(Press::Long), true);
    state(&mut m, 130, true, 2);
    let t = audio(&mut m, 140, "b", 2, None, true);
    assert!(batches(&t).is_empty());
    let t = m.tick(190).unwrap();
    assert_eq!(batches(&t).len(), 2);
    assert!(batches(&t).iter().all(|(_, sources)| sources.len() == 1));
    assert_eq!(gestures(&t), [Gesture::LongHold]);
}
#[test]
fn missing_audio_is_explicit_error_and_remains_retained() {
    let mut m = short_window();
    start_long(&mut m);
    let t = m.observe(100, Observation::Lost("a".into())).unwrap();
    assert_eq!(t.snapshot.app_state, AppState::Error);
    assert_eq!(t.sessions[0].sources, ["a"]);
    assert!(batches(&m.tick(10000).unwrap()).is_empty());
}

#[test]
fn next_true_before_previous_short_final_preserves_the_double_prefix() {
    let mut m = short_window();
    state(&mut m, 0, true, 1);
    audio(&mut m, 10, "a", 1, Some(Press::Short), false);
    state(&mut m, 20, false, 2);
    state(&mut m, 40, true, 2);
    let t = audio(&mut m, 60, "a", 1, Some(Press::Short), true);
    assert_eq!(t.snapshot.prefix, Some(Gesture::SinglePush));
    assert!(gestures(&t).is_empty());
    let t = audio(&mut m, 110, "b", 2, Some(Press::Short), true);
    assert_eq!(gestures(&t), [Gesture::DoublePush]);
}
#[test]
fn old_final_does_not_close_a_new_candidate_before_its_first_audio() {
    let mut m = short_window();
    let id = start_long(&mut m);
    state(&mut m, 100, false, 2);
    state(&mut m, 130, true, 2);
    audio(&mut m, 140, "a", 1, Some(Press::Long), true);
    let t = m.tick(190).unwrap();
    assert!(t.snapshot.collecting);
    assert!(batches(&t).is_empty());
    let t = audio(&mut m, 200, "b", 2, Some(Press::Short), true);
    assert_eq!(batches(&t), [(id, vec!["a".into()])]);
}
#[test]
fn distinct_long_sources_wait_for_parent_final_before_confirming_merge() {
    let mut m = short_window();
    let id = start_long(&mut m);
    state(&mut m, 100, false, 2);
    state(&mut m, 130, true, 2);
    let t = audio(&mut m, 140, "b", 2, Some(Press::Long), false);
    assert_eq!(t.snapshot.gesture_state, GestureState::ResumePending);
    assert!(batches(&t).is_empty());
    // The parent's delayed final is not an end event for the second source.
    let t = audio(&mut m, 180, "a", 1, Some(Press::Long), true);
    assert_eq!(t.snapshot.session_id, Some(id));
    assert!(t.snapshot.collecting);
    assert_eq!(t.sessions[0].sources, ["a", "b"]);
}
#[test]
fn malformed_source_does_not_fire_timers_or_mutate_the_timeline() {
    let mut m = short_window();
    state(&mut m, 0, true, 1);
    assert!(
        m.observe(
            100,
            Observation::Source(SourceObservation {
                id: "a".into(),
                first_collection: 2,
                last_collection: 1,
                classification: None,
                final_seen: false,
                complete: false,
            })
        )
        .is_err()
    );
    assert_eq!(m.tick(30).unwrap().snapshot.app_state, AppState::Idle);
    assert_eq!(m.tick(50).unwrap().snapshot.app_state, AppState::Recording);
}
#[test]
fn a_source_cannot_move_between_sessions_as_later_collections_arrive() {
    let mut m = short_window();
    let old = start_long(&mut m);
    state(&mut m, 100, false, 2);
    m.tick(150).unwrap();
    let new = state(&mut m, 200, true, 2).snapshot.session_id.unwrap();
    audio(&mut m, 210, "b", 2, Some(Press::Long), false);
    let t = source(&mut m, 220, "a", 1, 1, Some(Press::Long), false, false);
    assert_eq!(
        t.sessions.iter().find(|s| s.id == old).unwrap().sources,
        ["a"]
    );
    assert_eq!(
        t.sessions.iter().find(|s| s.id == new).unwrap().sources,
        ["b"]
    );
}

#[test]
fn default_tap_window_uses_early_count_hints_without_waiting_for_full_transfer() {
    // C2712 -> C2713 and C2714 -> C2715: the next count was observed
    // after 61/60 ms, although the corresponding C arrived after 241/270 ms.
    for (hint, gap) in [(61, 241), (60, 270)] {
        let mut m = SessionState::default();
        audio(&mut m, 0, "a", 1, Some(Press::Short), true);
        m.observe(hint, Observation::RangePending(true)).unwrap();
        assert!(gestures(&m.tick(70).unwrap()).is_empty());
        m.observe(
            120,
            Observation::Watermark {
                known_end: 3,
                processed_end: 2,
            },
        )
        .unwrap();
        m.observe(120, Observation::RangePending(false)).unwrap();
        assert!(gestures(&m.tick(gap - 1).unwrap()).is_empty());
        assert_eq!(
            gestures(&audio(&mut m, gap, "b", 2, Some(Press::Short), true)),
            [Gesture::DoublePush]
        );
        assert!(gestures(&m.tick(1000).unwrap()).is_empty());
    }
    let mut m = SessionState::default();
    audio(&mut m, 0, "a", 1, Some(Press::Short), true);
    assert!(gestures(&m.tick(69).unwrap()).is_empty());
    assert_eq!(gestures(&m.tick(70).unwrap()), [Gesture::SinglePush]);
    assert!(gestures(&audio(&mut m, 71, "b", 2, Some(Press::Short), true)).is_empty());
    assert_eq!(gestures(&m.tick(141).unwrap()), [Gesture::SinglePush]);
}

#[test]
fn meter_previews_are_read_only_and_cannot_revive_released_or_old_recordings() {
    let mut m = SessionState::default();
    let id = state(&mut m, 0, true, 10).snapshot.session_id.unwrap();
    assert_eq!(m.meter_session(10), None); // Hidden tap candidate.
    m.tick(50).unwrap();
    let before = m.snapshot();
    assert_eq!(m.meter_session(10), Some(id));
    assert_eq!(m.meter_session(9), None);
    assert_eq!(m.snapshot(), before);
    audio(&mut m, 60, "audio", 10, None, false);
    assert_eq!(m.meter_session(10), Some(id));
    assert_eq!(m.meter_session(11), None); // Cannot claim another unknown source.
    state(&mut m, 100, false, 11);
    assert_eq!(m.meter_session(10), None);
    m.observe(101, Observation::Connected(false)).unwrap();
    assert_eq!(m.meter_session(10), None);
    m.observe(102, Observation::Discontinuity { unread: 65537 })
        .unwrap();
    state(&mut m, 110, true, 65537);
    m.tick(160).unwrap();
    assert_eq!(m.meter_session(10), None);
}
