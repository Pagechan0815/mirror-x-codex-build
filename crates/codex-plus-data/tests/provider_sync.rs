use codex_plus_data::{
    ProviderSyncStatus, ProviderSyncTargetSource, load_provider_sync_targets, run_provider_sync,
    run_provider_sync_with_target,
};
use rusqlite::Connection;
use serde_json::json;
use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};
use tempfile::tempdir;

static CODEX_HOME_ENV_LOCK: Mutex<()> = Mutex::new(());

struct CodexHomeEnvGuard {
    previous: Option<OsString>,
}

impl CodexHomeEnvGuard {
    fn set(path: &Path) -> Self {
        let previous = std::env::var_os("CODEX_HOME");
        unsafe {
            std::env::set_var("CODEX_HOME", path);
        }
        Self { previous }
    }
}

impl Drop for CodexHomeEnvGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.previous {
                Some(value) => std::env::set_var("CODEX_HOME", value),
                None => std::env::remove_var("CODEX_HOME"),
            }
        }
    }
}

fn write_rollout(path: &Path, provider: &str, thread_id: &str, cwd: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let first = json!({
        "type": "session_meta",
        "payload": {
            "id": thread_id,
            "model_provider": provider,
            "cwd": cwd
        }
    });
    let event = json!({"type": "event_msg", "payload": {"type": "user_message"}});
    fs::write(path, format!("{first}\n{event}\n")).unwrap();
}

fn write_rollout_with_providers(path: &Path, providers: &[&str], thread_id: &str, cwd: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut lines = Vec::new();
    for provider in providers {
        lines.push(
            json!({
                "type": "session_meta",
                "payload": {
                    "id": thread_id,
                    "model_provider": provider,
                    "cwd": cwd
                }
            })
            .to_string(),
        );
        lines.push(json!({"type": "event_msg", "payload": {"type": "task_started"}}).to_string());
    }
    lines.push(json!({"type": "event_msg", "payload": {"type": "user_message"}}).to_string());
    fs::write(path, format!("{}\n", lines.join("\n"))).unwrap();
}

fn write_rollout_with_source(
    path: &Path,
    provider: &str,
    thread_id: &str,
    cwd: &str,
    source: serde_json::Value,
) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let first = json!({
        "type": "session_meta",
        "payload": {
            "id": thread_id,
            "model_provider": provider,
            "cwd": cwd,
            "source": source
        }
    });
    let event = json!({"type": "event_msg", "payload": {"type": "user_message"}});
    fs::write(path, format!("{first}\n{event}\n")).unwrap();
}

fn create_state_db(path: &Path) {
    let db = Connection::open(path).unwrap();
    db.execute(
        "CREATE TABLE threads (id TEXT PRIMARY KEY, model_provider TEXT, archived INTEGER, has_user_event INTEGER, cwd TEXT)",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO threads VALUES ('thread-1', 'old-provider', 0, 0, 'C:/old')",
        [],
    )
    .unwrap();
}

fn create_state_db_with_providers(path: &Path, rows: &[(&str, &str, i64)]) {
    let db = Connection::open(path).unwrap();
    db.execute(
        "CREATE TABLE threads (id TEXT PRIMARY KEY, model_provider TEXT, archived INTEGER, has_user_event INTEGER, cwd TEXT)",
        [],
    )
    .unwrap();
    for (id, provider, archived) in rows {
        db.execute(
            "INSERT INTO threads VALUES (?1, ?2, ?3, 1, 'C:/workspace')",
            (id, provider, archived),
        )
        .unwrap();
    }
}

#[test]
fn provider_sync_targets_default_to_codex_home_env() {
    let _lock = CODEX_HOME_ENV_LOCK.lock().unwrap();
    let tmp = tempdir().unwrap();
    let home = tmp.path().join("custom-codex-home");
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join("config.toml"), "model_provider = \"custom\"\n").unwrap();
    let _guard = CodexHomeEnvGuard::set(&home);

    let targets = load_provider_sync_targets(None);

    assert_eq!(targets.current_provider, "custom");
    assert!(targets.targets.iter().any(|target| target.id == "custom"));
}

#[test]
fn provider_sync_targets_merge_config_rollout_sqlite_and_sort_current_first() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join(".codex");
    fs::create_dir(&home).unwrap();
    fs::write(
        home.join("config.toml"),
        r#"model_provider = "custom"

[model_providers.custom]
name = "custom"

[model_providers.apigather]
name = "apigather"
"#,
    )
    .unwrap();
    write_rollout(
        &home.join("sessions/2026/rollout-openai.jsonl"),
        "openai",
        "thread-openai",
        "C:/workspace/openai",
    );
    write_rollout(
        &home.join("archived_sessions/rollout-legacy.jsonl"),
        "legacy-provider",
        "thread-legacy",
        "C:/workspace/legacy",
    );
    create_state_db_with_providers(
        &home.join("state_5.sqlite"),
        &[
            ("thread-sqlite", "sqlite-provider", 0),
            ("thread-openai", "openai", 1),
        ],
    );

    let targets = load_provider_sync_targets(Some(&home));

    assert_eq!(targets.current_provider, "custom");
    let ids = targets
        .targets
        .iter()
        .map(|target| target.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        vec![
            "custom",
            "apigather",
            "legacy-provider",
            "openai",
            "sqlite-provider",
        ]
    );
    let custom = targets
        .targets
        .iter()
        .find(|target| target.id == "custom")
        .unwrap();
    assert!(custom.is_current_provider);
    assert!(custom.sources.contains(&ProviderSyncTargetSource::Config));
    let openai = targets
        .targets
        .iter()
        .find(|target| target.id == "openai")
        .unwrap();
    assert!(openai.sources.contains(&ProviderSyncTargetSource::Config));
    assert!(openai.sources.contains(&ProviderSyncTargetSource::Rollout));
    assert!(openai.sources.contains(&ProviderSyncTargetSource::Sqlite));
    let legacy = targets
        .targets
        .iter()
        .find(|target| target.id == "legacy-provider")
        .unwrap();
    assert_eq!(legacy.sources, vec![ProviderSyncTargetSource::Rollout]);
}

#[test]
fn provider_sync_maps_official_mixed_to_custom_provider_id() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join(".codex");
    fs::create_dir(&home).unwrap();
    fs::write(
        home.join("config.toml"),
        r#"model_provider = "custom"

[model_providers.custom]
name = "custom"
wire_api = "responses"
requires_openai_auth = true
base_url = "https://example.com/v1"
experimental_bearer_token = "sk-test"
"#,
    )
    .unwrap();
    let rollout = home.join("sessions/2026/rollout-official-mix.jsonl");
    write_rollout(&rollout, "openai", "thread-1", "C:/workspace");
    create_state_db(&home.join("state_5.sqlite"));

    let result = run_provider_sync(Some(&home));

    assert_eq!(result.status, ProviderSyncStatus::Synced);
    assert_eq!(result.target_provider, "custom");
    assert_eq!(result.changed_session_files, 1);
    assert_eq!(result.sqlite_provider_rows_updated, 1);
    let first: serde_json::Value = serde_json::from_str(
        fs::read_to_string(&rollout)
            .unwrap()
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(first["payload"]["model_provider"], "custom");
    let db = Connection::open(home.join("state_5.sqlite")).unwrap();
    let provider: String = db
        .query_row(
            "SELECT model_provider FROM threads WHERE id = 'thread-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(provider, "custom");
}

#[test]
fn provider_sync_rewrites_all_session_meta_model_providers() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join(".codex");
    fs::create_dir(&home).unwrap();
    fs::write(home.join("config.toml"), "model_provider = \"apigather\"\n").unwrap();
    let rollout = home.join("sessions/2026/rollout-multi-meta.jsonl");
    write_rollout_with_providers(
        &rollout,
        &["openai", "ccx", "mirrorplus"],
        "thread-1",
        "C:/workspace",
    );
    create_state_db(&home.join("state_5.sqlite"));

    let result = run_provider_sync(Some(&home));

    assert_eq!(result.status, ProviderSyncStatus::Synced);
    assert_eq!(result.target_provider, "apigather");
    assert_eq!(result.changed_session_files, 1);

    let providers = fs::read_to_string(&rollout)
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|record| record["type"] == "session_meta")
        .map(|record| {
            record["payload"]["model_provider"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect::<Vec<_>>();
    assert_eq!(providers, vec!["apigather", "apigather", "apigather"]);
}

#[test]
fn provider_sync_preserves_subagent_history_and_honors_explicit_user_threads() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join(".codex");
    fs::create_dir(&home).unwrap();
    fs::write(
        home.join("config.toml"),
        "model_provider = \"mirrorplus\"\n",
    )
    .unwrap();
    let root_rollout = home.join("sessions/2026/rollout-root.jsonl");
    let child_rollout = home.join("sessions/2026/rollout-child.jsonl");
    let promoted_rollout = home.join("sessions/2026/rollout-promoted.jsonl");
    let legacy_user_rollout = home.join("sessions/2026/rollout-legacy-user.jsonl");
    write_rollout(&root_rollout, "openai", "thread-root", "C:/workspace");
    write_rollout_with_source(
        &child_rollout,
        "openai",
        "thread-child",
        "C:/workspace",
        json!({"sub_agent": {"name": "reviewer"}}),
    );
    write_rollout_with_source(
        &promoted_rollout,
        "openai",
        "thread-promoted",
        "C:/workspace",
        json!({"sub_agent": {"name": "legacy-marker"}}),
    );
    write_rollout(
        &legacy_user_rollout,
        "openai",
        "thread-legacy-user",
        "C:/workspace",
    );

    let db_path = home.join("state_5.sqlite");
    let db = Connection::open(&db_path).unwrap();
    db.execute(
        "CREATE TABLE threads (id TEXT PRIMARY KEY, model_provider TEXT, archived INTEGER, has_user_event INTEGER, cwd TEXT, source TEXT, thread_source TEXT)",
        [],
    )
    .unwrap();
    for (id, source, thread_source) in [
        ("thread-root", "", "user"),
        (
            "thread-child",
            r#"{"sub_agent":{"name":"reviewer"}}"#,
            "subagent",
        ),
        (
            "thread-promoted",
            r#"{"sub_agent":{"name":"guardian"}}"#,
            "user",
        ),
        ("thread-legacy-user", "subagent_review", "user"),
    ] {
        db.execute(
            "INSERT INTO threads VALUES (?1, 'openai', 0, 0, 'C:/old', ?2, ?3)",
            (id, source, thread_source),
        )
        .unwrap();
    }
    db.execute(
        "CREATE TABLE local_thread_catalog (thread_id TEXT, model_provider TEXT, source_kind TEXT, thread_source TEXT)",
        [],
    )
    .unwrap();
    for (id, source_kind, thread_source) in [
        ("thread-root", "", "user"),
        (
            "thread-child",
            r#"{"sub_agent":{"name":"reviewer"}}"#,
            "subagent",
        ),
        (
            "thread-promoted",
            r#"{"sub_agent":{"name":"guardian"}}"#,
            "user",
        ),
        ("thread-legacy-user", "subagent_review", "user"),
    ] {
        db.execute(
            "INSERT INTO local_thread_catalog VALUES (?1, 'openai', ?2, ?3)",
            (id, source_kind, thread_source),
        )
        .unwrap();
    }
    db.execute(
        "CREATE TABLE thread_spawn_edges (parent_thread_id TEXT, child_thread_id TEXT)",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO thread_spawn_edges VALUES ('thread-root', 'thread-child')",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO thread_spawn_edges VALUES ('thread-root', 'thread-promoted')",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO thread_spawn_edges VALUES ('thread-root', 'thread-legacy-user')",
        [],
    )
    .unwrap();
    drop(db);

    let result = run_provider_sync(Some(&home));

    assert_eq!(result.status, ProviderSyncStatus::Synced);
    assert_eq!(result.changed_session_files, 2);
    let rollout_provider = |path: &Path| {
        let text = fs::read_to_string(path).unwrap();
        serde_json::from_str::<serde_json::Value>(text.lines().next().unwrap()).unwrap()["payload"]
            ["model_provider"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(rollout_provider(&root_rollout), "mirrorplus");
    assert_eq!(rollout_provider(&child_rollout), "openai");
    assert_eq!(rollout_provider(&promoted_rollout), "openai");
    assert_eq!(rollout_provider(&legacy_user_rollout), "mirrorplus");

    let db = Connection::open(&db_path).unwrap();
    let providers = [
        "thread-root",
        "thread-child",
        "thread-promoted",
        "thread-legacy-user",
    ]
    .into_iter()
    .map(|thread_id| {
        db.query_row(
            "SELECT model_provider FROM threads WHERE id = ?1",
            [thread_id],
            |row| row.get::<_, String>(0),
        )
        .unwrap()
    })
    .collect::<Vec<_>>();
    assert_eq!(
        providers,
        vec!["mirrorplus", "openai", "openai", "mirrorplus"]
    );
    let catalog_providers = [
        "thread-root",
        "thread-child",
        "thread-promoted",
        "thread-legacy-user",
    ]
    .into_iter()
    .map(|thread_id| {
        db.query_row(
            "SELECT model_provider FROM local_thread_catalog WHERE thread_id = ?1",
            [thread_id],
            |row| row.get::<_, String>(0),
        )
        .unwrap()
    })
    .collect::<Vec<_>>();
    assert_eq!(
        catalog_providers,
        vec!["mirrorplus", "openai", "openai", "mirrorplus"]
    );
}

#[test]
fn provider_sync_audits_catalog_only_sessions_without_claiming_recovery() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join(".codex");
    let sqlite_dir = home.join("sqlite");
    fs::create_dir_all(&sqlite_dir).unwrap();
    fs::write(
        home.join("config.toml"),
        "model_provider = \"mirrorplus\"\n",
    )
    .unwrap();

    let state_db = home.join("state_5.sqlite");
    create_state_db_with_providers(&state_db, &[("canonical", "openai", 0)]);
    let current_rollout_id = "01a01579-4a5d-77e3-89c0-751d38ad21f8";
    write_rollout(
        &home.join("sessions/2026/08/18").join(format!(
            "rollout-2026-08-18T23-24-25-{current_rollout_id}.jsonl"
        )),
        "custom",
        current_rollout_id,
        "C:/workspace",
    );

    let catalog_db = sqlite_dir.join("codex-dev.db");
    let db = Connection::open(&catalog_db).unwrap();
    db.execute("CREATE TABLE threads (id TEXT PRIMARY KEY)", [])
        .unwrap();
    db.execute(
        "CREATE TABLE local_thread_catalog (thread_id TEXT, model_provider TEXT, source_kind TEXT, thread_source TEXT)",
        [],
    )
    .unwrap();
    for (thread_id, source_kind, thread_source) in [
        (current_rollout_id, "", "user"),
        ("backup-only", "", "user"),
        ("no-source", "", "user"),
        ("agent-only", r#"{"subagent":{"name":"guardian"}}"#, "user"),
    ] {
        db.execute(
            "INSERT INTO local_thread_catalog VALUES (?1, 'custom', ?2, ?3)",
            (thread_id, source_kind, thread_source),
        )
        .unwrap();
    }
    drop(db);

    let backup_db = home.join("backups_state/provider-sync/20260818233010/db/state_5.sqlite");
    fs::create_dir_all(backup_db.parent().unwrap()).unwrap();
    create_state_db_with_providers(&backup_db, &[("backup-only", "custom", 0)]);
    fs::write(
        home.join("backups_state/provider-sync/20260818233010/db/broken.sqlite"),
        b"not a sqlite database",
    )
    .unwrap();

    let result = run_provider_sync(Some(&home));

    assert_eq!(result.status, ProviderSyncStatus::Synced);
    assert_eq!(result.repair_audit.catalog_only_sessions, 3);
    assert_eq!(result.repair_audit.catalog_only_with_current_rollout, 1);
    assert_eq!(result.repair_audit.catalog_only_with_backup_database, 1);
    assert_eq!(result.repair_audit.catalog_only_without_recovery_source, 1);
    assert!(result.message.contains("未自动重建缺失的 canonical 会话"));
}

#[test]
fn provider_sync_target_discovery_reads_all_session_meta_providers() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join(".codex");
    fs::create_dir(&home).unwrap();
    fs::write(home.join("config.toml"), "model_provider = \"custom\"\n").unwrap();
    write_rollout_with_providers(
        &home.join("sessions/2026/rollout-multi-meta.jsonl"),
        &["openai", "ccx", "mirrorplus"],
        "thread-1",
        "C:/workspace",
    );

    let targets = load_provider_sync_targets(Some(&home));
    let ids = targets
        .targets
        .iter()
        .map(|target| target.id.as_str())
        .collect::<Vec<_>>();

    assert!(ids.contains(&"openai"));
    assert!(ids.contains(&"ccx"));
    assert!(ids.contains(&"mirrorplus"));
}

#[test]
fn provider_sync_updates_rollout_sqlite_visibility_and_creates_backup() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join(".codex");
    fs::create_dir(&home).unwrap();
    fs::write(home.join("config.toml"), "model_provider = \"apigather\"\n").unwrap();
    let rollout = home.join("sessions/2026/rollout-abc.jsonl");
    write_rollout(&rollout, "openai", "thread-1", "C:/workspace");
    create_state_db(&home.join("state_5.sqlite"));

    let result = run_provider_sync(Some(&home));

    assert_eq!(result.status, ProviderSyncStatus::Synced);
    assert_eq!(result.target_provider, "apigather");
    assert_eq!(result.changed_session_files, 1);
    assert_eq!(result.sqlite_rows_updated, 3);
    assert_eq!(result.sqlite_provider_rows_updated, 1);
    assert_eq!(result.sqlite_user_event_rows_updated, 1);
    assert_eq!(result.sqlite_cwd_rows_updated, 1);
    let first: serde_json::Value = serde_json::from_str(
        fs::read_to_string(&rollout)
            .unwrap()
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(first["payload"]["model_provider"], "apigather");
    let db = Connection::open(home.join("state_5.sqlite")).unwrap();
    let row = db
        .query_row(
            "SELECT model_provider, has_user_event, cwd FROM threads WHERE id = 'thread-1'",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        row,
        ("apigather".to_string(), 1, "C:/workspace".to_string())
    );
    let backup_dir = result.backup_dir.unwrap();
    assert!(backup_dir.join("session-meta-backup.json").exists());
    assert!(backup_dir.join("db/state_5.sqlite").exists());
}

#[test]
fn provider_sync_updates_new_codex_sqlite_directory_db() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join(".codex");
    let sqlite_dir = home.join("sqlite");
    fs::create_dir_all(&sqlite_dir).unwrap();
    fs::write(home.join("config.toml"), "model_provider = \"apigather\"\n").unwrap();
    let rollout = home.join("sessions/2026/rollout-abc.jsonl");
    write_rollout(&rollout, "openai", "thread-1", "C:/workspace");
    let db_path = sqlite_dir.join("codex-dev.db");
    create_state_db(&db_path);

    let result = run_provider_sync(Some(&home));

    assert_eq!(result.status, ProviderSyncStatus::Synced);
    assert_eq!(result.sqlite_rows_updated, 3);
    let db = Connection::open(&db_path).unwrap();
    let row = db
        .query_row(
            "SELECT model_provider, has_user_event, cwd FROM threads WHERE id = 'thread-1'",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        row,
        ("apigather".to_string(), 1, "C:/workspace".to_string())
    );
    let backup_dir = result.backup_dir.unwrap();
    assert!(backup_dir.join("db/sqlite/codex-dev.db").exists());
}

#[test]
fn provider_sync_backup_metadata_contains_reference_fields_and_managed_marker() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join(".codex");
    fs::create_dir(&home).unwrap();
    fs::write(home.join("config.toml"), "model_provider = \"apigather\"\n").unwrap();
    write_rollout(
        &home.join("sessions/rollout-backup.jsonl"),
        "openai",
        "thread-1",
        "C:/workspace",
    );
    create_state_db(&home.join("state_5.sqlite"));

    let result = run_provider_sync(Some(&home));

    assert_eq!(result.status, ProviderSyncStatus::Synced);
    let backup_dir = result.backup_dir.unwrap();
    let metadata: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(backup_dir.join("metadata.json")).unwrap())
            .unwrap();
    assert_eq!(metadata["version"], 2);
    assert_eq!(metadata["namespace"], "provider-sync");
    assert_eq!(metadata["codexHome"], home.to_string_lossy().to_string());
    assert_eq!(metadata["targetProvider"], "apigather");
    assert_eq!(metadata["changedSessionFiles"], 1);
    assert_eq!(metadata["managedBy"], "mirror+ provider sync");
    assert!(metadata["createdAt"].as_str().unwrap().contains('T'));
    assert!(
        metadata["dbFiles"]
            .as_array()
            .unwrap()
            .contains(&json!("state_5.sqlite"))
    );
}

#[test]
fn provider_sync_explicit_target_overrides_config_without_switching_config() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join(".codex");
    fs::create_dir(&home).unwrap();
    fs::write(home.join("config.toml"), "model_provider = \"apigather\"\n").unwrap();
    let rollout = home.join("sessions/2026/rollout-target.jsonl");
    write_rollout(&rollout, "openai", "thread-1", "C:/workspace");
    create_state_db(&home.join("state_5.sqlite"));

    let result = run_provider_sync_with_target(Some(&home), Some("custom"));

    assert_eq!(result.status, ProviderSyncStatus::Synced);
    assert_eq!(result.target_provider, "custom");
    assert_eq!(
        fs::read_to_string(home.join("config.toml")).unwrap(),
        "model_provider = \"apigather\"\n"
    );
    let first: serde_json::Value = serde_json::from_str(
        fs::read_to_string(&rollout)
            .unwrap()
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(first["payload"]["model_provider"], "custom");
    let db = Connection::open(home.join("state_5.sqlite")).unwrap();
    let provider: String = db
        .query_row(
            "SELECT model_provider FROM threads WHERE id = 'thread-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(provider, "custom");
}

#[test]
fn provider_sync_rejects_invalid_explicit_target_before_writes() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join(".codex");
    fs::create_dir(&home).unwrap();
    fs::write(home.join("config.toml"), "model_provider = \"apigather\"\n").unwrap();
    let rollout = home.join("sessions/rollout-invalid-target.jsonl");
    write_rollout(&rollout, "openai", "thread-1", "C:/workspace");
    let original = fs::read_to_string(&rollout).unwrap();

    let result = run_provider_sync_with_target(Some(&home), Some("bad\nprovider"));

    assert_eq!(result.status, ProviderSyncStatus::Skipped);
    assert!(result.message.contains("Invalid provider sync target"));
    assert_eq!(fs::read_to_string(&rollout).unwrap(), original);
    assert!(result.backup_dir.is_none());
}

#[test]
fn provider_sync_repairs_sqlite_when_rollout_provider_matches_and_normalizes_paths() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join(".codex");
    fs::create_dir(&home).unwrap();
    fs::write(home.join("config.toml"), "model_provider = \"apigather\"\n").unwrap();
    write_rollout(
        &home.join("archived_sessions/rollout-current.jsonl"),
        "apigather",
        "thread-1",
        "\\\\?\\C:\\workspace",
    );
    create_state_db(&home.join("state_5.sqlite"));
    fs::write(
        home.join(".codex-global-state.json"),
        json!({
            "electron-saved-workspace-roots": ["\\\\?\\C:\\workspace"],
            "project-order": ["\\\\?\\C:\\workspace"],
            "active-workspace-roots": "\\\\?\\C:\\workspace",
            "electron-workspace-root-labels": {"\\\\?\\C:\\workspace": "Workspace"}
        })
        .to_string(),
    )
    .unwrap();

    let result = run_provider_sync(Some(&home));

    assert_eq!(result.status, ProviderSyncStatus::Synced);
    assert_eq!(result.changed_session_files, 0);
    assert_eq!(result.sqlite_rows_updated, 3);
    assert_eq!(result.sqlite_provider_rows_updated, 1);
    assert_eq!(result.sqlite_user_event_rows_updated, 1);
    assert_eq!(result.sqlite_cwd_rows_updated, 1);
    let db = Connection::open(home.join("state_5.sqlite")).unwrap();
    let row: String = db
        .query_row("SELECT cwd FROM threads WHERE id = 'thread-1'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(row, "C:/workspace");
    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(home.join(".codex-global-state.json")).unwrap())
            .unwrap();
    assert_eq!(
        state["electron-saved-workspace-roots"],
        json!(["C:/workspace"])
    );
    assert_eq!(state["project-order"], json!(["C:/workspace"]));
    assert_eq!(state["active-workspace-roots"], json!("C:/workspace"));
    assert_eq!(
        state["electron-workspace-root-labels"],
        json!({"C:/workspace": "Workspace"})
    );
}

#[test]
fn provider_sync_does_not_restore_cwd_for_projectless_threads() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join(".codex");
    fs::create_dir(&home).unwrap();
    fs::write(home.join("config.toml"), "model_provider = \"apigather\"\n").unwrap();
    write_rollout(
        &home.join("sessions/rollout-projectless.jsonl"),
        "apigather",
        "thread-1",
        "C:/old/project",
    );
    create_state_db(&home.join("state_5.sqlite"));
    fs::write(
        home.join(".codex-global-state.json"),
        json!({
            "projectless-thread-ids": ["thread-1"]
        })
        .to_string(),
    )
    .unwrap();

    let result = run_provider_sync(Some(&home));

    assert_eq!(result.status, ProviderSyncStatus::Synced);
    assert_eq!(result.sqlite_cwd_rows_updated, 0);
    let db = Connection::open(home.join("state_5.sqlite")).unwrap();
    let row: String = db
        .query_row("SELECT cwd FROM threads WHERE id = 'thread-1'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(row, "C:/old");
}

#[test]
fn provider_sync_normalizes_open_in_target_preferences_per_path() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join(".codex");
    fs::create_dir(&home).unwrap();
    fs::write(home.join("config.toml"), "model_provider = \"apigather\"\n").unwrap();
    write_rollout(
        &home.join("sessions/rollout-current.jsonl"),
        "apigather",
        "thread-1",
        "\\\\?\\C:\\workspace",
    );
    create_state_db(&home.join("state_5.sqlite"));
    fs::write(
        home.join(".codex-global-state.json"),
        json!({
            "electron-saved-workspace-roots": ["\\\\?\\C:\\workspace"],
            "project-order": ["\\\\?\\C:\\workspace"],
            "active-workspace-roots": ["\\\\?\\C:\\workspace"],
            "electron-workspace-root-labels": {"\\\\?\\C:\\workspace": "Workspace"},
            "open-in-target-preferences": {
                "perPath": {
                    "\\\\?\\C:\\workspace": "terminal"
                }
            }
        })
        .to_string(),
    )
    .unwrap();

    let result = run_provider_sync(Some(&home));

    assert_eq!(result.status, ProviderSyncStatus::Synced);
    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(home.join(".codex-global-state.json")).unwrap())
            .unwrap();
    assert_eq!(
        state["open-in-target-preferences"]["perPath"],
        json!({"C:/workspace": "terminal"})
    );
    assert!(home.join(".codex-global-state.json.bak").exists());
}

#[test]
fn provider_sync_restores_rollout_first_line_when_later_step_fails() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join(".codex");
    fs::create_dir(&home).unwrap();
    fs::write(home.join("config.toml"), "model_provider = \"apigather\"\n").unwrap();
    let rollout = home.join("sessions/rollout-needs-rewrite.jsonl");
    write_rollout(&rollout, "openai", "thread-1", "C:/workspace");
    let original_first_line = fs::read_to_string(&rollout)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_string();
    let db = Connection::open(home.join("state_5.sqlite")).unwrap();
    db.execute(
        "CREATE TABLE threads (id TEXT PRIMARY KEY, model_provider TEXT, archived INTEGER, has_user_event INTEGER, cwd TEXT)",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO threads VALUES ('thread-1', 'old-provider', 0, 0, 'C:/old')",
        [],
    )
    .unwrap();
    db.execute(
        "CREATE TRIGGER fail_provider_sync_update BEFORE UPDATE ON threads BEGIN SELECT RAISE(ABORT, 'boom'); END",
        [],
    )
    .unwrap();
    drop(db);

    let result = run_provider_sync(Some(&home));

    assert_eq!(result.status, ProviderSyncStatus::Skipped);
    assert!(result.message.contains("Provider sync skipped"));
    let restored_first_line = fs::read_to_string(&rollout)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_string();
    assert_eq!(restored_first_line, original_first_line);
}

#[test]
fn provider_sync_rolls_back_sqlite_provider_update_when_later_update_fails() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join(".codex");
    fs::create_dir(&home).unwrap();
    fs::write(home.join("config.toml"), "model_provider = \"apigather\"\n").unwrap();
    write_rollout(
        &home.join("sessions/rollout-current.jsonl"),
        "apigather",
        "thread-1",
        "C:/workspace",
    );
    let db = Connection::open(home.join("state_5.sqlite")).unwrap();
    db.execute(
        "CREATE TABLE threads (id TEXT PRIMARY KEY, model_provider TEXT, archived INTEGER, has_user_event INTEGER, cwd TEXT)",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO threads VALUES ('thread-1', 'old-provider', 0, 1, 'C:/old')",
        [],
    )
    .unwrap();
    db.execute(
        "CREATE TRIGGER fail_cwd_update BEFORE UPDATE OF cwd ON threads BEGIN SELECT RAISE(ABORT, 'boom'); END",
        [],
    )
    .unwrap();
    drop(db);

    let result = run_provider_sync(Some(&home));

    assert_eq!(result.status, ProviderSyncStatus::Skipped);
    let db = Connection::open(home.join("state_5.sqlite")).unwrap();
    let row = db
        .query_row(
            "SELECT model_provider, has_user_event, cwd FROM threads WHERE id = 'thread-1'",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(row, ("old-provider".to_string(), 1, "C:/old".to_string()));
}

#[test]
fn provider_sync_restores_global_state_when_later_step_fails() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join(".codex");
    fs::create_dir(&home).unwrap();
    fs::write(home.join("config.toml"), "model_provider = \"apigather\"\n").unwrap();
    write_rollout(
        &home.join("sessions/rollout-current.jsonl"),
        "apigather",
        "thread-1",
        "\\\\?\\C:\\workspace",
    );
    create_state_db(&home.join("state_5.sqlite"));
    let state_path = home.join(".codex-global-state.json");
    let original_state = json!({
        "electron-saved-workspace-roots": ["\\\\?\\C:\\workspace"],
        "project-order": ["\\\\?\\C:\\workspace"]
    })
    .to_string();
    fs::write(&state_path, &original_state).unwrap();
    fs::create_dir_all(home.join("backups_state/provider-sync/blocker")).unwrap();
    fs::write(
        home.join("backups_state/provider-sync/blocker/metadata.json"),
        json!({"managedBy": "mirror+ provider sync"}).to_string(),
    )
    .unwrap();

    let result = run_provider_sync_with_target(Some(&home), Some("bad/provider"));

    assert_eq!(result.status, ProviderSyncStatus::Skipped);
    assert_eq!(fs::read_to_string(&state_path).unwrap(), original_state);
}

#[test]
fn provider_sync_skips_when_home_missing_or_lock_exists_and_prunes_backups() {
    let tmp = tempdir().unwrap();
    let missing = tmp.path().join(".missing");
    let result = run_provider_sync(Some(&missing));
    assert_eq!(result.status, ProviderSyncStatus::Skipped);

    let home = tmp.path().join(".codex");
    fs::create_dir(&home).unwrap();
    fs::create_dir_all(home.join("tmp/provider-sync.lock")).unwrap();
    fs::write(home.join("config.toml"), "model_provider = \"apigather\"\n").unwrap();
    let result = run_provider_sync(Some(&home));
    assert_eq!(result.status, ProviderSyncStatus::Skipped);
    assert!(result.message.to_lowercase().contains("lock"));

    fs::remove_dir_all(home.join("tmp/provider-sync.lock")).unwrap();
    let backup_root = home.join("backups_state/provider-sync");
    for index in 0..6 {
        let backup = backup_root.join(format!("2000010100000{index}"));
        fs::create_dir_all(&backup).unwrap();
        fs::write(
            backup.join("metadata.json"),
            json!({"managedBy": "mirror+ provider sync"}).to_string(),
        )
        .unwrap();
    }
    write_rollout(
        &home.join("sessions/rollout-new.jsonl"),
        "openai",
        "thread-1",
        "C:/workspace",
    );
    let result = run_provider_sync(Some(&home));
    assert_eq!(result.status, ProviderSyncStatus::Synced);
    let backups = fs::read_dir(&backup_root)
        .unwrap()
        .filter(|entry| entry.as_ref().unwrap().path().is_dir())
        .count();
    assert_eq!(backups, 5);
}

#[test]
fn provider_sync_reclaims_stale_lock_from_dead_pid() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join(".codex");
    fs::create_dir(&home).unwrap();
    fs::write(home.join("config.toml"), "model_provider = \"custom\"\n").unwrap();
    write_rollout(
        &home.join("sessions/rollout-stale-lock.jsonl"),
        "openai",
        "thread-1",
        "C:/workspace",
    );
    create_state_db(&home.join("state_5.sqlite"));
    let lock_dir = home.join("tmp/provider-sync.lock");
    fs::create_dir_all(&lock_dir).unwrap();
    fs::write(
        lock_dir.join("owner.json"),
        json!({
            "pid": 99_999,
            "startedAt": 1
        })
        .to_string(),
    )
    .unwrap();

    let result = run_provider_sync(Some(&home));

    assert_eq!(result.status, ProviderSyncStatus::Synced);
    assert_eq!(result.changed_session_files, 1);
    assert!(!lock_dir.exists());
}

#[test]
fn provider_sync_preserves_rollout_mtime() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join(".codex");
    fs::create_dir(&home).unwrap();
    fs::write(home.join("config.toml"), "model_provider = \"apigather\"\n").unwrap();
    let rollout = home.join("sessions/2026/rollout-mtime.jsonl");
    write_rollout(&rollout, "openai", "thread-1", "C:/workspace");

    let past = SystemTime::now() - Duration::from_secs(86400);
    let file = fs::File::options().write(true).open(&rollout).unwrap();
    file.set_times(fs::FileTimes::new().set_modified(past))
        .unwrap();
    drop(file);

    let mtime_before = fs::metadata(&rollout).unwrap().modified().unwrap();

    let result = run_provider_sync(Some(&home));

    assert_eq!(result.status, ProviderSyncStatus::Synced);
    assert_eq!(result.changed_session_files, 1);

    let mtime_after = fs::metadata(&rollout).unwrap().modified().unwrap();
    let drift = mtime_after
        .duration_since(mtime_before)
        .or_else(|e| Ok::<_, std::convert::Infallible>(e.duration()))
        .unwrap();
    assert!(
        drift < Duration::from_secs(2),
        "mtime drifted by {drift:?}, expected < 2s"
    );
}

#[test]
fn provider_sync_preserves_crlf_invalid_lines_and_missing_final_newline() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join(".codex");
    fs::create_dir_all(home.join("sessions")).unwrap();
    fs::write(home.join("config.toml"), "model_provider = \"custom\"\n").unwrap();
    let rollout = home.join("sessions/rollout-line-endings.jsonl");
    let session = json!({
        "type": "session_meta",
        "payload": {
            "id": "thread-1",
            "model_provider": "openai",
            "cwd": "C:/workspace"
        }
    });
    let invalid = "this is intentionally not json";
    let final_event = json!({"type": "event_msg", "payload": {"type": "user_message"}});
    let final_event_text = final_event.to_string();
    fs::write(
        &rollout,
        format!("{invalid}\r\n{session}\r\n{final_event_text}"),
    )
    .unwrap();

    let result = run_provider_sync(Some(&home));

    assert_eq!(result.status, ProviderSyncStatus::Synced);
    let updated = fs::read_to_string(&rollout).unwrap();
    assert!(updated.starts_with(&format!("{invalid}\r\n")));
    assert!(updated.contains("\r\n"));
    assert!(!updated.ends_with('\n'));
    assert_eq!(updated.lines().last(), Some(final_event_text.as_str()));
    let session: serde_json::Value = serde_json::from_str(updated.lines().nth(1).unwrap()).unwrap();
    assert_eq!(session["payload"]["model_provider"], "custom");
}

#[test]
fn provider_sync_streams_past_a_large_non_metadata_line() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join(".codex");
    fs::create_dir_all(home.join("sessions")).unwrap();
    fs::write(home.join("config.toml"), "model_provider = \"custom\"\n").unwrap();
    let rollout = home.join("sessions/rollout-large-line.jsonl");
    let session = json!({
        "type": "session_meta",
        "payload": {
            "id": "thread-large",
            "model_provider": "openai",
            "cwd": "C:/workspace"
        }
    });
    let large_line = format!(
        "{{\"type\":\"event_msg\",\"payload\":{{\"blob\":\"{}\"}}}}",
        "x".repeat(9 * 1024 * 1024)
    );
    let original_large_line = large_line.as_bytes().to_vec();
    fs::write(&rollout, format!("{session}\n{large_line}\n")).unwrap();

    let result = run_provider_sync(Some(&home));

    assert_eq!(result.status, ProviderSyncStatus::Synced);
    let updated = fs::read(&rollout).unwrap();
    let mut records = updated.split(|byte| *byte == b'\n');
    let rewritten_session: serde_json::Value =
        serde_json::from_slice(records.next().unwrap()).unwrap();
    assert_eq!(rewritten_session["payload"]["model_provider"], "custom");
    assert_eq!(records.next().unwrap(), original_large_line);
    assert_eq!(records.next(), Some(&[][..]));
    assert_eq!(records.next(), None);
}

#[cfg(windows)]
#[test]
fn provider_sync_reports_an_exclusively_locked_rollout_as_partial() {
    use std::os::windows::fs::OpenOptionsExt;

    let tmp = tempdir().unwrap();
    let home = tmp.path().join(".codex");
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join("config.toml"), "model_provider = \"custom\"\n").unwrap();
    let rollout = home.join("sessions/rollout-locked.jsonl");
    write_rollout(&rollout, "openai", "thread-locked", "C:/workspace");
    let original = fs::read(&rollout).unwrap();
    let exclusive = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&rollout)
        .unwrap();

    let result = run_provider_sync(Some(&home));

    assert_eq!(result.status, ProviderSyncStatus::Partial);
    assert_eq!(result.skipped_locked_rollout_files, vec![rollout.clone()]);
    drop(exclusive);
    assert_eq!(fs::read(&rollout).unwrap(), original);
}
