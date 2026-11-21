use super::*;

#[test]
fn participant_playback_matches_watch_and_local_volume_settings() {
    let mut intent = Intent::default();
    let mic = TrackSource::Microphone;
    let screen = TrackSource::ScreenshareAudio;
    assert_eq!(route(&intent, "alice", mic, false), Some(Route::Native));
    assert_eq!(route(&intent, "alice", screen, false), None);
    intent.watching.insert("alice".into());
    assert_eq!(
        route(&intent, "alice", screen, false),
        Some(Route::Media(1.0))
    );
    let volume = intent
        .preferred
        .participant_volumes
        .entry("alice".into())
        .or_default();
    volume.mic = 200;
    volume.screen = 25;
    intent.preferred.output_volume = 150;
    assert_eq!(route(&intent, "alice", mic, false), Some(Route::Voice(3.0)));
    assert_eq!(
        route(&intent, "alice", screen, false),
        Some(Route::Media(0.375))
    );
    assert_eq!(route(&intent, "bob", mic, false), Some(Route::Voice(1.5)));
    assert_eq!(route(&intent, "bob", screen, false), None);
    intent
        .preferred
        .participant_volumes
        .get_mut("alice")
        .unwrap()
        .muted = true;
    assert_eq!(route(&intent, "alice", mic, false), Some(Route::Silent));
    assert_eq!(route(&intent, "alice", screen, false), Some(Route::Silent));
    intent.mute.deafened = true;
    assert_eq!(route(&intent, "bob", mic, false), Some(Route::Silent));
    assert_eq!(route(&intent, "bob", screen, false), None);
    intent.mute.deafened = false;
    assert_eq!(route(&intent, "bob", mic, true), Some(Route::Silent));
    assert_eq!(route(&intent, "bob", mic, false), Some(Route::Voice(1.5)));
    intent.watching.clear();
    assert_eq!(route(&intent, "alice", mic, false), Some(Route::Silent));
    assert_eq!(route(&intent, "alice", screen, false), None);
}
