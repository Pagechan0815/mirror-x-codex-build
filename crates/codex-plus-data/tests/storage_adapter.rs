use codex_plus_core::models::{DeleteStatus, SessionRef};
use codex_plus_data::{
    BackupStore, SQLiteStorageAdapter, delete_local_from_paths,
    move_codex_thread_workspace_from_paths,
};
use fs2::FileExt;
use rusqlite::Connection;
use serde_json::json;
use std::fs::{self, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use tempfile::tempdir;

fn session(id: &str, title: &str) -> SessionRef {
    SessionRef::new(id, title).unwrap()
}

fn create_supported_db(path: &Path) {
    let db = Connection::open(path).unwrap();
    db.execute(
        "CREATE TABLE sessions (id TEXT PRIMARY KEY, title TEXT NOT NULL)",
        [],
    )
    .unwrap();
    db.execute(
        "CREATE TABLE messages (id INTEGER PRIMARY KEY, session_id TEXT NOT NULL, body TEXT NOT NULL)",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO sessions (id, title) VALUES ('s1', 'First')",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO messages (session_id, body) VALUES ('s1', 'hello')",
        [],
    )
    .unwrap();
}

fn create_codex_thread_db(path: &Path, rollout_path: &Path) {
    let db = Connection::open(path).unwrap();
    db.execute("CREATE TABLE threads (id TEXT PRIMARY KEY, rollout_path TEXT, title TEXT, cwd TEXT, archived INTEGER, archived_at INTEGER, updated_at INTEGER, updated_at_ms INTEGER)", []).unwrap();
    db.execute(
        "CREATE TABLE thread_dynamic_tools (thread_id TEXT NOT NULL, tool_name TEXT NOT NULL)",
        [],
    )
    .unwrap();
    db.execute(
        "CREATE TABLE thread_goals (thread_id TEXT NOT NULL, goal TEXT NOT NULL)",
        [],
    )
    .unwrap();
    db.execute("CREATE TABLE thread_spawn_edges (parent_thread_id TEXT NOT NULL, child_thread_id TEXT NOT NULL, status TEXT NOT NULL)", []).unwrap();
    db.execute(
        "CREATE TABLE stage1_outputs (thread_id TEXT NOT NULL, output TEXT NOT NULL)",
        [],
    )
    .unwrap();
    db.execute(
        "CREATE TABLE agent_job_items (id TEXT PRIMARY KEY, assigned_thread_id TEXT)",
        [],
    )
    .unwrap();
    db.execute("INSERT INTO threads (id, rollout_path, title, cwd, archived, archived_at, updated_at, updated_at_ms) VALUES ('t1', ?1, 'Codex Thread', '/old/project', 0, NULL, 100, 100000)", [rollout_path.to_string_lossy().to_string()]).unwrap();
    db.execute(
        "INSERT INTO thread_dynamic_tools (thread_id, tool_name) VALUES ('t1', 'Read')",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO thread_goals (thread_id, goal) VALUES ('t1', 'delete me')",
        [],
    )
    .unwrap();
    db.execute("INSERT INTO thread_spawn_edges (parent_thread_id, child_thread_id, status) VALUES ('t1', 'child', 'running')", []).unwrap();
    db.execute("INSERT INTO thread_spawn_edges (parent_thread_id, child_thread_id, status) VALUES ('parent', 't1', 'done')", []).unwrap();
    db.execute(
        "INSERT INTO stage1_outputs (thread_id, output) VALUES ('t1', 'cached')",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO agent_job_items (id, assigned_thread_id) VALUES ('job1', 't1')",
        [],
    )
    .unwrap();
}

fn thread_count(path: &Path, id: &str) -> i64 {
    let db = Connection::open(path).unwrap();
    db.query_row("SELECT COUNT(*) FROM threads WHERE id = ?1", [id], |row| {
        row.get::<_, i64>(0)
    })
    .unwrap()
}

#[test]
fn backup_store_writes_reads_and_sanitizes_tokens() {
    let tmp = tempdir().unwrap();
    let store = BackupStore::new(tmp.path());

    let token = store
        .write_backup(
            "s1",
            Path::new("C:/state/codex.sqlite"),
            json!({"sessions": [{"id": "s1", "title": "Hello"}]}),
        )
        .unwrap();
    let backup = store.read_backup(&token).unwrap();

    assert_eq!(backup["session_id"], "s1");
    assert_eq!(backup["source_db"], "C:/state/codex.sqlite");
    assert_eq!(backup["tables"]["sessions"][0]["title"], "Hello");
    assert_eq!(
        store.path_for("../bad token!").file_name().unwrap(),
        "badtoken.json"
    );
    assert!(store.read_backup("missing").is_err());
}

#[test]
fn delete_local_session_creates_backup_and_undo_restores_rows() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("codex.sqlite");
    create_supported_db(&db_path);
    let adapter = SQLiteStorageAdapter::new(&db_path, BackupStore::new(tmp.path().join("backups")));

    let deleted = adapter.delete_local(&session("s1", "First"));

    assert_eq!(deleted.status, DeleteStatus::LocalDeleted);
    assert_eq!(deleted.message, "已从本地存储删除");
    let db = Connection::open(&db_path).unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM sessions", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM messages", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(db);

    let restored = adapter.undo(deleted.undo_token.as_deref().unwrap());

    assert_eq!(restored.status, DeleteStatus::Undone);
    let db = Connection::open(&db_path).unwrap();
    assert_eq!(
        db.query_row("SELECT title FROM sessions WHERE id = 's1'", [], |row| {
            row.get::<_, String>(0)
        })
        .unwrap(),
        "First"
    );
    assert_eq!(
        db.query_row(
            "SELECT body FROM messages WHERE session_id = 's1'",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "hello"
    );
}

#[test]
fn undo_fails_on_existing_db_row_conflict_without_overwriting_new_row() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("codex.sqlite");
    create_supported_db(&db_path);
    let adapter = SQLiteStorageAdapter::new(&db_path, BackupStore::new(tmp.path().join("backups")));
    let deleted = adapter.delete_local(&session("s1", "First"));
    let token = deleted.undo_token.as_deref().unwrap();
    let db = Connection::open(&db_path).unwrap();
    db.execute(
        "INSERT INTO sessions (id, title) VALUES ('s1', 'New Session')",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO messages (session_id, body) VALUES ('s1', 'new body')",
        [],
    )
    .unwrap();
    drop(db);

    let restored = adapter.undo(token);

    assert_eq!(restored.status, DeleteStatus::Failed);
    assert_eq!(restored.undo_token.as_deref(), Some(token));
    assert!(restored.message.to_lowercase().contains("restore conflict"));
    let db = Connection::open(&db_path).unwrap();
    assert_eq!(
        db.query_row("SELECT title FROM sessions WHERE id = 's1'", [], |row| {
            row.get::<_, String>(0)
        })
        .unwrap(),
        "New Session"
    );
    assert_eq!(
        db.query_row(
            "SELECT body FROM messages WHERE session_id = 's1'",
            [],
            |row| { row.get::<_, String>(0) }
        )
        .unwrap(),
        "new body"
    );
}

#[test]
fn undo_fails_on_existing_rollout_file_conflict_without_overwriting_new_file() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("state_5.sqlite");
    let rollout_path = tmp.path().join("rollout.jsonl");
    fs::write(&rollout_path, "old rollout\n").unwrap();
    create_codex_thread_db(&db_path, &rollout_path);
    let adapter = SQLiteStorageAdapter::new(&db_path, BackupStore::new(tmp.path().join("backups")))
        .with_codex_home(tmp.path());
    let deleted = adapter.delete_local(&session("t1", "Codex Thread"));
    let token = deleted.undo_token.as_deref().unwrap();
    fs::write(&rollout_path, "new rollout\n").unwrap();

    let restored = adapter.undo(token);

    assert_eq!(restored.status, DeleteStatus::Failed);
    assert_eq!(restored.undo_token.as_deref(), Some(token));
    assert!(restored.message.to_lowercase().contains("restore conflict"));
    assert_eq!(fs::read_to_string(&rollout_path).unwrap(), "new rollout\n");
    let db = Connection::open(&db_path).unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM threads WHERE id = 't1'", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        0
    );
}

#[test]
fn large_rollout_delete_uses_streaming_sidecar_and_undo_restores() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("state_5.sqlite");
    let rollout_path = tmp.path().join("large-rollout.jsonl");
    let original = vec![b'x'; 8 * 1024 * 1024];
    fs::write(&rollout_path, &original).unwrap();
    create_codex_thread_db(&db_path, &rollout_path);
    let backup_store = BackupStore::new(tmp.path().join("backups"));
    let adapter =
        SQLiteStorageAdapter::new(&db_path, backup_store.clone()).with_codex_home(tmp.path());

    let deleted = adapter.delete_local(&session("t1", "Codex Thread"));

    assert_eq!(deleted.status, DeleteStatus::LocalDeleted);
    assert!(!rollout_path.exists());
    let token = deleted.undo_token.as_deref().unwrap();
    let backup_path = backup_store.path_for(token);
    assert!(fs::metadata(&backup_path).unwrap().len() < 64 * 1024);
    let backup = backup_store.read_backup(token).unwrap();
    let file = &backup["tables"]["__files"][0];
    assert!(file.get("content_b64").is_none());
    assert_eq!(file["size"].as_u64(), Some(original.len() as u64));
    let sidecar = tmp
        .path()
        .join("backups")
        .join(format!("{token}.files"))
        .join(file["sidecar"].as_str().unwrap());
    assert_eq!(fs::metadata(sidecar).unwrap().len(), original.len() as u64);

    let restored = adapter.undo(token);

    assert_eq!(restored.status, DeleteStatus::Undone);
    assert_eq!(fs::read(&rollout_path).unwrap(), original);
    assert_eq!(thread_count(&db_path, "t1"), 1);
}

#[test]
fn tampered_rollout_sidecar_blocks_undo_before_database_restore() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("state_5.sqlite");
    let rollout_path = tmp.path().join("rollout.jsonl");
    fs::write(&rollout_path, "original rollout\n").unwrap();
    create_codex_thread_db(&db_path, &rollout_path);
    let backup_store = BackupStore::new(tmp.path().join("backups"));
    let adapter =
        SQLiteStorageAdapter::new(&db_path, backup_store.clone()).with_codex_home(tmp.path());
    let deleted = adapter.delete_local(&session("t1", "Codex Thread"));
    let token = deleted.undo_token.as_deref().unwrap();
    let backup = backup_store.read_backup(token).unwrap();
    let sidecar = tmp
        .path()
        .join("backups")
        .join(format!("{token}.files"))
        .join(backup["tables"]["__files"][0]["sidecar"].as_str().unwrap());
    OpenOptions::new()
        .append(true)
        .open(sidecar)
        .unwrap()
        .write_all(b"tampered")
        .unwrap();

    let restored = adapter.undo(token);

    assert_eq!(restored.status, DeleteStatus::Failed);
    assert!(restored.message.contains("sidecar"));
    assert_eq!(thread_count(&db_path, "t1"), 0);
    assert!(!rollout_path.exists());
}

#[test]
fn unreadable_rollout_blocks_delete_before_database_changes() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("state_5.sqlite");
    let rollout_path = tmp.path().join("rollout.jsonl");
    fs::create_dir(&rollout_path).unwrap();
    create_codex_thread_db(&db_path, &rollout_path);
    let adapter = SQLiteStorageAdapter::new(&db_path, BackupStore::new(tmp.path().join("backups")))
        .with_codex_home(tmp.path());

    let deleted = adapter.delete_local(&session("t1", "Codex Thread"));

    assert_eq!(deleted.status, DeleteStatus::Failed);
    assert_eq!(thread_count(&db_path, "t1"), 1);
    assert!(rollout_path.is_dir());
}

#[test]
fn locked_rollout_blocks_delete_before_database_changes() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("state_5.sqlite");
    let rollout_path = tmp.path().join("rollout.jsonl");
    let original = b"active rollout\n";
    fs::write(&rollout_path, original).unwrap();
    create_codex_thread_db(&db_path, &rollout_path);
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&rollout_path)
        .unwrap();
    lock.lock_exclusive().unwrap();
    let adapter = SQLiteStorageAdapter::new(&db_path, BackupStore::new(tmp.path().join("backups")))
        .with_codex_home(tmp.path());

    let deleted = adapter.delete_local(&session("t1", "Codex Thread"));

    assert_eq!(deleted.status, DeleteStatus::Failed);
    assert_eq!(thread_count(&db_path, "t1"), 1);
    FileExt::unlock(&lock).unwrap();
    assert_eq!(fs::read(&rollout_path).unwrap(), original);
}

#[test]
fn undo_fails_for_unknown_backup_table_without_executing_it() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("codex.sqlite");
    create_supported_db(&db_path);
    let backup_store = BackupStore::new(tmp.path().join("backups"));
    let adapter = SQLiteStorageAdapter::new(&db_path, backup_store.clone());
    let deleted = adapter.delete_local(&session("s1", "First"));
    let token = deleted.undo_token.as_deref().unwrap();
    let backup_path = backup_store.path_for(token);
    let mut backup: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&backup_path).unwrap()).unwrap();
    backup["tables"]["evil_table"] = json!([{"id": "owned"}]);
    fs::write(&backup_path, serde_json::to_string_pretty(&backup).unwrap()).unwrap();

    let restored = adapter.undo(token);

    assert_eq!(restored.status, DeleteStatus::Failed);
    assert_eq!(restored.undo_token.as_deref(), Some(token));
    assert!(
        restored
            .message
            .to_lowercase()
            .contains("unknown restore table")
    );
    let db = Connection::open(&db_path).unwrap();
    let table_exists = db
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'evil_table'",
            [],
            |_| Ok(()),
        )
        .is_ok();
    assert!(!table_exists);
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM sessions WHERE id = 's1'", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        0
    );
}

#[test]
fn undo_rejects_backup_file_paths_outside_thread_rollouts() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("state_5.sqlite");
    let rollout_path = tmp.path().join("rollout.jsonl");
    let outside_path = tmp.path().join("outside.txt");
    fs::write(&rollout_path, "{\"type\":\"message\"}\n").unwrap();
    create_codex_thread_db(&db_path, &rollout_path);
    let backup_store = BackupStore::new(tmp.path().join("backups"));
    let adapter = SQLiteStorageAdapter::new(&db_path, backup_store.clone());
    let deleted = adapter.delete_local(&session("t1", "Codex Thread"));
    let token = deleted.undo_token.as_deref().unwrap();
    let backup_path = backup_store.path_for(token);
    let mut backup: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&backup_path).unwrap()).unwrap();
    backup["tables"]["__files"] = json!([{
        "path": outside_path.to_string_lossy().to_string(),
        "content_b64": "b3duZWQ="
    }]);
    fs::write(&backup_path, serde_json::to_string_pretty(&backup).unwrap()).unwrap();

    let restored = adapter.undo(token);

    assert_eq!(restored.status, DeleteStatus::Failed);
    assert_eq!(restored.undo_token.as_deref(), Some(token));
    assert!(
        restored
            .message
            .to_lowercase()
            .contains("unexpected backup file path")
    );
    assert!(!outside_path.exists());
    let db = Connection::open(&db_path).unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM threads WHERE id = 't1'", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        0
    );
}

#[test]
fn undo_rejects_tampered_rollout_path_outside_codex_home() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join("codex-home");
    fs::create_dir_all(&home).unwrap();
    let db_path = home.join("state_5.sqlite");
    let rollout_path = home.join("rollout.jsonl");
    let outside_path = tmp.path().join("outside.jsonl");
    fs::write(&rollout_path, "{\"type\":\"message\"}\n").unwrap();
    create_codex_thread_db(&db_path, &rollout_path);
    let backup_store = BackupStore::new(home.join("backups"));
    let adapter = SQLiteStorageAdapter::new(&db_path, backup_store.clone()).with_codex_home(&home);
    let deleted = adapter.delete_local(&session("t1", "Codex Thread"));
    let token = deleted.undo_token.as_deref().unwrap();
    let backup_path = backup_store.path_for(token);
    let mut backup: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&backup_path).unwrap()).unwrap();
    backup["tables"]["threads"][0]["rollout_path"] =
        json!(outside_path.to_string_lossy().to_string());
    backup["tables"]["__files"][0]["path"] = json!(outside_path.to_string_lossy().to_string());
    fs::write(&backup_path, serde_json::to_string_pretty(&backup).unwrap()).unwrap();

    let restored = adapter.undo(token);

    assert_eq!(restored.status, DeleteStatus::Failed);
    assert_eq!(restored.undo_token.as_deref(), Some(token));
    assert!(restored.message.contains("outside the resolved Codex home"));
    assert_eq!(thread_count(&db_path, "t1"), 0);
    assert!(!outside_path.exists());
}

#[test]
fn generic_delete_rolls_back_when_later_delete_fails() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("codex.sqlite");
    create_supported_db(&db_path);
    let db = Connection::open(&db_path).unwrap();
    db.execute(
        "CREATE TRIGGER fail_session_delete BEFORE DELETE ON sessions BEGIN SELECT RAISE(ABORT, 'boom'); END",
        [],
    )
    .unwrap();
    drop(db);
    let adapter = SQLiteStorageAdapter::new(&db_path, BackupStore::new(tmp.path().join("backups")));

    let result = adapter.delete_local(&session("s1", "First"));

    assert_eq!(result.status, DeleteStatus::Failed);
    assert!(result.undo_token.is_some());
    assert!(result.backup_path.is_some());
    let db = Connection::open(&db_path).unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM sessions WHERE id = 's1'", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id = 's1'",
            [],
            |row| { row.get::<_, i64>(0) }
        )
        .unwrap(),
        1
    );
}

#[test]
fn delete_codex_thread_schema_removes_related_rows_file_and_undo_restores_everything() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("state_5.sqlite");
    let rollout_path = tmp.path().join("rollout.jsonl");
    fs::write(&rollout_path, "{\"type\":\"message\"}\n").unwrap();
    create_codex_thread_db(&db_path, &rollout_path);
    let adapter = SQLiteStorageAdapter::new(&db_path, BackupStore::new(tmp.path().join("backups")))
        .with_codex_home(tmp.path());

    let deleted = adapter.delete_local(&session("local:t1", "Codex Thread"));

    assert_eq!(deleted.status, DeleteStatus::LocalDeleted);
    assert!(!rollout_path.exists());
    let db = Connection::open(&db_path).unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM threads WHERE id = 't1'", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT assigned_thread_id FROM agent_job_items WHERE id = 'job1'",
            [],
            |row| row.get::<_, Option<String>>(0)
        )
        .unwrap(),
        None
    );
    drop(db);

    let restored = adapter.undo(deleted.undo_token.as_deref().unwrap());

    assert_eq!(restored.status, DeleteStatus::Undone);
    assert_eq!(
        fs::read_to_string(&rollout_path).unwrap(),
        "{\"type\":\"message\"}\n"
    );
    let db = Connection::open(&db_path).unwrap();
    assert_eq!(
        db.query_row("SELECT title FROM threads WHERE id = 't1'", [], |row| {
            row.get::<_, String>(0)
        })
        .unwrap(),
        "Codex Thread"
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM thread_spawn_edges WHERE parent_thread_id = 't1' OR child_thread_id = 't1'", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        db.query_row(
            "SELECT assigned_thread_id FROM agent_job_items WHERE id = 'job1'",
            [],
            |row| row.get::<_, Option<String>>(0)
        )
        .unwrap(),
        Some("t1".to_string())
    );
}

#[test]
fn delete_local_from_paths_removes_duplicate_threads_from_all_databases() {
    let tmp = tempdir().unwrap();
    let first_db = tmp.path().join("first.sqlite");
    let second_db = tmp.path().join("second.sqlite");
    let first_rollout = tmp.path().join("first.jsonl");
    let second_rollout = tmp.path().join("second.jsonl");
    fs::write(&first_rollout, "{\"type\":\"message\"}\n").unwrap();
    fs::write(&second_rollout, "{\"type\":\"message\"}\n").unwrap();
    create_codex_thread_db(&first_db, &first_rollout);
    create_codex_thread_db(&second_db, &second_rollout);

    let result = delete_local_from_paths(
        vec![first_db.clone(), second_db.clone()],
        BackupStore::new(tmp.path().join("backups")),
        &session("t1", "Codex Thread"),
        None,
    );

    assert_eq!(result.status, DeleteStatus::LocalDeleted);
    assert_eq!(result.message, "已从 2 个本地存储删除");
    assert_eq!(thread_count(&first_db, "t1"), 0);
    assert_eq!(thread_count(&second_db, "t1"), 0);
    assert!(!first_rollout.exists());
    assert!(!second_rollout.exists());
}

#[test]
fn delete_local_from_paths_deduplicates_equivalent_database_paths() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("state_5.sqlite");
    let equivalent_path = tmp.path().join(".").join("state_5.sqlite");
    let rollout_path = tmp.path().join("rollout.jsonl");
    fs::write(&rollout_path, "session\n").unwrap();
    create_codex_thread_db(&db_path, &rollout_path);

    let result = delete_local_from_paths(
        vec![db_path.clone(), equivalent_path],
        BackupStore::new(tmp.path().join("backups")),
        &session("t1", "Codex Thread"),
        Some(tmp.path()),
    );

    assert_eq!(result.status, DeleteStatus::LocalDeleted);
    assert!(!result.undo_token.as_deref().unwrap().starts_with('['));
    assert_eq!(thread_count(&db_path, "t1"), 0);
    assert!(!rollout_path.exists());
}

#[test]
fn delete_codex_thread_removes_session_index_entry_and_undo_restores_it() {
    let tmp = tempdir().unwrap();
    let home = tmp.path();
    let db_path = home.join("state_5.sqlite");
    let rollout_path = home.join("rollout.jsonl");
    let index_path = home.join("session_index.jsonl");
    fs::write(&rollout_path, "{\"type\":\"message\"}\n").unwrap();
    create_codex_thread_db(&db_path, &rollout_path);
    fs::write(
        &index_path,
        concat!(
            "{\"id\":\"t1\",\"thread_name\":\"Codex Thread\"}\n",
            "{\"id\":\"other\",\"thread_name\":\"Keep me\"}\n"
        ),
    )
    .unwrap();
    let adapter = SQLiteStorageAdapter::new(&db_path, BackupStore::new(home.join("backups")))
        .with_codex_home(home);

    let deleted = adapter.delete_local(&session("local:t1", "Codex Thread"));

    assert_eq!(deleted.status, DeleteStatus::LocalDeleted);
    let index_text = fs::read_to_string(&index_path).unwrap();
    assert!(!index_text.contains("\"id\":\"t1\""));
    assert!(index_text.contains("\"id\":\"other\""));
    assert_eq!(thread_count(&db_path, "t1"), 0);

    let restored = adapter.undo(deleted.undo_token.as_deref().unwrap());

    assert_eq!(restored.status, DeleteStatus::Undone);
    let index_text = fs::read_to_string(&index_path).unwrap();
    assert_eq!(index_text.matches("\"id\":\"t1\"").count(), 1);
    assert_eq!(index_text.matches("\"id\":\"other\"").count(), 1);
    assert_eq!(thread_count(&db_path, "t1"), 1);
    assert!(rollout_path.exists());
}

#[test]
fn delete_codex_thread_sqlite_dir_layout_uses_root_session_index() {
    let tmp = tempdir().unwrap();
    let home = tmp.path();
    let sqlite_dir = home.join("sqlite");
    fs::create_dir_all(&sqlite_dir).unwrap();
    let db_path = sqlite_dir.join("codex-dev.db");
    let rollout_path = home.join("rollout.jsonl");
    let index_path = home.join("session_index.jsonl");
    fs::write(&rollout_path, "{\"type\":\"message\"}\n").unwrap();
    fs::write(
        &index_path,
        "{\"id\":\"t1\",\"thread_name\":\"Codex Thread\"}\n",
    )
    .unwrap();
    create_codex_thread_db(&db_path, &rollout_path);
    let adapter = SQLiteStorageAdapter::new(&db_path, BackupStore::new(home.join("backups")))
        .with_codex_home(home);

    let deleted = adapter.delete_local(&session("t1", "Codex Thread"));

    assert_eq!(deleted.status, DeleteStatus::LocalDeleted);
    assert!(
        !fs::read_to_string(&index_path)
            .unwrap()
            .contains("\"id\":\"t1\"")
    );
    let restored = adapter.undo(deleted.undo_token.as_deref().unwrap());
    assert_eq!(restored.status, DeleteStatus::Undone);
    assert!(
        fs::read_to_string(&index_path)
            .unwrap()
            .contains("\"id\":\"t1\"")
    );
    assert_eq!(thread_count(&db_path, "t1"), 1);
}

#[test]
fn invalid_session_index_blocks_delete_before_any_user_data_changes() {
    let tmp = tempdir().unwrap();
    let home = tmp.path();
    let db_path = home.join("state_5.sqlite");
    let rollout_path = home.join("rollout.jsonl");
    fs::write(&rollout_path, "{\"type\":\"message\"}\n").unwrap();
    fs::write(home.join("session_index.jsonl"), [0xff, 0xfe]).unwrap();
    create_codex_thread_db(&db_path, &rollout_path);
    let adapter = SQLiteStorageAdapter::new(&db_path, BackupStore::new(home.join("backups")))
        .with_codex_home(home);

    let deleted = adapter.delete_local(&session("t1", "Codex Thread"));

    assert_eq!(deleted.status, DeleteStatus::Failed);
    assert_eq!(thread_count(&db_path, "t1"), 1);
    assert!(rollout_path.exists());
    assert_eq!(
        fs::read(home.join("session_index.jsonl")).unwrap(),
        [0xff, 0xfe]
    );
}

#[test]
fn rollout_outside_codex_home_blocks_delete_before_any_user_data_changes() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join("codex-home");
    let outside = tmp.path().join("unrelated-user-file.jsonl");
    fs::create_dir_all(&home).unwrap();
    fs::write(&outside, "do not delete\n").unwrap();
    let db_path = home.join("state_5.sqlite");
    create_codex_thread_db(&db_path, &outside);
    let adapter = SQLiteStorageAdapter::new(&db_path, BackupStore::new(home.join("backups")))
        .with_codex_home(&home);

    let deleted = adapter.delete_local(&session("t1", "Codex Thread"));

    assert_eq!(deleted.status, DeleteStatus::Failed);
    assert!(deleted.message.contains("outside the resolved Codex home"));
    assert_eq!(thread_count(&db_path, "t1"), 1);
    assert_eq!(fs::read_to_string(&outside).unwrap(), "do not delete\n");
}

#[test]
fn grouped_undo_restores_duplicate_threads_to_their_source_databases() {
    let tmp = tempdir().unwrap();
    let home = tmp.path().join("codex-home");
    let sqlite_dir = home.join("sqlite");
    fs::create_dir_all(&sqlite_dir).unwrap();
    let old_db = home.join("state_5.sqlite");
    let new_db = sqlite_dir.join("codex-dev.db");
    let rollout = home.join("rollout.jsonl");
    fs::write(
        &rollout,
        "{\"type\":\"message\",\"payload\":\"original\"}\n",
    )
    .unwrap();
    fs::write(home.join("session_index.jsonl"), "{\"id\":\"t1\"}\n").unwrap();
    create_codex_thread_db(&old_db, &rollout);
    create_codex_thread_db(&new_db, &rollout);
    Connection::open(&new_db)
        .unwrap()
        .execute("ALTER TABLE threads ADD COLUMN recency_at INTEGER", [])
        .unwrap();

    let backups = BackupStore::new(tmp.path().join("backups"));
    let deleted = delete_local_from_paths(
        vec![old_db.clone(), new_db.clone()],
        backups.clone(),
        &session("t1", "Codex Thread"),
        Some(&home),
    );
    let token = deleted.undo_token.as_deref().unwrap();

    assert_eq!(deleted.status, DeleteStatus::LocalDeleted);
    assert_eq!(thread_count(&old_db, "t1"), 0);
    assert_eq!(thread_count(&new_db, "t1"), 0);
    assert!(!rollout.exists());

    let restored = SQLiteStorageAdapter::new(&old_db, backups)
        .with_allowed_db_paths(vec![old_db.clone(), new_db.clone()])
        .with_codex_home(&home)
        .undo(token);

    assert_eq!(restored.status, DeleteStatus::Undone);
    assert_eq!(thread_count(&old_db, "t1"), 1);
    assert_eq!(thread_count(&new_db, "t1"), 1);
    assert!(rollout.exists());
    assert!(
        fs::read_to_string(home.join("session_index.jsonl"))
            .unwrap()
            .contains("\"id\":\"t1\"")
    );
}

#[test]
fn grouped_undo_preflights_all_databases_before_restoring_any() {
    let tmp = tempdir().unwrap();
    let first_db = tmp.path().join("first.sqlite");
    let second_db = tmp.path().join("second.sqlite");
    let first_rollout = tmp.path().join("first.jsonl");
    let second_rollout = tmp.path().join("second.jsonl");
    fs::write(&first_rollout, "{\"type\":\"message\"}\n").unwrap();
    fs::write(&second_rollout, "{\"type\":\"message\"}\n").unwrap();
    create_codex_thread_db(&first_db, &first_rollout);
    create_codex_thread_db(&second_db, &second_rollout);
    let backups = BackupStore::new(tmp.path().join("backups"));
    let deleted = delete_local_from_paths(
        vec![first_db.clone(), second_db.clone()],
        backups.clone(),
        &session("t1", "Codex Thread"),
        Some(tmp.path()),
    );
    let token = deleted.undo_token.as_deref().unwrap();
    Connection::open(&second_db)
        .unwrap()
        .execute(
            "ALTER TABLE threads RENAME COLUMN title TO renamed_title",
            [],
        )
        .unwrap();

    let restored = SQLiteStorageAdapter::new(&first_db, backups)
        .with_allowed_db_paths(vec![first_db.clone(), second_db.clone()])
        .with_codex_home(tmp.path())
        .undo(token);

    assert_eq!(restored.status, DeleteStatus::Failed);
    assert!(restored.message.contains("title"));
    assert_eq!(thread_count(&first_db, "t1"), 0);
    assert_eq!(thread_count(&second_db, "t1"), 0);
    assert!(!first_rollout.exists());
    assert!(!second_rollout.exists());
}

#[test]
fn grouped_undo_resumes_after_one_database_was_already_restored() {
    let tmp = tempdir().unwrap();
    let first_db = tmp.path().join("first.sqlite");
    let second_db = tmp.path().join("second.sqlite");
    let first_rollout = tmp.path().join("first.jsonl");
    let second_rollout = tmp.path().join("second.jsonl");
    fs::write(&first_rollout, "first\n").unwrap();
    fs::write(&second_rollout, "second\n").unwrap();
    create_codex_thread_db(&first_db, &first_rollout);
    create_codex_thread_db(&second_db, &second_rollout);
    let backups = BackupStore::new(tmp.path().join("backups"));
    let deleted = delete_local_from_paths(
        vec![first_db.clone(), second_db.clone()],
        backups.clone(),
        &session("t1", "Codex Thread"),
        Some(tmp.path()),
    );
    let grouped_token = deleted.undo_token.as_deref().unwrap();
    let tokens = serde_json::from_str::<Vec<String>>(grouped_token).unwrap();
    let adapter = SQLiteStorageAdapter::new(&first_db, backups)
        .with_allowed_db_paths(vec![first_db.clone(), second_db.clone()])
        .with_codex_home(tmp.path());

    let first_restore = adapter.undo(&tokens[0]);
    assert_eq!(first_restore.status, DeleteStatus::Undone);
    assert_eq!(thread_count(&first_db, "t1"), 1);
    assert_eq!(thread_count(&second_db, "t1"), 0);

    let resumed = adapter.undo(grouped_token);

    assert_eq!(resumed.status, DeleteStatus::Undone);
    assert_eq!(thread_count(&first_db, "t1"), 1);
    assert_eq!(thread_count(&second_db, "t1"), 1);
    assert_eq!(fs::read_to_string(&first_rollout).unwrap(), "first\n");
    assert_eq!(fs::read_to_string(&second_rollout).unwrap(), "second\n");
}

#[test]
fn grouped_undo_rejects_source_database_outside_allowed_paths() {
    let tmp = tempdir().unwrap();
    let allowed_db = tmp.path().join("allowed.sqlite");
    let outside_db = tmp.path().join("outside.sqlite");
    create_supported_db(&allowed_db);
    create_supported_db(&outside_db);
    let backups = BackupStore::new(tmp.path().join("backups"));
    let deleted = SQLiteStorageAdapter::new(&outside_db, backups.clone())
        .delete_local(&session("s1", "First"));

    let restored = SQLiteStorageAdapter::new(&allowed_db, backups)
        .undo(deleted.undo_token.as_deref().unwrap());

    assert_eq!(restored.status, DeleteStatus::Failed);
    assert!(restored.message.contains("allowed local storage path"));
    assert_eq!(
        Connection::open(&outside_db)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn move_thread_workspace_from_paths_uses_database_that_contains_thread() {
    let tmp = tempdir().unwrap();
    let stale_db = tmp.path().join("stale.sqlite");
    let live_db = tmp.path().join("live.sqlite");
    let stale_rollout = tmp.path().join("stale.jsonl");
    let live_rollout = tmp.path().join("live.jsonl");
    fs::write(&stale_rollout, "{\"type\":\"message\"}\n").unwrap();
    fs::write(
        &live_rollout,
        "{\"type\":\"session_meta\",\"payload\":{\"id\":\"t1\",\"cwd\":\"/old/project\",\"title\":\"Codex Thread\"}}\n",
    )
    .unwrap();
    create_codex_thread_db(&stale_db, &stale_rollout);
    create_codex_thread_db(&live_db, &live_rollout);
    Connection::open(&stale_db)
        .unwrap()
        .execute("DELETE FROM threads WHERE id = 't1'", [])
        .unwrap();

    let result = move_codex_thread_workspace_from_paths(
        vec![stale_db.clone(), live_db.clone()],
        BackupStore::new(tmp.path().join("backups")),
        &session("local:t1", "Codex Thread"),
        "/new/project",
    );

    assert_eq!(result["status"], "moved");
    assert_eq!(result["target_cwd"], "/new/project");
    assert_eq!(result["db_path"], live_db.to_string_lossy().to_string());
    assert_eq!(
        Connection::open(&live_db)
            .unwrap()
            .query_row("SELECT cwd FROM threads WHERE id = 't1'", [], |row| row
                .get::<_, String>(
                0
            ))
            .unwrap(),
        "/new/project"
    );
    assert_eq!(thread_count(&stale_db, "t1"), 0);
}

#[test]
fn list_local_sessions_reads_codex_threads_ordered_by_update_time() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("state_5.sqlite");
    let backup = BackupStore::new(tmp.path().join("backups"));
    let adapter = SQLiteStorageAdapter::new(&db_path, backup);
    let db = Connection::open(&db_path).unwrap();
    db.execute(
        "CREATE TABLE threads (id TEXT PRIMARY KEY, rollout_path TEXT, title TEXT, cwd TEXT, model_provider TEXT, archived INTEGER, updated_at_ms INTEGER)",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO threads VALUES ('t1', 'r1.jsonl', 'First', 'C:/a', 'openai', 0, 100)",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO threads VALUES ('t2', 'r2.jsonl', 'Second', 'C:/b', 'custom', 1, 300)",
        [],
    )
    .unwrap();
    drop(db);

    let sessions = adapter.list_local_sessions().unwrap();

    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].id, "t2");
    assert_eq!(sessions[0].title, "Second");
    assert_eq!(sessions[0].model_provider, "custom");
    assert!(sessions[0].archived);
    assert_eq!(sessions[1].id, "t1");
}

#[test]
fn list_local_sessions_reads_codex_automation_runs_schema() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("codex-dev.db");
    let backup = BackupStore::new(tmp.path().join("backups"));
    let adapter = SQLiteStorageAdapter::new(&db_path, backup);
    let db = Connection::open(&db_path).unwrap();
    db.execute(
        "CREATE TABLE automation_runs (
            thread_id TEXT PRIMARY KEY,
            status TEXT,
            thread_title TEXT,
            source_cwd TEXT,
            created_at INTEGER,
            updated_at INTEGER
        )",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO automation_runs VALUES ('t1', 'running', 'First', 'C:/a', 100, 200)",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO automation_runs VALUES ('t2', 'archived', 'Second', 'C:/b', 300, 400)",
        [],
    )
    .unwrap();
    drop(db);

    let sessions = adapter.list_local_sessions().unwrap();

    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].id, "t2");
    assert_eq!(sessions[0].title, "Second");
    assert_eq!(sessions[0].cwd, "C:/b");
    assert!(sessions[0].archived);
    assert_eq!(sessions[0].db_path, db_path.to_string_lossy());
    assert_eq!(sessions[1].id, "t1");
}

#[test]
fn delete_local_session_removes_codex_automation_run_and_inbox_items() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("codex-dev.db");
    let backup = BackupStore::new(tmp.path().join("backups"));
    let adapter = SQLiteStorageAdapter::new(&db_path, backup);
    let db = Connection::open(&db_path).unwrap();
    db.execute(
        "CREATE TABLE automation_runs (thread_id TEXT PRIMARY KEY, thread_title TEXT)",
        [],
    )
    .unwrap();
    db.execute(
        "CREATE TABLE inbox_items (id TEXT PRIMARY KEY, thread_id TEXT, title TEXT)",
        [],
    )
    .unwrap();
    db.execute("INSERT INTO automation_runs VALUES ('t1', 'First')", [])
        .unwrap();
    db.execute("INSERT INTO inbox_items VALUES ('i1', 't1', 'Inbox')", [])
        .unwrap();
    drop(db);

    let result = adapter.delete_local(&session("t1", "First"));

    assert_eq!(result.status, DeleteStatus::LocalDeleted);
    let db = Connection::open(&db_path).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM automation_runs WHERE thread_id = 't1'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM inbox_items WHERE thread_id = 't1'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        0
    );
}

#[test]
fn undo_codex_thread_delete_fails_when_agent_job_was_reassigned() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("state_5.sqlite");
    let rollout_path = tmp.path().join("rollout.jsonl");
    fs::write(&rollout_path, "{\"type\":\"message\"}\n").unwrap();
    create_codex_thread_db(&db_path, &rollout_path);
    let adapter = SQLiteStorageAdapter::new(&db_path, BackupStore::new(tmp.path().join("backups")))
        .with_codex_home(tmp.path());

    let deleted = adapter.delete_local(&session("local:t1", "Codex Thread"));

    assert_eq!(deleted.status, DeleteStatus::LocalDeleted);
    let token = deleted.undo_token.as_deref().unwrap();
    let db = Connection::open(&db_path).unwrap();
    db.execute(
        "INSERT INTO threads (id, rollout_path, title, cwd, archived, archived_at, updated_at, updated_at_ms) VALUES ('t2', NULL, 'Other Thread', '/new/project', 0, NULL, 200, 200000)",
        [],
    )
    .unwrap();
    db.execute(
        "UPDATE agent_job_items SET assigned_thread_id = 't2' WHERE id = 'job1'",
        [],
    )
    .unwrap();
    drop(db);

    let restored = adapter.undo(token);

    assert_eq!(restored.status, DeleteStatus::Failed);
    assert_eq!(restored.undo_token.as_deref(), Some(token));
    assert!(restored.message.to_lowercase().contains("restore conflict"));
    let db = Connection::open(&db_path).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT assigned_thread_id FROM agent_job_items WHERE id = 'job1'",
            [],
            |row| row.get::<_, Option<String>>(0)
        )
        .unwrap(),
        Some("t2".to_string())
    );
}

#[test]
fn codex_delete_rolls_back_when_related_delete_fails() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("state_5.sqlite");
    let rollout_path = tmp.path().join("rollout.jsonl");
    fs::write(&rollout_path, "{\"type\":\"message\"}\n").unwrap();
    create_codex_thread_db(&db_path, &rollout_path);
    let db = Connection::open(&db_path).unwrap();
    db.execute(
        "CREATE TRIGGER fail_goals_delete BEFORE DELETE ON thread_goals BEGIN SELECT RAISE(ABORT, 'boom'); END",
        [],
    )
    .unwrap();
    drop(db);
    let adapter = SQLiteStorageAdapter::new(&db_path, BackupStore::new(tmp.path().join("backups")));

    let result = adapter.delete_local(&session("t1", "Codex Thread"));

    assert_eq!(result.status, DeleteStatus::Failed);
    assert!(result.undo_token.is_some());
    assert!(rollout_path.exists());
    let db = Connection::open(&db_path).unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM threads WHERE id = 't1'", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM thread_dynamic_tools WHERE thread_id = 't1'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM thread_goals WHERE thread_id = 't1'",
            [],
            |row| { row.get::<_, i64>(0) }
        )
        .unwrap(),
        1
    );
}

#[test]
fn missing_db_and_unsupported_schema_return_failed_results() {
    let tmp = tempdir().unwrap();
    let missing = tmp.path().join("missing.sqlite");
    let adapter = SQLiteStorageAdapter::new(&missing, BackupStore::new(tmp.path().join("backups")));

    let result = adapter.delete_local(&session("s1", "First"));

    assert_eq!(result.status, DeleteStatus::Failed);
    assert!(result.message.contains("Database not found"));

    let db_path = tmp.path().join("unknown.sqlite");
    let db = Connection::open(&db_path).unwrap();
    db.execute("CREATE TABLE unrelated (id TEXT PRIMARY KEY)", [])
        .unwrap();
    drop(db);
    let adapter =
        SQLiteStorageAdapter::new(&db_path, BackupStore::new(tmp.path().join("backups2")));

    let result = adapter.delete_local(&session("s1", "First"));

    assert_eq!(result.status, DeleteStatus::Failed);
    assert!(result.message.contains("Unsupported"));
}

#[test]
fn archived_lookup_workspace_move_and_sort_keys_match_expected_shape() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("state_5.sqlite");
    let rollout_path = tmp.path().join("rollout.jsonl");
    fs::write(
        &rollout_path,
        "{\"type\":\"session_meta\",\"payload\":{\"id\":\"t1\",\"cwd\":\"/old/project\",\"title\":\"Codex Thread\"}}\n{\"type\":\"session_meta\",\"payload\":{\"id\":\"other\",\"cwd\":\"/old/project\"}}\n",
    )
    .unwrap();
    create_codex_thread_db(&db_path, &rollout_path);
    let db = Connection::open(&db_path).unwrap();
    db.execute(
        "UPDATE threads SET archived = 1, archived_at = 123 WHERE id = 't1'",
        [],
    )
    .unwrap();
    db.execute("INSERT INTO threads (id, rollout_path, title, cwd, archived, archived_at, updated_at, updated_at_ms) VALUES ('t2', ?1, 'Second', '/other/project', 0, NULL, 200, 200000)", [rollout_path.to_string_lossy().to_string()]).unwrap();
    drop(db);
    let adapter = SQLiteStorageAdapter::new(&db_path, BackupStore::new(tmp.path().join("backups")));

    assert_eq!(
        adapter.find_archived_thread_by_title("Codex Thread 2026年5月9日，1:19 · RustGUI"),
        Some(session("t1", "Codex Thread"))
    );

    let moved =
        adapter.move_codex_thread_workspace(&session("local:t1", "Codex Thread"), "/new/project");
    assert_eq!(moved["status"], "moved");
    assert_eq!(moved["previous_cwd"], "/old/project");
    assert_eq!(moved["target_cwd"], "/new/project");
    assert_eq!(moved["rollout_updated"], true);
    assert_eq!(moved["updated_at"], 100);
    assert_eq!(moved["updated_at_ms"], 100000);
    let text = fs::read_to_string(&rollout_path).unwrap();
    assert!(text.contains("\"id\":\"t1\",\"cwd\":\"/new/project\""));
    assert!(text.contains("\"id\":\"other\",\"cwd\":\"/old/project\""));

    assert_eq!(
        adapter.codex_thread_sort_key(&session("local:t1", "Codex Thread")),
        json!({"status": "ok", "session_id": "t1", "updated_at": 100, "updated_at_ms": 100000, "created_at_ms": null})
    );
    assert_eq!(
        adapter.codex_thread_sort_keys(&[
            session("local:t2", "Second"),
            session("local:t1", "Codex Thread")
        ]),
        json!({
            "status": "ok",
            "sort_keys": [
                {"session_id": "t2", "updated_at": 200, "updated_at_ms": 200000, "created_at_ms": null},
                {"session_id": "t1", "updated_at": 100, "updated_at_ms": 100000, "created_at_ms": null}
            ]
        })
    );

    assert_eq!(
        adapter.codex_thread_usage_history(&session("local:t1", "Codex Thread")),
        json!({
            "status": "ok",
            "session_id": "t1",
            "rollout_path": rollout_path.to_string_lossy().to_string(),
            "history": []
        })
    );
}

#[test]
fn workspace_move_streams_large_rollout_and_preserves_unrelated_content() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("state_5.sqlite");
    let rollout_path = tmp.path().join("large-rollout.jsonl");
    let large_line = format!(
        "{{\"type\":\"event_msg\",\"payload\":{{\"blob\":\"{}\"}}}}\n",
        "x".repeat(4 * 1024 * 1024)
    );
    fs::write(
        &rollout_path,
        format!(
            "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"t1\",\"cwd\":\"/old/project\"}}}}\n{large_line}{{\"type\":\"event_msg\",\"payload\":{{\"message\":\"tail-marker\"}}}}\n"
        ),
    )
    .unwrap();
    create_codex_thread_db(&db_path, &rollout_path);
    let adapter = SQLiteStorageAdapter::new(&db_path, BackupStore::new(tmp.path().join("backups")));

    let moved =
        adapter.move_codex_thread_workspace(&session("local:t1", "Codex Thread"), "/new/project");

    assert_eq!(moved["status"], "moved");
    assert_eq!(moved["rollout_updated"], true);
    let updated = fs::read_to_string(&rollout_path).unwrap();
    assert!(updated.contains("\"id\":\"t1\",\"cwd\":\"/new/project\""));
    assert!(updated.contains(&large_line));
    assert!(updated.contains("tail-marker"));
    assert_eq!(
        fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry
                .file_name()
                .to_string_lossy()
                .contains("workspace-move"))
            .count(),
        0
    );
}

#[cfg(windows)]
#[test]
fn workspace_move_locked_rollout_rolls_back_and_succeeds_after_unlock() {
    use std::os::windows::fs::OpenOptionsExt;

    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("state_5.sqlite");
    let rollout_path = tmp.path().join("locked-rollout.jsonl");
    let original =
        b"{\"type\":\"session_meta\",\"payload\":{\"id\":\"t1\",\"cwd\":\"/old/project\"}}\n";
    fs::write(&rollout_path, original).unwrap();
    create_codex_thread_db(&db_path, &rollout_path);
    let adapter = SQLiteStorageAdapter::new(&db_path, BackupStore::new(tmp.path().join("backups")));
    let lock = OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&rollout_path)
        .unwrap();

    let blocked =
        adapter.move_codex_thread_workspace(&session("local:t1", "Codex Thread"), "/new/project");

    assert_eq!(blocked["status"], "failed");
    let db = Connection::open(&db_path).unwrap();
    let cwd: String = db
        .query_row("SELECT cwd FROM threads WHERE id = 't1'", [], |row| {
            row.get(0)
        })
        .unwrap();
    drop(db);
    assert_eq!(cwd, "/old/project");
    assert_eq!(fs::read(&rollout_path).unwrap(), original);

    drop(lock);
    let retried =
        adapter.move_codex_thread_workspace(&session("local:t1", "Codex Thread"), "/new/project");
    assert_eq!(retried["status"], "moved");
    assert!(
        fs::read_to_string(&rollout_path)
            .unwrap()
            .contains("\"cwd\":\"/new/project\"")
    );
}

#[test]
fn thread_usage_history_reads_rollout_token_count_events() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("state_5.sqlite");
    let rollout_path = tmp.path().join("rollout.jsonl");
    fs::write(
        &rollout_path,
        concat!(
            "{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"turn-1\"}}\n",
            "{\"timestamp\":\"2026-06-02T05:00:00Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"input_tokens\":5000,\"cached_input_tokens\":1500,\"output_tokens\":500,\"total_tokens\":5500},\"last_token_usage\":{\"input_tokens\":1200,\"cached_input_tokens\":900,\"output_tokens\":120,\"total_tokens\":1320},\"model_context_window\":258400}}}\n",
            "{\"timestamp\":\"2026-06-02T05:00:01Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"ignore\"}}\n",
            "{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"turn-2\"}}\n",
            "{\"timestamp\":\"2026-06-02T05:01:00Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"input_tokens\":7000,\"cached_input_tokens\":2500,\"output_tokens\":750,\"total_tokens\":7750},\"last_token_usage\":{\"input_tokens\":2000,\"cached_input_tokens\":1200,\"output_tokens\":250,\"total_tokens\":2250},\"model_context_window\":258400}}}\n"
        ),
    )
    .unwrap();
    create_codex_thread_db(&db_path, &rollout_path);
    let adapter = SQLiteStorageAdapter::new(&db_path, BackupStore::new(tmp.path().join("backups")));

    assert_eq!(
        adapter.codex_thread_usage_history(&session("local:t1", "Codex Thread")),
        json!({
            "status": "ok",
            "session_id": "t1",
            "rollout_path": rollout_path.to_string_lossy().to_string(),
            "history": [
                {
                    "source": "rollout-history",
                    "conversation_id": "local:t1",
                    "turn_id": "turn-1",
                    "observed_at": "2026-06-02T05:00:00Z",
                    "usage": {
                        "inputTokens": 1200,
                        "outputTokens": 120,
                        "totalTokens": 1320,
                        "cachedTokens": 900,
                        "cacheReadTokens": 0,
                        "cacheCreationTokens": 0,
                        "contextUsed": 5500,
                        "contextLimit": 258400,
                        "hasBreakdown": true
                    }
                },
                {
                    "source": "rollout-history",
                    "conversation_id": "local:t1",
                    "turn_id": "turn-2",
                    "observed_at": "2026-06-02T05:01:00Z",
                    "usage": {
                        "inputTokens": 2000,
                        "outputTokens": 250,
                        "totalTokens": 2250,
                        "cachedTokens": 1200,
                        "cacheReadTokens": 0,
                        "cacheCreationTokens": 0,
                        "contextUsed": 7750,
                        "contextLimit": 258400,
                        "hasBreakdown": true
                    }
                }
            ]
        })
    );
}

#[test]
fn thread_usage_history_reads_recent_tail_without_loading_large_rollout() {
    const SCAN_BUDGET: u64 = 32 * 1024 * 1024;
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("state_5.sqlite");
    let rollout_path = tmp.path().join("large-rollout.jsonl");
    let total_len = SCAN_BUDGET + 8 * 1024;
    let mut rollout = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&rollout_path)
        .unwrap();
    rollout.set_len(total_len).unwrap();
    rollout
        .seek(SeekFrom::Start(total_len - SCAN_BUDGET + 16))
        .unwrap();
    rollout
        .write_all(
            concat!(
                "\n{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"recent-turn\"}}\n",
                "{\"timestamp\":\"2026-08-21T12:00:00Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"total_tokens\":9000},\"last_token_usage\":{\"input_tokens\":1000,\"output_tokens\":200,\"total_tokens\":1200},\"model_context_window\":258400}}}\n"
            )
            .as_bytes(),
        )
        .unwrap();
    drop(rollout);
    create_codex_thread_db(&db_path, &rollout_path);
    let adapter = SQLiteStorageAdapter::new(&db_path, BackupStore::new(tmp.path().join("backups")));

    let result = adapter.codex_thread_usage_history(&session("local:t1", "Codex Thread"));

    assert_eq!(result["status"], "ok");
    assert_eq!(result["historyTruncated"], true);
    assert_eq!(result["history"].as_array().unwrap().len(), 1);
    assert_eq!(result["history"][0]["turn_id"], "recent-turn");
    assert_eq!(result["history"][0]["usage"]["totalTokens"], 1200);
}
