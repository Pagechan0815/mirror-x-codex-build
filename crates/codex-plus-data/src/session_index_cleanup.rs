use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const MAX_SESSION_INDEX_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionIndexCleanupCandidate {
    pub id: String,
    pub thread_name: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionIndexCleanupPreview {
    pub snapshot_sha256: String,
    pub candidates: Vec<SessionIndexCleanupCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionIndexCleanupResult {
    pub pruned_entries: usize,
    pub backup_dir: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct SessionIndexCleanupApplyError {
    pub message: String,
    pub backup_dir: Option<PathBuf>,
}

#[derive(Debug)]
struct CleanupPlan {
    path: PathBuf,
    original_bytes: Vec<u8>,
    original_text: String,
    snapshot_sha256: String,
    candidates: Vec<SessionIndexCleanupCandidate>,
}

pub fn preview_session_index_cleanup(
    codex_home: Option<&Path>,
) -> anyhow::Result<SessionIndexCleanupPreview> {
    let home = codex_home
        .map(Path::to_path_buf)
        .unwrap_or_else(codex_plus_core::codex_home::default_codex_home_dir);
    let live_ids = collect_live_thread_ids(&home)?;
    let Some(plan) = plan_cleanup(&home.join("session_index.jsonl"), &live_ids)? else {
        return Ok(SessionIndexCleanupPreview {
            snapshot_sha256: sha256_hex(&[]),
            candidates: Vec::new(),
        });
    };
    Ok(SessionIndexCleanupPreview {
        snapshot_sha256: plan.snapshot_sha256,
        candidates: plan.candidates,
    })
}

pub fn apply_session_index_cleanup(
    codex_home: Option<&Path>,
    expected_snapshot_sha256: &str,
    confirmed_thread_ids: &[String],
) -> Result<SessionIndexCleanupResult, SessionIndexCleanupApplyError> {
    let require_stopped_app = codex_home.is_none();
    if require_stopped_app {
        ensure_codex_stopped(None)?;
    }
    let home = codex_home
        .map(Path::to_path_buf)
        .unwrap_or_else(codex_plus_core::codex_home::default_codex_home_dir);
    let lock = CleanupLock::acquire(&home.join("tmp/session-index-cleanup.lock"))?;
    let live_ids = collect_live_thread_ids(&home).map_err(|error| apply_error(error, None))?;
    let plan = plan_cleanup(&home.join("session_index.jsonl"), &live_ids)
        .map_err(|error| apply_error(error, None))?
        .ok_or_else(|| apply_error("session_index.jsonl 不存在，无法清理", None))?;
    if plan.snapshot_sha256 != expected_snapshot_sha256 {
        return Err(apply_error(
            "会话索引已在预览后变化；本次没有写入，请重新预览",
            None,
        ));
    }
    let candidate_ids = plan
        .candidates
        .iter()
        .map(|candidate| candidate.id.as_str())
        .collect::<HashSet<_>>();
    let selected_ids = confirmed_thread_ids
        .iter()
        .map(|id| id.trim())
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect::<HashSet<_>>();
    if selected_ids
        .iter()
        .any(|id| !candidate_ids.contains(id.as_str()))
    {
        return Err(apply_error(
            "确认列表包含非候选会话；本次没有写入，请重新预览",
            None,
        ));
    }
    let (next_text, removed) = filtered_text(&plan, &selected_ids);
    if removed == 0 {
        drop(lock);
        return Ok(SessionIndexCleanupResult {
            pruned_entries: 0,
            backup_dir: None,
        });
    }
    let backup_dir = create_backup(&home, &plan, removed)?;
    let current =
        fs::read(&plan.path).map_err(|error| apply_error(error, Some(backup_dir.clone())))?;
    if current != plan.original_bytes {
        return Err(apply_error(
            "会话索引在写入前再次变化；本次没有覆盖新内容",
            Some(backup_dir),
        ));
    }
    if require_stopped_app {
        ensure_codex_stopped(Some(backup_dir.clone()))?;
    }
    codex_plus_core::settings::atomic_write(&plan.path, next_text.as_bytes()).map_err(|error| {
        apply_error(
            format!("会话索引原子写入失败，可从备份恢复：{error}"),
            Some(backup_dir.clone()),
        )
    })?;
    drop(lock);
    Ok(SessionIndexCleanupResult {
        pruned_entries: removed,
        backup_dir: Some(backup_dir),
    })
}

fn collect_live_thread_ids(home: &Path) -> anyhow::Result<HashSet<String>> {
    let mut ids = HashSet::new();
    for root_name in ["sessions", "archived_sessions"] {
        collect_rollout_ids(&home.join(root_name), &mut ids)?;
    }
    for path in codex_plus_core::codex_sqlite::codex_session_db_paths_from_home(home) {
        collect_sqlite_ids(&path, &mut ids)?;
    }
    Ok(ids)
}

fn collect_rollout_ids(root: &Path, ids: &mut HashSet<String>) -> anyhow::Result<()> {
    if !root.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_rollout_ids(&path, ids)?;
            continue;
        }
        if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
            continue;
        }
        if let Some(id) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(rollout_id_from_filename)
        {
            ids.insert(id);
        }
        let reader = BufReader::new(File::open(&path)?);
        for line in reader.lines() {
            let line = line?;
            let Ok(record) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if record.get("type").and_then(Value::as_str) != Some("session_meta") {
                continue;
            }
            if let Some(id) = record
                .pointer("/payload/id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
            {
                ids.insert(id.to_string());
            }
        }
    }
    Ok(())
}

fn rollout_id_from_filename(name: &str) -> Option<String> {
    let stem = name.strip_prefix("rollout-")?.strip_suffix(".jsonl")?;
    if stem.len() < 36 {
        return None;
    }
    let candidate = &stem[stem.len() - 36..];
    Uuid::parse_str(candidate).ok().map(|id| id.to_string())
}

fn collect_sqlite_ids(path: &Path, ids: &mut HashSet<String>) -> anyhow::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let db = Connection::open(path)?;
    for (table, column) in [
        ("threads", "id"),
        ("local_thread_catalog", "thread_id"),
        ("automation_runs", "thread_id"),
        ("inbox_items", "thread_id"),
        ("sessions", "id"),
        ("messages", "session_id"),
        ("thread_dynamic_tools", "thread_id"),
        ("thread_goals", "thread_id"),
        ("thread_spawn_edges", "parent_thread_id"),
        ("thread_spawn_edges", "child_thread_id"),
    ] {
        if !sqlite_columns(&db, table)?.contains(column) {
            continue;
        }
        let sql =
            format!("SELECT DISTINCT {column} FROM {table} WHERE COALESCE({column}, '') <> ''");
        let mut statement = db.prepare(&sql)?;
        for id in statement.query_map([], |row| row.get::<_, String>(0))? {
            ids.insert(id?);
        }
    }
    Ok(())
}

fn sqlite_columns(db: &Connection, table: &str) -> anyhow::Result<HashSet<String>> {
    let mut statement = db.prepare(&format!("PRAGMA table_info({table})"))?;
    Ok(statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<HashSet<_>>>()?)
}

fn plan_cleanup(path: &Path, live_ids: &HashSet<String>) -> anyhow::Result<Option<CleanupPlan>> {
    if !path.exists() {
        return Ok(None);
    }
    let size = fs::metadata(path)?.len();
    if size > MAX_SESSION_INDEX_BYTES {
        anyhow::bail!(
            "session_index.jsonl 过大（{} MB），为避免低内存设备卡死，本次只读预览已停止",
            size / 1024 / 1024
        );
    }
    let original_bytes = fs::read(path)?;
    let original_text = String::from_utf8(original_bytes.clone())?;
    let candidates = original_text
        .lines()
        .filter_map(known_candidate)
        .filter(|candidate| !live_ids.contains(&candidate.id))
        .collect();
    Ok(Some(CleanupPlan {
        path: path.to_path_buf(),
        snapshot_sha256: sha256_hex(&original_bytes),
        original_bytes,
        original_text,
        candidates,
    }))
}

fn known_candidate(line: &str) -> Option<SessionIndexCleanupCandidate> {
    let record = serde_json::from_str::<Value>(line).ok()?;
    let object = record.as_object()?;
    if object.len() != 3
        || !["id", "thread_name", "updated_at"]
            .iter()
            .all(|key| object.contains_key(*key))
    {
        return None;
    }
    let id = object.get("id")?.as_str()?.trim();
    let thread_name = object.get("thread_name")?.as_str()?;
    let updated_at = object.get("updated_at")?.as_str()?.trim();
    if id.is_empty() || updated_at.is_empty() {
        return None;
    }
    Some(SessionIndexCleanupCandidate {
        id: id.to_string(),
        thread_name: thread_name.to_string(),
        updated_at: updated_at.to_string(),
    })
}

fn filtered_text(plan: &CleanupPlan, selected: &HashSet<String>) -> (String, usize) {
    let mut output = String::with_capacity(plan.original_text.len());
    let mut removed = 0;
    for segment in plan.original_text.split_inclusive('\n') {
        let (line, ending) = segment
            .strip_suffix('\n')
            .map(|line| {
                (
                    line.strip_suffix('\r').unwrap_or(line),
                    if line.ends_with('\r') { "\r\n" } else { "\n" },
                )
            })
            .unwrap_or((segment, ""));
        if known_candidate(line).is_some_and(|candidate| selected.contains(&candidate.id)) {
            removed += 1;
        } else {
            output.push_str(line);
            output.push_str(ending);
        }
    }
    (output, removed)
}

fn create_backup(
    home: &Path,
    plan: &CleanupPlan,
    removed: usize,
) -> Result<PathBuf, SessionIndexCleanupApplyError> {
    let backup_dir = home
        .join("backups_state/session-index-cleanup")
        .join(format!(
            "{}-{}",
            chrono::Utc::now().format("%Y%m%d-%H%M%S"),
            Uuid::new_v4()
        ));
    fs::create_dir_all(&backup_dir).map_err(|error| apply_error(error, None))?;
    fs::write(backup_dir.join("session_index.jsonl"), &plan.original_bytes)
        .map_err(|error| apply_error(error, Some(backup_dir.clone())))?;
    let metadata = serde_json::to_vec_pretty(&serde_json::json!({
        "version": 1,
        "snapshotSha256": plan.snapshot_sha256,
        "prunedEntries": removed,
        "createdAt": chrono::Utc::now().to_rfc3339(),
        "managedBy": "Mirror X Codex session index cleanup"
    }))
    .map_err(|error| apply_error(error, Some(backup_dir.clone())))?;
    fs::write(backup_dir.join("metadata.json"), metadata)
        .map_err(|error| apply_error(error, Some(backup_dir.clone())))?;
    Ok(backup_dir)
}

fn ensure_codex_stopped(backup_dir: Option<PathBuf>) -> Result<(), SessionIndexCleanupApplyError> {
    let pids = codex_plus_core::watcher::find_codex_processes();
    if pids.is_empty() {
        return Ok(());
    }
    Err(apply_error(
        "Codex 仍在运行；请完全退出后重新预览并确认清理",
        backup_dir,
    ))
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn apply_error(
    message: impl std::fmt::Display,
    backup_dir: Option<PathBuf>,
) -> SessionIndexCleanupApplyError {
    SessionIndexCleanupApplyError {
        message: message.to_string(),
        backup_dir,
    }
}

struct CleanupLock(PathBuf);

impl CleanupLock {
    fn acquire(path: &Path) -> Result<Self, SessionIndexCleanupApplyError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| apply_error(error, None))?;
        }
        if path.exists() {
            let stale = fs::metadata(path)
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age.as_secs() >= 10 * 60);
            if stale {
                fs::remove_dir(path).map_err(|error| {
                    apply_error(format!("无法回收过期的会话维护锁：{error}"), None)
                })?;
            }
        }
        fs::create_dir(path)
            .map_err(|error| apply_error(format!("另一个会话维护操作正在进行：{error}"), None))?;
        Ok(Self(path.to_path_buf()))
    }
}

impl Drop for CleanupLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index_line(id: &str, name: &str) -> String {
        serde_json::json!({ "id": id, "thread_name": name, "updated_at": "2026-08-26T00:00:00Z" })
            .to_string()
    }

    #[test]
    fn cleanup_requires_preview_and_preserves_unknown_lines() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let live = Uuid::new_v4().to_string();
        let stale = Uuid::new_v4().to_string();
        fs::create_dir_all(home.join("sessions/2026/08/26")).unwrap();
        fs::write(
            home.join(format!("sessions/2026/08/26/rollout-{live}.jsonl")),
            "",
        )
        .unwrap();
        fs::write(
            home.join("session_index.jsonl"),
            format!(
                "{}\nnot-json\n{}\n",
                index_line(&live, "live"),
                index_line(&stale, "stale")
            ),
        )
        .unwrap();

        let preview = preview_session_index_cleanup(Some(home)).unwrap();
        assert_eq!(preview.candidates.len(), 1);
        assert_eq!(preview.candidates[0].id, stale);
        let result = apply_session_index_cleanup(
            Some(home),
            &preview.snapshot_sha256,
            std::slice::from_ref(&stale),
        )
        .unwrap();
        assert_eq!(result.pruned_entries, 1);
        assert!(
            result
                .backup_dir
                .unwrap()
                .join("session_index.jsonl")
                .is_file()
        );
        let remaining = fs::read_to_string(home.join("session_index.jsonl")).unwrap();
        assert!(remaining.contains(&live));
        assert!(remaining.contains("not-json"));
        assert!(!remaining.contains(&stale));
    }

    #[test]
    fn cleanup_rejects_a_stale_preview_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let stale = Uuid::new_v4().to_string();
        fs::write(
            home.join("session_index.jsonl"),
            index_line(&stale, "stale"),
        )
        .unwrap();
        let preview = preview_session_index_cleanup(Some(home)).unwrap();
        fs::write(home.join("session_index.jsonl"), "changed\n").unwrap();

        let error = apply_session_index_cleanup(Some(home), &preview.snapshot_sha256, &[stale])
            .unwrap_err();
        assert!(error.message.contains("预览后变化"));
        assert_eq!(
            fs::read_to_string(home.join("session_index.jsonl")).unwrap(),
            "changed\n"
        );
    }
}
