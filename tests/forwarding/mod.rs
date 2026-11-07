use super::*;
fn message(index: u64, text: &str) -> Value {
    json!({"sender":"10086","date":"26/09/06,12:00:00+32","text":text,"indices":[index]})
}
fn notification() -> Notification {
    Notification {
        id: "sample".into(),
        device: "Test module".into(),
        sender: "10086".into(),
        received_at: "2026-09-06T12:00:00+08:00".into(),
        text: "Test SMS".into(),
        parts: Vec::new(),
        kind: Kind::Sms,
    }
}

#[test]
fn older_channel_lists_migrate_without_writes_or_losing_credentials() {
    let (f, _dir) = forwarder();
    // Five channels (before the SIM channel) and six (before Bark).
    for kept in [5, 6] {
        let mut settings = Settings::default();
        settings.channels.truncate(kept);
        settings.sms_platforms.truncate(kept);
        settings.channels[0].token = "SCT123456789".into();
        std::fs::write(&f.path, serde_json::to_vec(&settings).unwrap()).unwrap();
        let before = std::fs::read(&f.path).unwrap();
        let loaded = Forwarder::new(f.path.clone(), Arc::new(Store::new(true)));
        let snapshot = loaded.snapshot();
        let platforms: Vec<_> = snapshot["channels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["platform"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(platforms, PLATFORMS);
        assert_eq!(snapshot["error"], "");
        // A channel added this way is off and not used until chosen.
        assert_eq!(snapshot["channels"][6]["enabled"], false);
        assert!(
            !loaded
                .state
                .lock()
                .unwrap()
                .settings
                .sms_platforms
                .contains(&"bark".to_string())
        );
        assert_eq!(
            loaded.state.lock().unwrap().settings.channels[0].token,
            "SCT123456789"
        );
        assert_eq!(std::fs::read(&f.path).unwrap(), before);
    }
}

#[tokio::test]
async fn sim_forwarding_splits_messages_and_never_sends_when_disabled_or_looping() {
    let (f, _dir) = forwarder();
    let at = At::start(true, vec![]).unwrap();
    assert!(f.at.set(at.clone()).is_ok());
    let c = Channel {
        platform: "sim".into(),
        enabled: true,
        number: "+8613800138000".into(),
        ..Channel::default()
    };
    let mut note = notification();
    note.text = "短信转发".repeat(60);
    {
        let mut state = f.state.lock().unwrap();
        state.settings.enabled = true;
        state.settings.channels[5] = c.clone();
    }
    f.send_sim(&c, &note, RETRY_WINDOW, 0).await.unwrap();
    assert!(
        at.trace
            .lock()
            .unwrap()
            .iter()
            .filter(|cmd| cmd.contains("+CMGS="))
            .count()
            > 1
    );
    assert!(f.client.get().is_none());
    at.trace.lock().unwrap().clear();
    note.sender = c.number.clone();
    assert!(f.send_sim(&c, &note, RETRY_WINDOW, 0).await.is_err());
    assert!(
        !at.trace
            .lock()
            .unwrap()
            .iter()
            .any(|cmd| cmd.contains("+CMGS="))
    );
    f.sms_settings(false, false).await.unwrap();
    at.trace.lock().unwrap().clear();
    assert!(
        f.send_sim(&c, &notification(), RETRY_WINDOW, 0)
            .await
            .is_err()
    );
    assert!(at.trace.lock().unwrap().is_empty());
    assert!(f.test("sim").await.is_err());
}

#[tokio::test]
async fn unconfirmed_sim_segment_is_not_retried_or_deleted() {
    let (f, _dir) = forwarder();
    let at = At::start(true, vec![]).unwrap();
    assert!(f.at.set(at.clone()).is_ok());
    let note = notification();
    let c = Channel {
        platform: "sim".into(),
        enabled: true,
        number: "+8613800138000".into(),
        ..Channel::default()
    };
    let content = format!(
        "[{}] SMS\nFrom: {}\nTime: {}\n\n{}",
        note.device, note.sender, note.received_at, note.text
    );
    for (_, len) in sms::submit(&c.number, &content, 1).unwrap() {
        at.overrides
            .lock()
            .unwrap()
            .insert(format!("AT+CMGF=0;+CMGS={len}"), "+CMS ERROR: 500".into());
    }
    {
        let mut s = f.state.lock().unwrap();
        s.settings.channels[5] = c;
        s.settings.enabled = true;
        s.settings.delete_after_day = true;
        s.outcomes.insert(note.id.clone(), (1, false));
        s.queue.push_back(Job {
            notification: Arc::new(note),
            channel: 5,
            part: 0,
            devices_done: 0,
            attempts: 0,
            created: Instant::now(),
            next: Instant::now(),
        });
    }
    f.deliver_next().await;
    f.deliver_next().await;
    assert_eq!(
        at.trace
            .lock()
            .unwrap()
            .iter()
            .filter(|cmd| cmd.contains("+CMGS="))
            .count(),
        1
    );
    assert_eq!(f.snapshot()["queued"], 0);
    assert_eq!(f.snapshot()["records"][0]["status"], "failed");
    assert_eq!(f.snapshot()["cleanup"]["pending"], 0);
}
fn forwarder() -> (Forwarder, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    (
        Forwarder::new(
            dir.path().join("forwarding.json"),
            Arc::new(Store::new(true)),
        ),
        dir,
    )
}
#[test]
fn baseline_dedup_and_recycled_storage() {
    let mut d = Detector::default();
    let old = message(1, "old");
    let next = message(2, "new");
    assert!(d.scan(vec![old.clone()], "device").unwrap().is_empty());
    assert_eq!(
        d.scan(vec![old.clone(), next.clone()], "device")
            .unwrap()
            .len(),
        1
    );
    assert!(
        d.scan(vec![old.clone(), next], "device")
            .unwrap()
            .is_empty()
    );
    assert!(d.scan(vec![], "device").unwrap().is_empty());
    assert_eq!(
        d.scan(vec![message(1, "another")], "device").unwrap().len(),
        1
    );
    assert_eq!(d.seen.len(), 1);
}
#[test]
fn multipart_waits_for_all_parts_and_never_replays_baseline_fragments() {
    let mut d = Detector::default();
    d.scan(vec![], "device").unwrap();
    let mut first = message(1, "hello ");
    first["concatRef"] = json!("10086:12");
    first["concatTotal"] = json!(2);
    first["concatSeq"] = json!(1);
    let mut second = message(2, "world");
    second["concatRef"] = json!("10086:12");
    second["concatTotal"] = json!(2);
    second["concatSeq"] = json!(2);
    assert!(d.scan(vec![first.clone()], "device").unwrap().is_empty());
    let notes = d
        .scan(vec![second.clone(), first.clone()], "device")
        .unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].text, "hello world");
    assert!(
        d.scan(vec![second.clone(), first.clone()], "device")
            .unwrap()
            .is_empty()
    );
    let mut d = Detector::default();
    d.scan(vec![first.clone()], "device").unwrap();
    assert!(d.scan(vec![first, second], "device").unwrap().is_empty());
}
#[test]
fn multipart_reference_reuse_does_not_join_separate_messages() {
    let mut d = Detector::default();
    d.scan(vec![], "device").unwrap();
    let mut entries = Vec::new();
    for i in 0..4 {
        let mut v = message(i + 1, if i < 2 { "A" } else { "B" });
        v["concatRef"] = json!("same-ref");
        v["concatTotal"] = json!(2);
        v["concatSeq"] = json!(i % 2 + 1);
        entries.push(v);
    }
    let mut result = d
        .scan(entries, "device")
        .unwrap()
        .iter()
        .map(|n| n.text.clone())
        .collect::<Vec<_>>();
    result.sort();
    assert_eq!(result, vec!["AA", "BB"]);
}
#[test]
fn only_received_sms_enters_forwarding() {
    let raw = "+CMGL: 1,\"REC READ\",\"10086\",,\"26/09/06,12:00:00+32\"\nhello\n+CMGL: 2,\"STO SENT\",\"10086\",,\"26/09/06,12:00:01+32\"\noutgoing\nOK";
    let entries = sms::received(raw);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["text"], "hello");
    assert!(sms::received("+CMGL: 3,0,,12\ninvalid PDU\nOK").is_empty());
}
#[tokio::test]
async fn received_sms_from_at_transaction_keeps_full_envelope() {
    let at = At::start(true, vec![]).unwrap();
    at.overrides.lock().unwrap().insert(
        "sms".into(),
        "+CMGL: 2,\"REC READ\",\"10086\",,\"26/09/06,12:00:00+32\"\nNew SMS: 123456\nOK".into(),
    );
    let raw = at.page("sms", true).await.unwrap();
    assert!(parser::ok(&raw));
    let entries = sms::received(&raw);
    assert_eq!(entries.len(), 1, "{raw}");
    assert_eq!(entries[0]["text"], "New SMS: 123456");
    let mut d = Detector::default();
    d.scan(entries, "Test").unwrap();
    at.overrides.lock().unwrap().insert(
        "sms".into(),
        "+CMGL: 3,\"REC READ\",\"10086\",,\"26/09/06,12:00:00+32\"\nNext SMS\nOK".into(),
    );
    at.invalidate().await;
    let raw = at.page("sms", false).await.unwrap();
    assert_eq!(d.scan(sms::received(&raw), "Test").unwrap().len(), 1);
}
#[test]
fn utf8_chunks_preserve_entire_message() {
    let text = "短信😀".repeat(1500);
    let parts = chunks(&text, 1200);
    assert!(parts.len() > 1);
    assert!(parts.iter().all(|p| p.len() <= 1200));
    assert_eq!(parts.concat(), text);
}
#[test]
fn platform_payloads_and_signatures() {
    let n = notification();
    let server = Channel {
        platform: "serverchan".into(),
        token: "SCT123456789".into(),
        ..Channel::default()
    };
    let (url, p) = payload(&server, &n, &n.text, 0, 1, 1700000000, false).unwrap();
    assert_eq!(url.as_str(), "https://sctapi.ftqq.com/SCT123456789.send");
    assert!(p["desp"].as_str().unwrap().contains("10086"));
    for platform in ["wecom", "dingtalk", "feishu", "webhook"] {
        let c = Channel {
            platform: platform.into(),
            url: "https://example.com/hook?token=keep".into(),
            secret: "secret".into(),
            ..Channel::default()
        };
        let (url, p) = payload(&c, &n, &n.text, 0, 1, 1700000000, false).unwrap();
        if platform == "dingtalk" {
            let pairs: HashMap<_, _> = url.query_pairs().collect();
            assert_eq!(pairs["timestamp"], "1700000000000");
            assert_eq!(pairs["sign"], sign("secret", "1700000000000\nsecret"));
            assert_eq!(pairs["token"], "keep");
        }
        if platform == "feishu" {
            assert_eq!(p["timestamp"], "1700000000");
            assert_eq!(p["sign"], sign("1700000000\nsecret", ""));
        }
        if platform == "webhook" {
            assert_eq!(p["text"], "Test SMS");
            assert_eq!(p["sender"], "10086");
            assert_eq!(p["parts"], 1);
        }
    }
    assert_eq!(
        sign("key", "The quick brown fox jumps over the lazy dog"),
        "97yD9DBThCSxMpjmqm+xQ+9NWaFJRhdZl0edvC0aPNg="
    );
}
#[tokio::test]
async fn settings_persist_privately_preserve_secrets_and_skip_identical_writes() {
    let (f, _dir) = forwarder();
    let mut settings = Settings::default();
    settings.channels[0].token = "SCT123456789".into();
    settings.channels[0].enabled = true;
    f.save(settings.clone()).await.unwrap();
    let first = std::fs::metadata(&f.path).unwrap().modified().unwrap();
    let snapshot = f.snapshot().to_string();
    assert!(!snapshot.contains("SCT123456789"));
    assert!(snapshot.contains("has_token"));
    settings.channels[0].token.clear();
    f.save(settings.clone()).await.unwrap();
    assert_eq!(
        first,
        std::fs::metadata(&f.path).unwrap().modified().unwrap()
    );
    let loaded = Forwarder::new(f.path.clone(), Arc::new(Store::new(true)));
    assert_eq!(
        loaded.state.lock().unwrap().settings.channels[0].token,
        "SCT123456789"
    );
    settings.channels[0].clear = true;
    f.save(settings).await.unwrap();
    assert_eq!(f.snapshot()["channels"][0]["has_token"], false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&f.path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
#[tokio::test]
async fn expired_jobs_are_not_sent_and_disabling_clears_queue() {
    let (f, _dir) = forwarder();
    {
        let mut s = f.state.lock().unwrap();
        s.queue.push_back(Job {
            notification: Arc::new(notification()),
            channel: 4,
            part: 0,
            devices_done: 0,
            attempts: 1,
            created: Instant::now() - Duration::from_secs(181),
            next: Instant::now(),
        });
    }
    f.deliver_next().await;
    assert_eq!(f.snapshot()["queued"], 0);
    assert_eq!(f.snapshot()["records"][0]["detail"], "retry window expired");
    assert!(f.client.get().is_none());
}
#[test]
fn senders_pick_their_own_enabled_channels() {
    let mut s = Settings::default();
    s.channels[4].enabled = true;
    s.channels[4].url = "http://127.0.0.1/hook".into();
    s.enabled = true;
    s.sms_platforms = vec!["wecom".into()];
    assert!(
        s.validate().is_err(),
        "SMS forwarding has no enabled channel"
    );
    s.sms_platforms = vec!["webhook".into()];
    assert!(s.validate().is_ok());
    s.outage_enabled = true;
    assert!(s.validate().is_err(), "outage notices name no channel");
    s.outage_platforms = vec!["webhook".into(), "webhook".into()];
    assert!(s.validate().is_err(), "duplicate");
    s.outage_platforms = vec!["webhook".into()];
    assert!(s.validate().is_ok());
    s.outage_min_seconds = 5;
    assert!(s.validate().is_err());
    s.outage_min_seconds = 60;
    s.utc_offset_minutes = 900;
    assert!(s.validate().is_err());
    s.utc_offset_minutes = 480;
    s.language = "fr".into();
    assert!(s.validate().is_err());
    // Files written before these fields existed keep forwarding SMS on every channel.
    let old: Settings = serde_json::from_str(r#"{"enabled":false}"#).unwrap();
    assert_eq!(old.sms_platforms.len(), PLATFORMS.len());
    assert!(!old.outage_enabled);
}
#[test]
fn outage_notices_are_queued_once_and_only_when_long_enough() {
    let (f, _dir) = forwarder();
    let outage = Outage {
        lost: 1_760_000_000_000,
        restored: 1_760_000_185_000,
    };
    assert_eq!(f.notify_outage(outage), 0, "off by default");
    {
        let mut s = f.state.lock().unwrap();
        s.settings.channels[4].enabled = true;
        s.settings.channels[4].url = "http://127.0.0.1/hook".into();
        s.settings.channels[1].enabled = true;
        s.settings.outage_enabled = true;
        s.settings.outage_platforms = vec!["webhook".into()];
        s.settings.utc_offset_minutes = 480;
    }
    assert_eq!(f.notify_outage(outage), 1, "only the chosen channel");
    let job = f.state.lock().unwrap().queue[0].notification.clone();
    assert_eq!(job.event(), "outage");
    assert_eq!(
        job.text,
        "断网时间: 2025-10-09 16:53:20 (UTC+8)\n恢复时间: 2025-10-09 16:56:25 (UTC+8)\n断网时长: 3 分 5 秒"
    );
    f.state.lock().unwrap().settings.outage_min_seconds = 300;
    assert_eq!(f.notify_outage(outage), 0, "shorter than the threshold");
    let backwards = Outage {
        lost: 2_000_000,
        restored: 1_000_000,
    };
    f.state.lock().unwrap().settings.outage_min_seconds = 10;
    assert_eq!(f.notify_outage(backwards), 0, "clock set during the outage");
    let c = Channel {
        platform: "webhook".into(),
        url: "http://127.0.0.1/hook".into(),
        ..Channel::default()
    };
    let (_, body) = payload(&c, &job, &job.text, 0, 1, 1_700_000_000, false).unwrap();
    assert_eq!(body["event"], "network.restored");
    assert_eq!(body["seconds"], 185);
    assert_eq!(body["lost_at"], "2025-10-09T08:53:20+00:00");
    let wecom = Channel {
        platform: "wecom".into(),
        ..c
    };
    let (_, body) = payload(&wecom, &job, &job.text, 0, 1, 1_700_000_000, true).unwrap();
    assert!(
        body["text"]["content"]
            .as_str()
            .unwrap()
            .starts_with("[RM520N-EU] Internet restored\n")
    );
}
#[test]
fn durations_read_naturally() {
    assert_eq!(duration(5, true), "5 s");
    assert_eq!(duration(65, false), "1 分 5 秒");
    assert_eq!(duration(3600, true), "1 h");
    assert_eq!(duration(3725, false), "1 小时 2 分 5 秒");
    assert_eq!(clock(0, -330), "1969-12-31 18:30:00 (UTC-5:30)");
}
#[test]
fn rejects_credential_leaks_and_unbounded_configuration() {
    let mut s = Settings::default();
    s.channels[1].enabled = true;
    s.channels[1].url = "https://evil.example/cgi-bin/webhook/send?key=secret".into();
    assert!(s.validate().is_err());
    s.channels[1].url = "https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=test".into();
    assert!(s.validate().is_ok());
    s.channels[4].enabled = true;
    s.channels[4].url = "http://user:password@localhost/hook".into();
    assert!(s.validate().is_err());
    s.channels[4].url = "http://127.0.0.1/hook".into();
    assert!(s.validate().is_ok());
    s.channels[4].token = "Bearer test\r\nHost: other".into();
    assert!(s.validate().is_err());
}
#[test]
fn cleanup_requires_every_channel_to_confirm() {
    let (f, _dir) = forwarder();
    let mut n = notification();
    n.parts = vec![Part {
        storage: sms::Storage::ME,
        index: 1,
        fingerprint: [1; 16],
    }];
    let mut s = f.state.lock().unwrap();
    s.settings.delete_after_day = true;
    s.outcomes.insert(n.id.clone(), (2, false));
    f.completed(&mut s, &n, true);
    assert_eq!(f.cleanup.status()["pending"], 0);
    f.completed(&mut s, &n, false);
    assert_eq!(f.cleanup.status()["pending"], 0);
    s.outcomes.insert(n.id.clone(), (2, false));
    f.completed(&mut s, &n, true);
    f.completed(&mut s, &n, true);
    assert_eq!(f.cleanup.status()["pending"], 1);
}
#[tokio::test]
async fn disabling_sms_clears_pending_work_and_preserves_channel_settings() {
    let (f, _dir) = forwarder();
    let mut settings = Settings::default();
    settings.channels[0].token = "SCT123456789".into();
    settings.channels[0].enabled = true;
    settings.enabled = true;
    f.save(settings).await.unwrap();
    {
        let mut s = f.state.lock().unwrap();
        s.queue.push_back(Job {
            notification: Arc::new(notification()),
            channel: 0,
            part: 0,
            devices_done: 0,
            attempts: 0,
            created: Instant::now(),
            next: Instant::now(),
        });
    }
    f.sms_settings(true, true).await.unwrap();
    assert_eq!(
        f.snapshot()["queued"],
        1,
        "changing cleanup policy must preserve retries"
    );
    // Outage notices have their own settings; saving them keeps the SMS retries and dedup.
    f.state.lock().unwrap().detector.initialized = true;
    let mut outage = f.state.lock().unwrap().settings.clone();
    outage.outage_enabled = true;
    outage.outage_platforms = vec!["serverchan".into()];
    f.save(outage.clone()).await.unwrap();
    assert_eq!(f.snapshot()["queued"], 1);
    assert_eq!(f.snapshot()["ready"], true);
    assert_eq!(
        f.notify_outage(Outage {
            lost: 1_760_000_000_000,
            restored: 1_760_000_100_000
        }),
        1
    );
    outage.outage_enabled = false;
    f.save(outage).await.unwrap();
    assert_eq!(
        f.snapshot()["queued"],
        1,
        "only the outage notice is dropped"
    );
    assert_eq!(f.snapshot()["ready"], true);
    f.sms_settings(false, true).await.unwrap();
    assert!(!f.sms_enabled());
    assert_eq!(f.snapshot()["queued"], 0);
    assert_eq!(f.snapshot()["channels"][0]["has_token"], true);
    let loaded = Forwarder::new(f.path.clone(), Arc::new(Store::new(true)));
    assert!(!loaded.sms_enabled());
    assert_eq!(loaded.snapshot()["ready"], false);
}
#[tokio::test]
async fn webhook_posts_json_and_requires_http_success() {
    use axum::{Router, routing::post};
    let received = Arc::new(Mutex::new(Vec::<Value>::new()));
    let capture = received.clone();
    let app = Router::new().route(
        "/hook",
        post(
            move |headers: axum::http::HeaderMap, axum::Json(body): axum::Json<Value>| async move {
                assert_eq!(headers["authorization"], "Bearer sample");
                assert_eq!(headers["idempotency-key"], "sample-1");
                capture.lock().unwrap().push(body);
                axum::http::StatusCode::NO_CONTENT
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (f, _dir) = forwarder();
    let c = Channel {
        platform: "webhook".into(),
        url: format!("http://{address}/hook"),
        token: "Bearer sample".into(),
        ..Channel::default()
    };
    let n = notification();
    f.send(&c, &n, &n.text, 0, 1, Duration::from_secs(5), None)
        .await
        .unwrap();
    assert_eq!(received.lock().unwrap()[0]["device"], "Test module");
    let bad = Channel {
        url: format!("http://{address}/missing"),
        ..c
    };
    assert_eq!(
        f.send(&bad, &n, &n.text, 0, 1, Duration::from_secs(5), None)
            .await
            .unwrap_err(),
        ("HTTP 404".into(), false)
    );
    server.abort();
}
#[tokio::test]
async fn damaged_settings_file_is_rewritten_even_with_default_values() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("forwarding.json");
    std::fs::write(&path, "{damaged").unwrap();
    let f = Forwarder::new(path.clone(), Arc::new(Store::new(true)));
    assert_eq!(f.snapshot()["error"], "invalid saved forwarding settings");
    // Saving what is already in memory still repairs the file and clears the warning.
    f.save(Settings::default()).await.unwrap();
    assert_eq!(f.snapshot()["error"], "");
    let loaded = Forwarder::new(path.clone(), Arc::new(Store::new(true)));
    assert_eq!(loaded.snapshot()["error"], "");
    let first = std::fs::metadata(&path).unwrap().modified().unwrap();
    loaded.save(Settings::default()).await.unwrap();
    assert_eq!(first, std::fs::metadata(&path).unwrap().modified().unwrap());
}
#[test]
fn bark_titles_bodies_and_server_addresses() {
    let n = notification();
    let c = Channel {
        platform: "bark".into(),
        url: "https://bark.example.com/".into(),
        token: "keyA,keyB".into(),
        ..Channel::default()
    };
    let (url, p) = payload(&c, &n, &n.text, 0, 1, 0, false).unwrap();
    assert_eq!(url.as_str(), "https://bark.example.com/push");
    // Title "device-sender"; the body is the message text as received.
    assert_eq!(p, json!({"title":"Test module-10086","body":"Test SMS"}));
    let (_, p) = payload(&c, &n, "second half", 1, 2, 0, false).unwrap();
    assert_eq!(
        p,
        json!({"title":"Test module-10086 (2/2)","body":"second half"})
    );
    let outage = Notification {
        kind: Kind::Outage {
            lost: 0,
            restored: 1,
        },
        sender: String::new(),
        ..notification()
    };
    assert_eq!(
        payload(&c, &outage, "x", 0, 1, 0, false).unwrap().1["title"],
        "Test module-网络已恢复"
    );
    assert_eq!(
        payload(&c, &outage, "x", 0, 1, 0, true).unwrap().1["title"],
        "Test module-Internet restored"
    );
    for (server, push) in [
        ("", "https://api.day.app/push"),
        ("https://bark.example.com", "https://bark.example.com/push"),
        (
            "https://bark.example.com/push",
            "https://bark.example.com/push",
        ),
        (
            "http://192.168.1.2:8080/bark/",
            "http://192.168.1.2:8080/bark/push",
        ),
    ] {
        assert_eq!(bark_push_url(server).unwrap().as_str(), push, "{server}");
    }
    assert_eq!(
        bark_keys(" keyA, keyB，keyA;keyC  "),
        ["keyA", "keyB", "keyC"]
    );
    // A long message stays within one APNs payload per part.
    let long = "短信".repeat(2000);
    let parts = chunks(&long, BARK_PART_BYTES);
    assert!(parts.len() > 1 && parts.iter().all(|p| p.len() <= BARK_PART_BYTES));
}
#[test]
fn bark_needs_device_keys_and_a_plain_server_address() {
    let mut s = Settings::default();
    let bark = s
        .channels
        .iter()
        .position(|c| c.platform == "bark")
        .unwrap();
    s.channels[bark].enabled = true;
    assert!(s.validate().is_err(), "a device key is required");
    s.channels[bark].token = "aBcDeFgHiJkLmNoPqRsTuV,vUtSrQpOnMlKjIhGfEdCbA".into();
    assert!(s.validate().is_ok(), "the public server is the default");
    s.channels[bark].url = "https://bark.example.com/".into();
    assert!(s.validate().is_ok());
    for server in [
        "https://bark.example.com/aBcDeFgHiJkLmNoPqRsTuV/",
        "ftp://bark.example.com",
        "https://user:pw@bark.example.com",
        "https://bark.example.com/?key=1",
        "bark.example.com",
    ] {
        s.channels[bark].url = server.into();
        assert!(s.validate().is_err(), "{server}");
    }
    s.channels[bark].url = "https://bark.example.com".into();
    let many = (0..11)
        .map(|i| format!("key{i}"))
        .collect::<Vec<_>>()
        .join(",");
    for keys in ["https://api.day.app/abc", "abc/def", "a\"b", many.as_str()] {
        s.channels[bark].token = keys.into();
        assert!(s.validate().is_err(), "{keys}");
    }
}
#[tokio::test]
async fn bark_reaches_each_device_once_and_resumes_with_the_failed_one() {
    use axum::{Json, Router, http::StatusCode, routing::post};
    let received = Arc::new(Mutex::new(Vec::<Value>::new()));
    let capture = received.clone();
    let app = Router::new().route(
        "/bark/push",
        post(
            move |headers: axum::http::HeaderMap, Json(body): Json<Value>| async move {
                assert!(
                    headers["content-type"]
                        .to_str()
                        .unwrap()
                        .starts_with("application/json")
                );
                let mut list = capture.lock().unwrap();
                list.push(body.clone());
                let key = body["device_key"].as_str().unwrap_or_default();
                // The second device fails once, as when APNs is briefly unavailable; an
                // unknown key is rejected the way Bark rejects it.
                if key == "keyB" && list.iter().filter(|b| b["device_key"] == "keyB").count() == 1 {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({"code":500,"message":"push failed"})),
                    )
                } else if key == "unknown" {
                    (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"code":400,"message":"failed to get device token"})),
                    )
                } else if key == "nocode" {
                    (StatusCode::OK, Json(json!({})))
                } else {
                    (
                        StatusCode::OK,
                        Json(json!({"code":200,"message":"success"})),
                    )
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (f, _dir) = forwarder();
    let c = Channel {
        platform: "bark".into(),
        url: format!("http://{address}/bark/"),
        token: "keyA, keyB".into(),
        ..Channel::default()
    };
    let n = notification();
    let mut done = 0;
    let wait = Duration::from_secs(5);
    assert_eq!(
        f.deliver(&c, &n, &n.text, 0, 1, wait, &mut done)
            .await
            .unwrap_err(),
        ("device 2/2: HTTP 500".into(), true)
    );
    assert_eq!(done, 1);
    f.deliver(&c, &n, &n.text, 0, 1, wait, &mut done)
        .await
        .unwrap();
    assert_eq!(done, 2);
    let keys: Vec<_> = received
        .lock()
        .unwrap()
        .iter()
        .map(|b| b["device_key"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(keys, ["keyA", "keyB", "keyB"], "keyA is not notified twice");
    assert_eq!(
        received.lock().unwrap()[0],
        json!({"title":"Test module-10086","body":"Test SMS","device_key":"keyA"})
    );
    let unknown = Channel {
        token: "unknown".into(),
        ..c.clone()
    };
    assert_eq!(
        f.deliver(&unknown, &n, &n.text, 0, 1, wait, &mut 0)
            .await
            .unwrap_err(),
        ("HTTP 400".into(), false)
    );
    let nocode = Channel {
        token: "nocode".into(),
        ..c.clone()
    };
    assert_eq!(
        f.deliver(&nocode, &n, &n.text, 0, 1, wait, &mut 0)
            .await
            .unwrap_err()
            .0,
        "provider success code missing"
    );
    server.abort();
}
