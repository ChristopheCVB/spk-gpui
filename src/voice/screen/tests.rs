use super::*;

#[test]
fn presets_bound_capture_without_upscaling_or_distorting() {
    let quality = Quality::default();
    assert_eq!(quality.dimensions(3840, 2160), Some((1920, 1080)));
    assert_eq!(quality.dimensions(600, 1200), Some((540, 1080)));
    assert_eq!(quality.dimensions(801, 601), Some((800, 600)));
    assert_eq!(quality.dimensions(1, 480), None);
    assert_eq!(quality.name(), "spk:screen:1080:30");
}

#[test]
fn screen_audio_preferences_survive_reload_and_default_to_web_behavior() {
    let previous: super::super::DevicePreferences = serde_json::from_str("{}").unwrap();
    assert!(previous.screen_audio);
    let changed = super::super::DevicePreferences {
        screen_audio: false,
        ..previous
    };
    let round_trip = serde_json::to_string(&changed).unwrap();
    let restored: super::super::DevicePreferences = serde_json::from_str(&round_trip).unwrap();
    assert!(!restored.screen_audio);
}

#[tokio::test]
async fn cancellation_ignores_late_or_unconfirmed_capture() {
    let (sender, mut requests) = watch::channel(Intent::default());
    sender.send_modify(|intent| intent.screen.open());
    let attempt = requests.borrow().screen.attempt;
    assert!(
        !sender.send_if_modified(|intent| intent.screen.advance(attempt - 1, Action::Start(true)))
    );
    let result = select_until_cancelled(&mut requests, attempt, async {
        sender.send_modify(|intent| intent.screen.open());
        Ok(())
    })
    .await;
    assert_eq!(result, Err(CaptureError::Cancelled));
}

#[test]
fn capture_sinks_reject_unconfirmed_audio() {
    let (sink, mut samples) = CaptureSink::test_pair(false);
    sink.audio(vec![1, 2]);
    assert!(samples.try_recv().is_err());
    let (sink, mut samples) = CaptureSink::test_pair(true);
    sink.audio(vec![1, 2, 3]);
    assert!(samples.try_recv().is_err());
    sink.audio(vec![1, 2]);
    assert_eq!(samples.try_recv().unwrap(), [1, 2]);
}
