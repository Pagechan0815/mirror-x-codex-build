use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use codex_plus_core::relay_switch::{
    switch_relay_profile_in_home, switch_relay_profile_in_home_verified,
};
use codex_plus_core::settings::{
    AggregateRelayMember, AggregateRelayProfile, AggregateRelayStrategy, BackendSettings,
    LaunchMode, RelayMode, RelayProfile, SettingsStore,
};

#[test]
fn switch_rolls_back_active_settings_when_live_write_fails() {
    let temp = tempfile::tempdir().unwrap();
    let store = SettingsStore::new(temp.path().join("settings.json"));
    let original = BackendSettings {
        active_relay_id: "a".to_string(),
        relay_profiles: vec![pure_profile("a", "https://a.example/v1", "sk-a")],
        ..BackendSettings::default()
    };
    store.save(&original).unwrap();
    std::fs::create_dir(temp.path().join("codex")).unwrap();
    std::fs::write(
        temp.path().join("codex").join("auth.json"),
        r#"{"OPENAI_API_KEY":"sk-a"}"#,
    )
    .unwrap();
    std::fs::write(
        temp.path().join("codex").join("config.toml"),
        r#"model_provider = "custom"

[model_providers.custom]
name = "custom"
wire_api = "responses"
requires_openai_auth = true
base_url = "https://a.example/v1"
"#,
    )
    .unwrap();
    let next = BackendSettings {
        active_relay_id: "b".to_string(),
        relay_profiles: vec![
            pure_profile("a", "https://a.example/v1", "sk-a"),
            RelayProfile {
                id: "b".to_string(),
                name: "B".to_string(),
                relay_mode: RelayMode::PureApi,
                config_contents: "model_provider = \"custom\"\n".to_string(),
                auth_contents: "{bad json".to_string(),
                ..RelayProfile::default()
            },
        ],
        ..BackendSettings::default()
    };

    let error = switch_relay_profile_in_home(&store, &temp.path().join("codex"), next, "a")
        .expect_err("invalid auth should fail switch");

    assert!(
        error.to_string().contains("auth.json"),
        "unexpected error: {error:#}"
    );
    assert_eq!(store.load().unwrap().active_relay_id, "a");
}

#[test]
fn switch_rejects_corrupt_settings_before_touching_live_config() {
    let temp = tempfile::tempdir().unwrap();
    let settings_path = temp.path().join("settings.json");
    let corrupt = b"{not valid settings";
    std::fs::write(&settings_path, corrupt).unwrap();
    let store = SettingsStore::new(settings_path.clone());
    let home = temp.path().join("codex");
    std::fs::create_dir(&home).unwrap();
    let config = b"model_provider = \"original\"\n";
    let auth = br#"{"OPENAI_API_KEY":"original"}"#;
    std::fs::write(home.join("config.toml"), config).unwrap();
    std::fs::write(home.join("auth.json"), auth).unwrap();
    let next = BackendSettings {
        active_relay_id: "next".to_string(),
        relay_profiles: vec![pure_profile("next", "https://next.example/v1", "sk-next")],
        ..BackendSettings::default()
    };

    let error = switch_relay_profile_in_home(&store, &home, next, "original")
        .expect_err("corrupt settings must block provider switch");

    assert!(error.to_string().contains("读取当前供应商设置失败"));
    assert!(error.to_string().contains("未改写 settings.json"));
    assert_eq!(std::fs::read(settings_path).unwrap(), corrupt);
    assert_eq!(std::fs::read(home.join("config.toml")).unwrap(), config);
    assert_eq!(std::fs::read(home.join("auth.json")).unwrap(), auth);
}

#[test]
fn switch_backfills_previous_profile_from_live_before_selecting_target() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("codex");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(
        home.join("config.toml"),
        r#"model = "edited-live-model"
model_provider = "manual_a"
model_context_window = 1000000
model_auto_compact_token_limit = 900000

[model_providers.manual_a]
name = "manual_a"
wire_api = "responses"
requires_openai_auth = true
base_url = "https://edited-a.example/v1"
"#,
    )
    .unwrap();
    std::fs::write(
        home.join("auth.json"),
        r#"{"OPENAI_API_KEY":"sk-edited-a"}"#,
    )
    .unwrap();
    let store = SettingsStore::new(temp.path().join("settings.json"));
    let original = BackendSettings {
        active_relay_id: "a".to_string(),
        relay_profiles: vec![
            pure_profile("a", "https://a.example/v1", "sk-a"),
            pure_profile("b", "https://b.example/v1", "sk-b"),
        ],
        ..BackendSettings::default()
    };
    store.save(&original).unwrap();
    let next = BackendSettings {
        active_relay_id: "b".to_string(),
        relay_profiles: original.relay_profiles.clone(),
        ..BackendSettings::default()
    };

    switch_relay_profile_in_home(&store, &home, next, "a").unwrap();

    let stored = store.load().unwrap();
    let previous = stored
        .relay_profiles
        .iter()
        .find(|profile| profile.id == "a")
        .unwrap();
    assert!(previous.config_contents.contains("edited-live-model"));
    assert!(previous.config_contents.contains("manual_a"));
    assert_eq!(previous.context_window, "1000000");
    assert_eq!(previous.auto_compact_limit, "900000");
    assert_eq!(stored.active_relay_id, "b");
    assert_eq!(stored.launch_mode, LaunchMode::Patch);
}

#[test]
fn switch_to_aggregate_relay_allows_empty_config_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("codex");
    std::fs::create_dir(&home).unwrap();
    let store = SettingsStore::new(temp.path().join("settings.json"));
    let api = pure_profile("api", "https://api.example/v1", "sk-api");
    let aggregate = RelayProfile {
        id: "agg".to_string(),
        name: "聚合供应商 1".to_string(),
        relay_mode: RelayMode::Aggregate,
        config_contents: String::new(),
        auth_contents: String::new(),
        ..RelayProfile::default()
    };
    let original = BackendSettings {
        active_relay_id: "api".to_string(),
        relay_profiles: vec![api.clone(), aggregate.clone()],
        ..BackendSettings::default()
    };
    store.save(&original).unwrap();
    let next = BackendSettings {
        active_relay_id: "agg".to_string(),
        relay_profiles: vec![api, aggregate],
        aggregate_relay_profiles: vec![AggregateRelayProfile {
            id: "agg".to_string(),
            name: "聚合供应商 1".to_string(),
            strategy: AggregateRelayStrategy::Failover,
            members: vec![AggregateRelayMember {
                relay_id: "api".to_string(),
                weight: 1,
            }],
        }],
        active_aggregate_relay_id: "agg".to_string(),
        ..BackendSettings::default()
    };

    let result = switch_relay_profile_in_home(&store, &home, next, "api").unwrap();
    let live = std::fs::read_to_string(home.join("config.toml")).unwrap();

    assert!(result.configured);
    assert_eq!(store.load().unwrap().active_relay_id, "agg");
    assert!(live.contains(r#"base_url = "http://127.0.0.1:57321/v1""#));
}

#[test]
fn switch_returns_normalized_previous_official_profile_after_backfill() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("codex");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(
        home.join("config.toml"),
        r#"model = "gpt-5.5"
model_reasoning_effort = "high"
model_provider = "custom"

[model_providers.custom]
name = "custom"
wire_api = "responses"
requires_openai_auth = true
base_url = "https://third-party.example/v1"

[features]
goals = true
"#,
    )
    .unwrap();
    std::fs::write(
        home.join("auth.json"),
        r#"{"OPENAI_API_KEY":"sk-third-party"}"#,
    )
    .unwrap();
    let store = SettingsStore::new(temp.path().join("settings.json"));
    let official = RelayProfile {
        id: "official".to_string(),
        name: "官方".to_string(),
        relay_mode: RelayMode::Official,
        official_mix_api_key: false,
        auth_contents: r#"{"auth_mode":"chatgpt","tokens":{"access_token":"official"}}"#
            .to_string(),
        ..RelayProfile::default()
    };
    let pure = pure_profile("api", "https://third-party.example/v1", "sk-third-party");
    let original = BackendSettings {
        active_relay_id: "official".to_string(),
        relay_profiles: vec![official.clone(), pure.clone()],
        ..BackendSettings::default()
    };
    store.save(&original).unwrap();
    let next = BackendSettings {
        active_relay_id: "api".to_string(),
        relay_profiles: vec![official, pure],
        ..BackendSettings::default()
    };

    let result = switch_relay_profile_in_home(&store, &home, next, "official").unwrap();
    let returned = result
        .settings
        .relay_profiles
        .iter()
        .find(|profile| profile.id == "official")
        .unwrap();

    assert_eq!(returned.relay_mode, RelayMode::Official);
    assert!(!returned.official_mix_api_key);
    assert!(returned.config_contents.is_empty());
    assert!(returned.api_key.is_empty());
}

#[tokio::test]
async fn verified_switch_rejects_failed_preflight_without_writing_any_file() {
    let server = spawn_responses_server(vec![MockResponse::unauthorized()]);
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("codex");
    std::fs::create_dir(&home).unwrap();
    let original_config = b"model_provider = \"original\"\n";
    let original_auth = br#"{"OPENAI_API_KEY":"original"}"#;
    std::fs::write(home.join("config.toml"), original_config).unwrap();
    std::fs::write(home.join("auth.json"), original_auth).unwrap();
    let store = SettingsStore::new(temp.path().join("settings.json"));
    let original = BackendSettings {
        active_relay_id: "a".to_string(),
        relay_profiles: vec![pure_profile("a", "https://a.example/v1", "sk-a")],
        ..BackendSettings::default()
    };
    store.save(&original).unwrap();
    let mut target = pure_profile("b", &server.base_url, "sk-b");
    target.test_model = "gpt-test".to_string();
    let next = BackendSettings {
        active_relay_id: "b".to_string(),
        relay_profiles: vec![original.relay_profiles[0].clone(), target],
        ..BackendSettings::default()
    };

    let error = switch_relay_profile_in_home_verified(&store, &home, next, "a")
        .await
        .expect_err("401 preflight must block switch");

    assert!(error.to_string().contains("写入前真实请求验证失败"));
    assert_eq!(
        std::fs::read(home.join("config.toml")).unwrap(),
        original_config
    );
    assert_eq!(
        std::fs::read(home.join("auth.json")).unwrap(),
        original_auth
    );
    assert_eq!(store.load().unwrap().active_relay_id, "a");
    assert_eq!(server.finish(), 1);
}

#[tokio::test]
async fn verified_switch_restores_all_files_when_post_write_probe_fails() {
    let server = spawn_responses_server(vec![
        MockResponse::completed(),
        MockResponse::unauthorized(),
    ]);
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("codex");
    std::fs::create_dir(&home).unwrap();
    let original_config = b"model = \"old-model\"\nmodel_provider = \"original\"\n";
    let original_auth = br#"{"OPENAI_API_KEY":"original"}"#;
    std::fs::write(home.join("config.toml"), original_config).unwrap();
    std::fs::write(home.join("auth.json"), original_auth).unwrap();
    let store = SettingsStore::new(temp.path().join("settings.json"));
    let original = BackendSettings {
        active_relay_id: "a".to_string(),
        relay_profiles: vec![pure_profile("a", "https://a.example/v1", "sk-a")],
        ..BackendSettings::default()
    };
    store.save(&original).unwrap();
    let original_settings = std::fs::read(store.path()).unwrap();
    let mut target = pure_profile("b", &server.base_url, "sk-b");
    target.test_model = "gpt-test".to_string();
    let next = BackendSettings {
        active_relay_id: "b".to_string(),
        relay_profiles: vec![original.relay_profiles[0].clone(), target],
        ..BackendSettings::default()
    };

    let error = switch_relay_profile_in_home_verified(&store, &home, next, "a")
        .await
        .expect_err("post-write 401 must roll back switch");

    assert!(
        error.to_string().contains("写入后真实请求复核失败"),
        "unexpected error: {error:#}"
    );
    assert!(
        error.to_string().contains("已恢复写入前状态"),
        "unexpected error: {error:#}"
    );
    assert_eq!(
        std::fs::read(home.join("config.toml")).unwrap(),
        original_config
    );
    assert_eq!(
        std::fs::read(home.join("auth.json")).unwrap(),
        original_auth
    );
    assert_eq!(std::fs::read(store.path()).unwrap(), original_settings);
    assert_eq!(store.load().unwrap().active_relay_id, "a");
    assert_eq!(server.finish(), 2);
}

fn pure_profile(id: &str, base_url: &str, key: &str) -> RelayProfile {
    RelayProfile {
        id: id.to_string(),
        name: id.to_uppercase(),
        relay_mode: RelayMode::PureApi,
        config_contents: format!(
            r#"model_provider = "custom"

[model_providers.custom]
name = "custom"
wire_api = "responses"
requires_openai_auth = true
base_url = "{base_url}"
"#
        ),
        auth_contents: format!(r#"{{"OPENAI_API_KEY":"{key}"}}"#),
        ..RelayProfile::default()
    }
}

#[derive(Clone)]
struct MockResponse {
    status: u16,
    reason: &'static str,
    body: &'static str,
}

impl MockResponse {
    fn completed() -> Self {
        Self {
            status: 200,
            reason: "OK",
            body: r#"{"status":"completed","output":[{"content":[{"type":"output_text","text":"OK"}]}]}"#,
        }
    }

    fn unauthorized() -> Self {
        Self {
            status: 401,
            reason: "Unauthorized",
            body: r#"{"error":{"message":"invalid key"}}"#,
        }
    }
}

struct ResponsesServer {
    base_url: String,
    handle: thread::JoinHandle<usize>,
}

impl ResponsesServer {
    fn finish(self) -> usize {
        self.handle.join().unwrap()
    }
}

fn spawn_responses_server(responses: Vec<MockResponse>) -> ResponsesServer {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let expected = responses.len();
    let handle = thread::spawn(move || {
        let started = std::time::Instant::now();
        let mut served = 0;
        while served < expected && started.elapsed() < std::time::Duration::from_secs(10) {
            let Ok((mut stream, _)) = listener.accept() else {
                thread::sleep(std::time::Duration::from_millis(10));
                continue;
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            let mut request_bytes = Vec::new();
            let mut buffer = [0u8; 8192];
            loop {
                let read = stream.read(&mut buffer).unwrap_or_default();
                if read == 0 {
                    break;
                }
                request_bytes.extend_from_slice(&buffer[..read]);
                let Some(header_end) = request_bytes
                    .windows(4)
                    .position(|part| part == b"\r\n\r\n")
                    .map(|position| position + 4)
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request_bytes[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or_default();
                if request_bytes.len() >= header_end + content_length {
                    break;
                }
            }
            if request_bytes.is_empty() {
                continue;
            }
            let request = String::from_utf8_lossy(&request_bytes);
            assert!(request.starts_with("POST /responses "));
            let response = &responses[served];
            let wire = format!(
                "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.status,
                response.reason,
                response.body.len(),
                response.body
            );
            stream.write_all(wire.as_bytes()).unwrap();
            served += 1;
        }
        served
    });
    ResponsesServer {
        base_url: format!("http://{address}"),
        handle,
    }
}
