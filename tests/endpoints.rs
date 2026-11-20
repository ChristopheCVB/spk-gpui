#[path = "../build/endpoints.rs"]
mod endpoints;

#[test]
fn derives_spk_websocket_from_normalized_origin() {
    let (spk, websocket) = endpoints::spk("HTTPS://SPK.EXAMPLE:8443", false).unwrap();
    assert_eq!(spk.as_str(), "https://spk.example:8443/");
    assert_eq!(websocket.as_str(), "wss://spk.example:8443/_ws");
    assert_eq!(
        endpoints::livekit("wss://voice.example/proxy", false)
            .unwrap()
            .as_str(),
        "wss://voice.example/proxy"
    );
}

#[test]
fn rejects_ambiguous_or_credential_bearing_inputs() {
    for value in [
        "",
        "spk.example",
        "https:///spk.example",
        "https://spk.example:99999",
        "https://spk.example/path/..",
        "https://username:secret@spk.example",
        "https://@spk.example",
        "https://spk.example?query",
        "https://spk.example/#",
        "https://spk.example\n",
    ] {
        assert!(endpoints::spk(value, true).is_err(), "accepted {value:?}");
    }
    assert!(endpoints::livekit("https://voice.example", true).is_err());
    assert!(endpoints::livekit("wss://voice.example/?", true).is_err());
}

#[test]
fn allows_insecure_endpoints_only_on_debug_loopback() {
    let (_, websocket) = endpoints::spk("http://localhost:3000", true).unwrap();
    assert_eq!(websocket.as_str(), "ws://localhost:3000/_ws");
    assert!(endpoints::livekit("ws://127.0.0.1:7880", true).is_ok());
    assert!(endpoints::livekit("ws://[::1]:7880", true).is_ok());
    assert!(endpoints::spk("http://localhost:3000", false).is_err());
    assert!(endpoints::livekit("ws://127.0.0.1:7880", false).is_err());
    assert!(endpoints::spk("http://localhost.example", true).is_err());
    assert!(endpoints::livekit("ws://voice.example", true).is_err());
}
