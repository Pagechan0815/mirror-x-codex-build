use crate::BackupStore;
use crate::backup::BackupDraft;
use anyhow::Context;
use codex_plus_core::models::{DeleteResult, DeleteStatus, SessionRef};
use fs2::FileExt;
use rusqlite::types::{ToSqlOutput, Value as SqlValue, ValueRef};
use rusqlite::{Connection, OpenFlags, OptionalExtension, ToSql, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{HashSet, VecDeque};
use std::fs;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

const SQLITE_READ_BUSY_TIMEOUT: Duration = Duration::from_millis(2_000);
const MAX_ROLLOUT_USAGE_SCAN_BYTES: u64 = 32 * 1024 * 1024;
const MAX_ROLLOUT_USAGE_ENTRIES: usize = 4_096;
const MAX_ROLLOUT_USAGE_LINE_BYTES: usize = 1024 * 1024;
const MAX_WORKSPACE_META_LINE_BYTES: usize = 1024 * 1024;
const WORKSPACE_REWRITE_OVERHEAD_BYTES: u64 = 1024 * 1024;
const WORKSPACE_MOVE_JOURNAL_VERSION: u32 = 1;
const MAX_WORKSPACE_MOVE_JOURNAL_BYTES: u64 = 64 * 1024;
const WORKSPACE_MOVE_JOURNAL_LOCK_TIMEOUT: Duration = Duration::from_secs(2);
const WORKSPACE_MOVE_JOURNAL_LOCK_RETRY: Duration = Duration::from_millis(25);

pub fn delete_local_from_paths(
    db_paths: impl IntoIterator<Item = PathBuf>,
    backup_store: BackupStore,
    session: &SessionRef,
    codex_home: Option<&Path>,
) -> DeleteResult {
    let mut result = failed(
        &session.session_id,
        "Thread not found in local storage".to_string(),
    );
    let mut targets = Vec::new();
    for db_path in db_paths {
        if !db_path.is_file() {
            continue;
        }
        let db_path = match fs::canonicalize(&db_path) {
            Ok(path) => path,
            Err(error) => {
                return failed(
                    &session.session_id,
                    format!("删除前解析本地存储路径失败：{error}"),
                );
            }
        };
        if targets.iter().any(|candidate| candidate == &db_path) {
            continue;
        }
        match database_contains_session(&db_path, session) {
            Ok(true) => targets.push(db_path),
            Ok(false) => {}
            Err(error) => {
                return failed(
                    &session.session_id,
                    format!("删除前检查本地会话失败：{error}"),
                );
            }
        }
    }
    if targets.is_empty() {
        return result;
    }

    let mut deleted_count = 0usize;
    let mut backup_tokens = Vec::new();
    for db_path in &targets {
        let mut adapter = SQLiteStorageAdapter::new(db_path, backup_store.clone())
            .with_allowed_db_paths(targets.clone());
        if let Some(home) = codex_home {
            adapter = adapter.with_codex_home(home);
        }
        let candidate_result = adapter.delete_local(session);
        if matches!(candidate_result.status, DeleteStatus::LocalDeleted) {
            deleted_count += 1;
            if let Some(token) = candidate_result.undo_token.as_ref() {
                backup_tokens.push(token.clone());
            }
            result = candidate_result;
            continue;
        }

        if deleted_count == 0 {
            return candidate_result;
        }

        let grouped_token = json!(backup_tokens).to_string();
        let mut rollback_adapter = SQLiteStorageAdapter::new(&targets[0], backup_store.clone())
            .with_allowed_db_paths(targets.clone());
        if let Some(home) = codex_home {
            rollback_adapter = rollback_adapter.with_codex_home(home);
        }
        let rollback = rollback_adapter.undo(&grouped_token);
        return if matches!(rollback.status, DeleteStatus::Undone) {
            failed_with_undo(
                &session.session_id,
                format!(
                    "删除未完成，已自动恢复此前删除的数据：{}",
                    candidate_result.message
                ),
                &grouped_token,
                None,
            )
        } else {
            failed_with_undo(
                &session.session_id,
                format!(
                    "删除未完成，且自动恢复失败：{}；{}",
                    candidate_result.message, rollback.message
                ),
                &grouped_token,
                None,
            )
        };
    }
    if deleted_count > 1 {
        result.message = format!("已从 {deleted_count} 个本地存储删除");
        result.undo_token = Some(json!(backup_tokens).to_string());
        result.backup_path = None;
    } else if deleted_count == 1 && result.undo_token.is_none() {
        result.undo_token = backup_tokens.into_iter().next();
    }
    result
}

fn database_contains_session(db_path: &Path, session: &SessionRef) -> anyhow::Result<bool> {
    let db = open_read_only(db_path)?;
    let found = match schema_kind(&db)? {
        Some(SchemaKind::GenericSessions) => db
            .query_row(
                "SELECT 1 FROM sessions WHERE id = ?1 LIMIT 1",
                [&session.session_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some(),
        Some(SchemaKind::CodexThreads) => {
            let thread_id = normalize_codex_thread_id(&session.session_id);
            db.query_row(
                "SELECT 1 FROM threads WHERE id = ?1 LIMIT 1",
                [&thread_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        }
        Some(SchemaKind::CodexAutomationRuns) => {
            let thread_id = normalize_codex_thread_id(&session.session_id);
            db.query_row(
                "SELECT 1 FROM automation_runs WHERE thread_id = ?1 LIMIT 1",
                [&thread_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        }
        None => false,
    };
    Ok(found)
}

pub fn move_codex_thread_workspace_from_paths(
    db_paths: impl IntoIterator<Item = PathBuf>,
    backup_store: BackupStore,
    session: &SessionRef,
    target_cwd: &str,
) -> Value {
    let mut result = json!({"status": "failed", "session_id": session.session_id, "message": "Thread not found in local storage"});
    for db_path in db_paths {
        let adapter = SQLiteStorageAdapter::new(db_path, backup_store.clone());
        let candidate_result = adapter.move_codex_thread_workspace(session, target_cwd);
        if candidate_result.get("status").and_then(Value::as_str) == Some("moved") {
            return candidate_result;
        }
        result = candidate_result;
    }
    result
}

#[derive(Debug, Clone)]
pub struct SQLiteStorageAdapter {
    db_path: PathBuf,
    backup_store: BackupStore,
    allowed_db_paths: Vec<PathBuf>,
    codex_home: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SchemaKind {
    GenericSessions,
    CodexThreads,
    CodexAutomationRuns,
}

fn sqlite_limit(limit: usize) -> i64 {
    i64::try_from(limit).unwrap_or(i64::MAX)
}

fn open_read_only(path: &Path) -> anyhow::Result<Connection> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(SQLITE_READ_BUSY_TIMEOUT)?;
    Ok(db)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalSession {
    pub id: String,
    pub title: String,
    pub cwd: String,
    pub model_provider: String,
    pub archived: bool,
    pub updated_at_ms: Option<i64>,
    pub rollout_path: String,
    pub db_path: String,
}

#[derive(Debug, Clone)]
struct OwnedSqlValue(SqlValue);

impl ToSql for OwnedSqlValue {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::Owned(self.0.clone()))
    }
}

impl SQLiteStorageAdapter {
    pub fn new(db_path: impl Into<PathBuf>, backup_store: BackupStore) -> Self {
        let db_path = db_path.into();
        Self {
            allowed_db_paths: vec![db_path.clone()],
            db_path,
            backup_store,
            codex_home: None,
        }
    }

    pub fn with_allowed_db_paths(mut self, db_paths: impl IntoIterator<Item = PathBuf>) -> Self {
        for db_path in db_paths {
            if !self.allowed_db_paths.contains(&db_path) {
                self.allowed_db_paths.push(db_path);
            }
        }
        self
    }

    pub fn with_codex_home(mut self, codex_home: impl Into<PathBuf>) -> Self {
        self.codex_home = Some(codex_home.into());
        self
    }

    pub fn delete_local(&self, session: &SessionRef) -> DeleteResult {
        if !self.db_path.exists() {
            return failed(
                &session.session_id,
                format!("Database not found: {}", self.db_path.to_string_lossy()),
            );
        }
        let result = (|| -> anyhow::Result<DeleteResult> {
            let mut db = Connection::open(&self.db_path)?;
            match schema_kind(&db)? {
                Some(SchemaKind::GenericSessions) => self.delete_generic_session(&mut db, session),
                Some(SchemaKind::CodexThreads) => self.delete_codex_thread(&mut db, session),
                Some(SchemaKind::CodexAutomationRuns) => {
                    self.delete_codex_automation_run(&mut db, session)
                }
                None => Ok(failed(
                    &session.session_id,
                    "Unsupported local storage schema".to_string(),
                )),
            }
        })();
        result.unwrap_or_else(|err| failed(&session.session_id, err.to_string()))
    }

    pub fn list_local_sessions(&self) -> anyhow::Result<Vec<LocalSession>> {
        self.list_local_sessions_limited(usize::MAX)
    }

    pub fn list_local_sessions_limited(&self, limit: usize) -> anyhow::Result<Vec<LocalSession>> {
        if !self.db_path.exists() {
            return Ok(Vec::new());
        }
        recover_workspace_moves_for_db(&self.db_path).context("恢复上次未完成的会话移动失败")?;
        let db = open_read_only(&self.db_path)?;
        match schema_kind(&db)? {
            Some(SchemaKind::CodexThreads) => self.list_codex_threads(&db, limit),
            Some(SchemaKind::CodexAutomationRuns) => self.list_codex_automation_runs(&db, limit),
            _ => anyhow::bail!("Unsupported local storage schema"),
        }
    }

    fn list_codex_threads(
        &self,
        db: &Connection,
        limit: usize,
    ) -> anyhow::Result<Vec<LocalSession>> {
        let columns = table_columns(&db, "threads")?
            .into_iter()
            .collect::<HashSet<_>>();
        let title = optional_column_expression(&columns, "title", "''");
        let cwd = optional_column_expression(&columns, "cwd", "''");
        let model_provider = optional_column_expression(&columns, "model_provider", "''");
        let archived = optional_column_expression(&columns, "archived", "0");
        let updated_at_ms = if columns.contains("updated_at_ms") {
            "updated_at_ms"
        } else if columns.contains("updated_at") {
            "updated_at * 1000"
        } else if columns.contains("created_at_ms") {
            "created_at_ms"
        } else {
            "NULL"
        };
        let rollout_path = optional_column_expression(&columns, "rollout_path", "''");
        let mut subagent_filters = Vec::new();
        if has_table(db, "thread_spawn_edges")?
            && table_columns(db, "thread_spawn_edges")?
                .iter()
                .any(|column| column == "child_thread_id")
        {
            subagent_filters.push(
                "NOT EXISTS (SELECT 1 FROM thread_spawn_edges e WHERE e.child_thread_id = threads.id)",
            );
        }
        if has_table(db, "agent_job_items")?
            && table_columns(db, "agent_job_items")?
                .iter()
                .any(|column| column == "assigned_thread_id")
        {
            subagent_filters.push(
                "NOT EXISTS (SELECT 1 FROM agent_job_items j WHERE j.assigned_thread_id = threads.id)",
            );
        }
        let child_thread_filter = if subagent_filters.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", subagent_filters.join(" AND "))
        };
        let sql = format!(
            "SELECT id, {title}, {cwd}, {model_provider}, {archived}, {updated_at_ms}, {rollout_path}
             FROM threads
             {child_thread_filter}
             ORDER BY COALESCE({updated_at_ms}, 0) DESC, id DESC
             LIMIT ?1"
        );
        let mut stmt = db.prepare(&sql)?;
        let rows = stmt.query_map([sqlite_limit(limit)], |row| {
            Ok(LocalSession {
                id: row.get(0)?,
                title: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                cwd: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                model_provider: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                archived: row.get::<_, Option<i64>>(4)?.unwrap_or_default() != 0,
                updated_at_ms: row.get(5)?,
                rollout_path: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
                db_path: self.db_path.to_string_lossy().to_string(),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn list_codex_automation_runs(
        &self,
        db: &Connection,
        limit: usize,
    ) -> anyhow::Result<Vec<LocalSession>> {
        let columns = table_columns(db, "automation_runs")?
            .into_iter()
            .collect::<HashSet<_>>();
        let title = optional_column_expression(&columns, "thread_title", "''");
        let cwd = optional_column_expression(&columns, "source_cwd", "''");
        let status = optional_column_expression(&columns, "status", "''");
        let updated_at = optional_column_expression(&columns, "updated_at", "NULL");
        let created_at = optional_column_expression(&columns, "created_at", "NULL");
        let sql = format!(
            "SELECT thread_id, {title}, {cwd}, {status}, {updated_at}, {created_at}
             FROM automation_runs
             WHERE COALESCE(thread_id, '') <> ''
             ORDER BY COALESCE({updated_at}, {created_at}, 0) DESC, thread_id DESC
             LIMIT ?1"
        );
        let mut stmt = db.prepare(&sql)?;
        let rows = stmt.query_map([sqlite_limit(limit)], |row| {
            let updated_at_ms = row
                .get::<_, Option<i64>>(4)?
                .or(row.get::<_, Option<i64>>(5)?);
            Ok(LocalSession {
                id: row.get(0)?,
                title: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                cwd: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                model_provider: String::new(),
                archived: row
                    .get::<_, Option<String>>(3)?
                    .map(|status| status.eq_ignore_ascii_case("archived"))
                    .unwrap_or(false),
                updated_at_ms,
                rollout_path: String::new(),
                db_path: self.db_path.to_string_lossy().to_string(),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn undo(&self, token: &str) -> DeleteResult {
        let result = (|| -> anyhow::Result<DeleteResult> {
            let backups = undo_backups(&self.backup_store, token)?;
            let session_id = backups[0]["session_id"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Backup is missing its session id"))?
                .to_string();
            restore_backups(
                &self.backup_store,
                &backups,
                &self.db_path,
                &self.allowed_db_paths,
                self.codex_home.as_deref(),
            )?;
            Ok(DeleteResult {
                status: DeleteStatus::Undone,
                session_id,
                message: "Local session restored from backup".to_string(),
                undo_token: Some(token.to_string()),
                backup_path: None,
            })
        })();
        result.unwrap_or_else(|err| failed_with_undo("", err.to_string(), token, None))
    }

    pub fn find_archived_thread_by_title(&self, title: &str) -> Option<SessionRef> {
        let db = open_read_only(&self.db_path).ok()?;
        if schema_kind(&db).ok().flatten() != Some(SchemaKind::CodexThreads)
            || !has_columns(&db, "threads", &["archived"]).ok()?
        {
            return None;
        }
        let mut stmt = db
            .prepare(
                "SELECT id, title FROM threads
                 WHERE archived = 1 AND (title = ?1 OR title LIKE ?2 OR ?1 LIKE '%' || title || '%')
                 ORDER BY archived_at DESC LIMIT 1",
            )
            .ok()?;
        let mut rows = stmt.query((title, format!("%{title}%"))).ok()?;
        let row = rows.next().ok().flatten()?;
        let id: String = row.get(0).ok()?;
        let row_title: Option<String> = row.get(1).ok()?;
        SessionRef::new(id, row_title.unwrap_or_else(|| title.to_string())).ok()
    }

    pub fn move_codex_thread_workspace(
        &self,
        session: &SessionRef,
        target_cwd: &str,
    ) -> serde_json::Value {
        if self.db_path.exists()
            && let Err(error) = recover_workspace_moves_for_db(&self.db_path)
        {
            return json!({
                "status": "failed",
                "session_id": session.session_id,
                "message": format!("检测到上次未完成的会话移动，但自动恢复失败：{error:#}；未开始新的移动")
            });
        }
        if let Some(db_parent) = self.db_path.parent()
            && let Err(error) = codex_plus_core::mirror_access::ensure_storage_headroom(
                db_parent,
                WORKSPACE_REWRITE_OVERHEAD_BYTES,
                codex_plus_core::mirror_access::MIN_SAFE_FREE_SPACE_BYTES,
            )
        {
            return json!({
                "status": "failed",
                "session_id": session.session_id,
                "message": format!("移动前数据库磁盘检查失败：{error}；未修改数据库或 rollout")
            });
        }
        self.move_codex_thread_workspace_with_headroom(
            session,
            target_cwd,
            |path, planned_bytes| {
                codex_plus_core::mirror_access::ensure_storage_headroom(
                    path,
                    planned_bytes,
                    codex_plus_core::mirror_access::MIN_SAFE_FREE_SPACE_BYTES,
                )
                .map(|_| ())
            },
        )
    }

    fn move_codex_thread_workspace_with_headroom<F>(
        &self,
        session: &SessionRef,
        target_cwd: &str,
        ensure_headroom: F,
    ) -> serde_json::Value
    where
        F: Fn(&Path, u64) -> anyhow::Result<()>,
    {
        let target = target_cwd.trim();
        if target.is_empty() {
            return json!({"status": "failed", "session_id": session.session_id, "message": "目标项目路径为空"});
        }
        if !self.db_path.exists() {
            return json!({"status": "failed", "session_id": session.session_id, "message": format!("Database not found: {}", self.db_path.to_string_lossy())});
        }
        let result = (|| -> anyhow::Result<Value> {
            let mut db = Connection::open(&self.db_path)?;
            db.busy_timeout(SQLITE_READ_BUSY_TIMEOUT)?;
            if schema_kind(&db)? != Some(SchemaKind::CodexThreads)
                || !has_columns(&db, "threads", &["cwd", "rollout_path"])?
            {
                return Ok(
                    json!({"status": "failed", "session_id": session.session_id, "message": "Unsupported local storage schema"}),
                );
            }
            let thread_id = normalize_codex_thread_id(&session.session_id);
            let timestamp_columns = codex_thread_timestamp_columns(&db)?;
            let mut columns = vec![
                "id".to_string(),
                "title".to_string(),
                "cwd".to_string(),
                "rollout_path".to_string(),
            ];
            columns.extend(timestamp_columns);
            let sql = format!("SELECT {} FROM threads WHERE id = ?1", columns.join(", "));
            let row = {
                let mut stmt = db.prepare(&sql)?;
                let row = stmt.query_row([&thread_id], |row| {
                    let mut data = Map::new();
                    for (index, column) in columns.iter().enumerate() {
                        data.insert(column.clone(), sql_value_to_json(row.get_ref(index)?));
                    }
                    Ok(data)
                });
                match row {
                    Ok(row) => row,
                    Err(rusqlite::Error::QueryReturnedNoRows) => {
                        return Ok(
                            json!({"status": "failed", "session_id": thread_id, "message": "Thread not found in local storage"}),
                        );
                    }
                    Err(err) => return Err(err.into()),
                }
            };
            let previous_cwd = row
                .get("cwd")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let rollout_path = row
                .get("rollout_path")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let mut rollout = prepare_rollout_workspace_update(
                &rollout_path,
                &thread_id,
                target,
                &ensure_headroom,
            )?;
            if let RolloutWorkspaceUpdate::Staged(stage) = &mut rollout {
                stage.prepare_rollback_link()?;
            }

            let transaction = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let current: Option<(String, String)> = transaction
                .query_row(
                    "SELECT cwd, rollout_path FROM threads WHERE id = ?1",
                    [&thread_id],
                    |row| {
                        Ok((
                            row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                            row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                        ))
                    },
                )
                .optional()?;
            if current.as_ref() != Some(&(previous_cwd.clone(), rollout_path.clone())) {
                anyhow::bail!(
                    "会话记录在移动准备期间发生变化；未修改数据库或 rollout，请刷新后重试"
                );
            }
            transaction.execute(
                "UPDATE threads SET cwd = ?1 WHERE id = ?2",
                (target, thread_id.as_str()),
            )?;

            let mut database_only_journal = None;
            if let RolloutWorkspaceUpdate::Staged(stage) = &mut rollout {
                stage.write_recovery_journal(&self.db_path, &thread_id, &previous_cwd, target)?;
                stage.replace_target().with_context(|| {
                    format!(
                        "无法原子替换 rollout {}；数据库事务已回滚，原文件保持不变",
                        stage.target.display()
                    )
                })?;
            } else if let RolloutWorkspaceUpdate::AlreadyCurrent(stage) = &mut rollout
                && previous_cwd != target
            {
                database_only_journal = Some(write_database_only_workspace_move_journal(
                    &self.db_path,
                    &thread_id,
                    &previous_cwd,
                    target,
                    &rollout_path,
                    &stage.staged,
                )?);
            }

            if let Err(commit_error) = transaction.commit() {
                if let RolloutWorkspaceUpdate::Staged(stage) = &mut rollout {
                    let database_cwd = db
                        .query_row(
                            "SELECT cwd FROM threads WHERE id = ?1",
                            [&thread_id],
                            |row| row.get::<_, Option<String>>(0),
                        )
                        .optional();
                    match database_cwd {
                        Ok(Some(database_cwd))
                            if database_cwd.as_deref().unwrap_or_default() == previous_cwd =>
                        {
                            stage.restore_original().with_context(|| {
                                format!(
                                    "数据库提交失败（{commit_error}），且恢复原 rollout 失败；恢复副本保留在 {}",
                                    stage.rollback_path_display()
                                )
                            })?;
                            let _ = stage.clear_recovery_journal();
                        }
                        Ok(Some(database_cwd))
                            if database_cwd.as_deref().unwrap_or_default() == target =>
                        {
                            anyhow::bail!(
                                "数据库提交返回错误（{commit_error}），但复查显示目标 cwd 已写入；已保留恢复 journal，下次读取时将自动核验"
                            );
                        }
                        Ok(Some(database_cwd)) => {
                            anyhow::bail!(
                                "数据库提交失败（{commit_error}），且复查到未知 cwd（{}）；已保留恢复 journal",
                                database_cwd.as_deref().unwrap_or_default()
                            );
                        }
                        Ok(None) => {
                            anyhow::bail!(
                                "数据库提交失败（{commit_error}），且复查时会话记录已不存在；已保留恢复 journal"
                            );
                        }
                        Err(recheck_error) => {
                            anyhow::bail!(
                                "数据库提交失败（{commit_error}），且无法复查提交结果（{recheck_error}）；已保留恢复 journal"
                            );
                        }
                    }
                }
                anyhow::bail!("数据库提交失败，移动未完成：{commit_error}");
            }

            let (rollout_updated, rollout_error) = match rollout {
                RolloutWorkspaceUpdate::Missing => (false, String::new()),
                RolloutWorkspaceUpdate::AlreadyCurrent(mut stage) => {
                    stage.source_lock.take();
                    let cleanup_error = match cleanup_workspace_move_artifact(&stage.staged) {
                        Ok(()) => database_only_journal.as_ref().and_then(|journal_path| {
                            cleanup_workspace_move_artifact(journal_path)
                                .err()
                                .map(|error| {
                                    format!(
                                        "仅数据库移动的恢复 journal 未清理：{}（{error:#}）",
                                        journal_path.display()
                                    )
                                })
                        }),
                        Err(error) => Some(if let Some(journal_path) = &database_only_journal {
                            format!(
                                "仅数据库移动的 staging 未清理：{}（{error:#}）；恢复 journal 已保留：{}",
                                stage.staged.display(),
                                journal_path.display()
                            )
                        } else {
                            format!(
                                "未修改内容的 rollout staging 未清理：{}（{error:#}）",
                                stage.staged.display()
                            )
                        }),
                    };
                    (false, cleanup_error.unwrap_or_default())
                }
                RolloutWorkspaceUpdate::Staged(stage) => (true, stage.finish()),
            };
            let message = if rollout_error.is_empty() {
                "已移动对话".to_string()
            } else {
                format!("已移动对话；{rollout_error}")
            };
            let mut payload = json!({
                "status": "moved",
                "session_id": thread_id,
                "message": message,
                "previous_cwd": previous_cwd,
                "target_cwd": target,
                "rollout_updated": rollout_updated,
                "rollout_error": rollout_error,
            });
            if let Some(payload) = payload.as_object_mut() {
                add_timestamp_payload(payload, &row);
                payload.insert(
                    "db_path".to_string(),
                    json!(self.db_path.to_string_lossy().to_string()),
                );
            }
            Ok(payload)
        })();
        result.unwrap_or_else(|err| json!({"status": "failed", "session_id": session.session_id, "message": format!("{err:#}")}))
    }

    pub fn codex_thread_sort_key(&self, session: &SessionRef) -> serde_json::Value {
        if !self.db_path.exists() {
            return json!({"status": "failed", "session_id": session.session_id, "message": format!("Database not found: {}", self.db_path.to_string_lossy())});
        }
        let result = (|| -> anyhow::Result<Value> {
            let db = open_read_only(&self.db_path)?;
            if schema_kind(&db)? != Some(SchemaKind::CodexThreads) {
                return Ok(
                    json!({"status": "failed", "session_id": session.session_id, "message": "Unsupported local storage schema"}),
                );
            }
            let thread_id = normalize_codex_thread_id(&session.session_id);
            match fetch_thread_timestamp_payload(&db, &thread_id)? {
                Some(mut payload) => {
                    payload.insert("status".to_string(), json!("ok"));
                    payload.insert("session_id".to_string(), json!(thread_id));
                    Ok(Value::Object(payload))
                }
                None => Ok(
                    json!({"status": "failed", "session_id": thread_id, "message": "Thread not found in local storage"}),
                ),
            }
        })();
        result.unwrap_or_else(|err| json!({"status": "failed", "session_id": session.session_id, "message": err.to_string()}))
    }

    pub fn codex_thread_sort_keys(&self, sessions: &[SessionRef]) -> serde_json::Value {
        if !self.db_path.exists() {
            return json!({"status": "failed", "message": format!("Database not found: {}", self.db_path.to_string_lossy()), "sort_keys": []});
        }
        let thread_ids = sessions
            .iter()
            .filter(|session| !session.session_id.is_empty())
            .map(|session| normalize_codex_thread_id(&session.session_id))
            .fold(Vec::<String>::new(), |mut acc, id| {
                if !acc.contains(&id) && acc.len() < 200 {
                    acc.push(id);
                }
                acc
            });
        if thread_ids.is_empty() {
            return json!({"status": "ok", "sort_keys": []});
        }
        let result = (|| -> anyhow::Result<Value> {
            let db = open_read_only(&self.db_path)?;
            if schema_kind(&db)? != Some(SchemaKind::CodexThreads) {
                return Ok(
                    json!({"status": "failed", "message": "Unsupported local storage schema", "sort_keys": []}),
                );
            }
            let mut sort_keys = Vec::new();
            for thread_id in thread_ids {
                if let Some(mut payload) = fetch_thread_timestamp_payload(&db, &thread_id)? {
                    payload.insert("session_id".to_string(), json!(thread_id));
                    sort_keys.push(Value::Object(payload));
                }
            }
            Ok(json!({"status": "ok", "sort_keys": sort_keys}))
        })();
        result.unwrap_or_else(
            |err| json!({"status": "failed", "message": err.to_string(), "sort_keys": []}),
        )
    }

    pub fn codex_thread_usage_history(&self, session: &SessionRef) -> serde_json::Value {
        if !self.db_path.exists() {
            return json!({
                "status": "failed",
                "session_id": session.session_id,
                "message": format!("Database not found: {}", self.db_path.to_string_lossy()),
                "history": []
            });
        }
        let result = (|| -> anyhow::Result<Value> {
            let db = open_read_only(&self.db_path)?;
            if schema_kind(&db)? != Some(SchemaKind::CodexThreads)
                || !has_columns(&db, "threads", &["rollout_path"])?
            {
                return Ok(json!({
                    "status": "failed",
                    "session_id": session.session_id,
                    "message": "Unsupported local storage schema",
                    "history": []
                }));
            }
            let thread_id = normalize_codex_thread_id(&session.session_id);
            let rollout_path: Option<String> = db
                .query_row(
                    "SELECT rollout_path FROM threads WHERE id = ?1",
                    [&thread_id],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(rollout_path) = rollout_path.filter(|path| !path.trim().is_empty()) else {
                return Ok(json!({
                    "status": "failed",
                    "session_id": thread_id,
                    "message": "Thread rollout path is empty",
                    "history": []
                }));
            };
            let rollout = PathBuf::from(&rollout_path);
            if !rollout.is_file() {
                return Ok(json!({
                    "status": "failed",
                    "session_id": thread_id,
                    "message": format!("rollout file not found: {rollout_path}"),
                    "history": []
                }));
            }
            let (history, truncated) = read_rollout_usage_history(&rollout, &thread_id)?;
            let mut response = json!({
                "status": "ok",
                "session_id": thread_id,
                "rollout_path": rollout_path,
                "history": history,
            });
            if truncated {
                response["historyTruncated"] = json!(true);
                response["historyNotice"] = json!(format!(
                    "历史使用量已限制为最近 {MAX_ROLLOUT_USAGE_ENTRIES} 条、最多扫描 {MAX_ROLLOUT_USAGE_SCAN_BYTES} 字节；原始会话文件未被修改。"
                ));
            }
            Ok(response)
        })();
        result.unwrap_or_else(|err| {
            json!({
                "status": "failed",
                "session_id": session.session_id,
                "message": err.to_string(),
                "history": []
            })
        })
    }

    fn delete_generic_session(
        &self,
        db: &mut Connection,
        session: &SessionRef,
    ) -> anyhow::Result<DeleteResult> {
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let sessions = select_dicts(
            &tx,
            "SELECT * FROM sessions WHERE id = ?1",
            &[&session.session_id],
        )?;
        if sessions.is_empty() {
            return Ok(failed(
                &session.session_id,
                "Session not found in local storage".to_string(),
            ));
        }
        let messages = if has_table(&tx, "messages")? {
            select_dicts(
                &tx,
                "SELECT * FROM messages WHERE session_id = ?1",
                &[&session.session_id],
            )?
        } else {
            Vec::new()
        };
        let token = self.backup_store.write_backup(
            &session.session_id,
            &self.db_path,
            json!({"sessions": sessions, "messages": messages}),
        )?;
        let backup_path = self.backup_store.path_for(&token);
        let delete_result = (|| -> anyhow::Result<()> {
            if has_table(&tx, "messages")? {
                tx.execute(
                    "DELETE FROM messages WHERE session_id = ?1",
                    [&session.session_id],
                )?;
            }
            tx.execute("DELETE FROM sessions WHERE id = ?1", [&session.session_id])?;
            tx.commit()?;
            Ok(())
        })();
        if let Err(err) = delete_result {
            return Ok(failed_with_undo(
                &session.session_id,
                err.to_string(),
                &token,
                Some(&backup_path),
            ));
        }
        Ok(local_deleted(&session.session_id, &token, &backup_path))
    }

    fn delete_codex_thread(
        &self,
        db: &mut Connection,
        session: &SessionRef,
    ) -> anyhow::Result<DeleteResult> {
        let thread_id = normalize_codex_thread_id(&session.session_id);
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let thread_rows = select_dicts(&tx, "SELECT * FROM threads WHERE id = ?1", &[&thread_id])?;
        if thread_rows.is_empty() {
            return Ok(failed(
                &session.session_id,
                "Thread not found in local storage".to_string(),
            ));
        }
        let mut tables = Map::new();
        tables.insert("threads".to_string(), Value::Array(thread_rows));
        backup_related_rows(
            &tx,
            &mut tables,
            "thread_dynamic_tools",
            "thread_id = ?1",
            &[&thread_id],
        )?;
        backup_related_rows(
            &tx,
            &mut tables,
            "thread_goals",
            "thread_id = ?1",
            &[&thread_id],
        )?;
        backup_related_rows(
            &tx,
            &mut tables,
            "thread_spawn_edges",
            "parent_thread_id = ?1 OR child_thread_id = ?1",
            &[&thread_id],
        )?;
        backup_related_rows(
            &tx,
            &mut tables,
            "stage1_outputs",
            "thread_id = ?1",
            &[&thread_id],
        )?;
        backup_related_rows(
            &tx,
            &mut tables,
            "agent_job_items",
            "assigned_thread_id = ?1",
            &[&thread_id],
        )?;
        let rollout_paths = rollout_file_paths(tables.get("threads").and_then(Value::as_array));
        if let Some(home) = self.codex_home.as_deref() {
            validate_delete_file_paths(&rollout_paths, home)?;
        }
        let session_index_lines = match self.codex_home.as_deref() {
            Some(home) => session_index_lines_for_thread(home, &thread_id)?,
            None => Vec::new(),
        };
        if !session_index_lines.is_empty() {
            tables.insert(
                "__session_index".to_string(),
                Value::Array(
                    session_index_lines
                        .iter()
                        .map(|line| Value::String(line.clone()))
                        .collect(),
                ),
            );
        }
        let mut file_snapshots = Vec::new();
        let token = if rollout_paths.is_empty() {
            self.backup_store
                .write_backup(&thread_id, &self.db_path, Value::Object(tables))?
        } else {
            let draft = self.backup_store.begin_draft()?;
            let (file_backups, snapshots) = snapshot_rollout_files(&rollout_paths, &draft)?;
            file_snapshots = snapshots;
            if !file_backups.is_empty() {
                tables.insert("__files".to_string(), Value::Array(file_backups));
            }
            draft.commit(&thread_id, &self.db_path, Value::Object(tables))?
        };
        let backup_path = self.backup_store.path_for(&token);
        if let Err(error) = verify_delete_file_snapshots(&file_snapshots) {
            return Ok(failed_with_undo(
                &thread_id,
                format!("rollout 在备份完成后发生变化，未删除任何数据：{error}"),
                &token,
                Some(&backup_path),
            ));
        }
        let delete_result = (|| -> anyhow::Result<()> {
            delete_related_rows(&tx, "thread_dynamic_tools", "thread_id = ?1", &[&thread_id])?;
            delete_related_rows(&tx, "thread_goals", "thread_id = ?1", &[&thread_id])?;
            delete_related_rows(
                &tx,
                "thread_spawn_edges",
                "parent_thread_id = ?1 OR child_thread_id = ?1",
                &[&thread_id],
            )?;
            delete_related_rows(&tx, "stage1_outputs", "thread_id = ?1", &[&thread_id])?;
            if has_table(&tx, "agent_job_items")?
                && has_columns(&tx, "agent_job_items", &["assigned_thread_id"])?
            {
                tx.execute(
                    "UPDATE agent_job_items SET assigned_thread_id = NULL WHERE assigned_thread_id = ?1",
                    [&thread_id],
                )?;
            }
            tx.execute("DELETE FROM threads WHERE id = ?1", [&thread_id])?;
            tx.commit()?;
            Ok(())
        })();
        if let Err(err) = delete_result {
            return Ok(failed_with_undo(
                &thread_id,
                err.to_string(),
                &token,
                Some(&backup_path),
            ));
        }
        let mut post_delete_errors = Vec::new();
        for snapshot in &file_snapshots {
            if let Err(error) = snapshot.verify_source_unchanged() {
                post_delete_errors.push(error.to_string());
                continue;
            }
            if let Err(error) = fs::remove_file(&snapshot.path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                post_delete_errors.push(format!("{}: {error}", snapshot.path.display()));
            }
        }
        drop(file_snapshots);
        if let Some(home) = self.codex_home.as_deref()
            && let Err(error) = remove_session_index_entry(home, &thread_id)
        {
            post_delete_errors.push(format!("session_index.jsonl 清理失败：{error}"));
        }
        if !post_delete_errors.is_empty() {
            let rollback = self.undo(&token);
            let message = if matches!(rollback.status, DeleteStatus::Undone) {
                format!(
                    "删除未完成，已自动恢复数据库、会话文件和索引：{}",
                    post_delete_errors.join("; ")
                )
            } else {
                format!(
                    "删除未完成，且自动恢复失败：{}；{}",
                    post_delete_errors.join("; "),
                    rollback.message
                )
            };
            return Ok(failed_with_undo(
                &thread_id,
                message,
                &token,
                Some(&backup_path),
            ));
        }
        Ok(local_deleted(&thread_id, &token, &backup_path))
    }

    fn delete_codex_automation_run(
        &self,
        db: &mut Connection,
        session: &SessionRef,
    ) -> anyhow::Result<DeleteResult> {
        let thread_id = normalize_codex_thread_id(&session.session_id);
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut tables = Map::new();
        backup_related_rows(
            &tx,
            &mut tables,
            "automation_runs",
            "thread_id = ?1",
            &[&thread_id],
        )?;
        backup_related_rows(
            &tx,
            &mut tables,
            "inbox_items",
            "thread_id = ?1",
            &[&thread_id],
        )?;
        if tables.values().all(|rows| {
            rows.as_array()
                .map(|items| items.is_empty())
                .unwrap_or(true)
        }) {
            return Ok(failed(
                &session.session_id,
                "Thread not found in local storage".to_string(),
            ));
        }
        let token =
            self.backup_store
                .write_backup(&thread_id, &self.db_path, Value::Object(tables))?;
        let backup_path = self.backup_store.path_for(&token);
        let delete_result = (|| -> anyhow::Result<()> {
            delete_related_rows(&tx, "automation_runs", "thread_id = ?1", &[&thread_id])?;
            delete_related_rows(&tx, "inbox_items", "thread_id = ?1", &[&thread_id])?;
            tx.commit()?;
            Ok(())
        })();
        if let Err(err) = delete_result {
            return Ok(failed_with_undo(
                &thread_id,
                err.to_string(),
                &token,
                Some(&backup_path),
            ));
        }
        Ok(local_deleted(&thread_id, &token, &backup_path))
    }
}

fn optional_column_expression<'a>(
    columns: &HashSet<String>,
    column: &'a str,
    fallback: &'a str,
) -> &'a str {
    if columns.contains(column) {
        column
    } else {
        fallback
    }
}

fn failed(session_id: &str, message: String) -> DeleteResult {
    DeleteResult {
        status: DeleteStatus::Failed,
        session_id: session_id.to_string(),
        message,
        undo_token: None,
        backup_path: None,
    }
}

fn local_deleted(session_id: &str, token: &str, backup_path: &Path) -> DeleteResult {
    DeleteResult {
        status: DeleteStatus::LocalDeleted,
        session_id: session_id.to_string(),
        message: "已从本地存储删除".to_string(),
        undo_token: Some(token.to_string()),
        backup_path: Some(backup_path.to_string_lossy().to_string()),
    }
}

fn read_rollout_usage_history(
    rollout_path: &Path,
    thread_id: &str,
) -> anyhow::Result<(Vec<Value>, bool)> {
    let mut file = File::open(rollout_path)?;
    let file_len = file.metadata()?.len();
    let scan_start = file_len.saturating_sub(MAX_ROLLOUT_USAGE_SCAN_BYTES);
    let mut discard_partial_first_line = false;
    if scan_start > 0 {
        file.seek(SeekFrom::Start(scan_start - 1))?;
        let mut previous = [0u8; 1];
        file.read_exact(&mut previous)?;
        discard_partial_first_line = previous[0] != b'\n';
        file.seek(SeekFrom::Start(scan_start))?;
    }
    let reader = BufReader::new(file);
    let mut current_turn_id = String::new();
    let mut history = VecDeque::with_capacity(MAX_ROLLOUT_USAGE_ENTRIES);
    let mut line = Vec::new();
    let mut bytes_scanned = 0u64;
    let mut truncated = scan_start > 0;

    let mut reader = reader;
    if discard_partial_first_line {
        if let Some((discarded, _)) = read_line_bounded(
            &mut reader,
            &mut line,
            MAX_ROLLOUT_USAGE_LINE_BYTES,
            MAX_ROLLOUT_USAGE_SCAN_BYTES as usize,
        )? {
            bytes_scanned = discarded as u64;
        }
    }
    while bytes_scanned < MAX_ROLLOUT_USAGE_SCAN_BYTES {
        let Some((line_bytes, line_was_truncated)) = read_line_bounded(
            &mut reader,
            &mut line,
            MAX_ROLLOUT_USAGE_LINE_BYTES,
            (MAX_ROLLOUT_USAGE_SCAN_BYTES - bytes_scanned) as usize,
        )?
        else {
            break;
        };
        bytes_scanned = bytes_scanned.saturating_add(line_bytes as u64);
        if line_was_truncated {
            truncated = true;
            continue;
        }
        let line_text = String::from_utf8_lossy(&line);
        if line_text.trim().is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_str(line_text.trim_end_matches(['\r', '\n'])) {
            Ok(value) => value,
            Err(_) => continue,
        };
        match value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "turn_context" => {
                current_turn_id = value
                    .get("payload")
                    .and_then(|payload| payload.get("turn_id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
            }
            "event_msg" => {
                let payload = match value.get("payload") {
                    Some(payload)
                        if payload.get("type").and_then(Value::as_str) == Some("token_count") =>
                    {
                        payload
                    }
                    _ => continue,
                };
                let info = match payload.get("info") {
                    Some(info) => info,
                    None => continue,
                };
                let last = info.get("last_token_usage");
                let total = info.get("total_token_usage");
                let model_context_window = info
                    .get("model_context_window")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                let input_tokens = last
                    .and_then(|usage| usage.get("input_tokens"))
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                let output_tokens = last
                    .and_then(|usage| usage.get("output_tokens"))
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                let total_tokens = last
                    .and_then(|usage| usage.get("total_tokens"))
                    .and_then(Value::as_i64)
                    .unwrap_or_else(|| {
                        total
                            .and_then(|usage| usage.get("total_tokens"))
                            .and_then(Value::as_i64)
                            .unwrap_or(0)
                    });
                let cached_tokens = last
                    .and_then(|usage| usage.get("cached_input_tokens"))
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                let context_used = total
                    .and_then(|usage| usage.get("total_tokens"))
                    .and_then(Value::as_i64)
                    .unwrap_or(total_tokens);
                if input_tokens <= 0 && output_tokens <= 0 && total_tokens <= 0 && context_used <= 0
                {
                    continue;
                }
                if history.len() == MAX_ROLLOUT_USAGE_ENTRIES {
                    history.pop_front();
                    truncated = true;
                }
                history.push_back(json!({
                    "source": "rollout-history",
                    "conversation_id": format!("local:{thread_id}"),
                    "turn_id": current_turn_id,
                    "observed_at": value.get("timestamp").and_then(Value::as_str).unwrap_or_default(),
                    "usage": {
                        "inputTokens": input_tokens,
                        "outputTokens": output_tokens,
                        "totalTokens": total_tokens,
                        "cachedTokens": cached_tokens,
                        "cacheReadTokens": 0,
                        "cacheCreationTokens": 0,
                        "contextUsed": context_used,
                        "contextLimit": model_context_window,
                        "hasBreakdown": input_tokens > 0 || output_tokens > 0 || cached_tokens > 0,
                    }
                }));
            }
            _ => {}
        }
    }
    if bytes_scanned >= MAX_ROLLOUT_USAGE_SCAN_BYTES {
        truncated = true;
    }
    Ok((history.into_iter().collect(), truncated))
}

/// Reads one JSONL record without allowing a corrupt/huge line to consume all
/// available memory. The returned byte count includes bytes discarded after
/// the cap, so the caller can enforce a total scan budget.
fn read_line_bounded<R: BufRead>(
    reader: &mut R,
    line: &mut Vec<u8>,
    max_bytes: usize,
    max_scan_bytes: usize,
) -> io::Result<Option<(usize, bool)>> {
    line.clear();
    let mut consumed = 0usize;
    let mut oversized = false;
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return if consumed == 0 {
                Ok(None)
            } else {
                Ok(Some((consumed, oversized)))
            };
        }
        let newline = buffer.iter().position(|byte| *byte == b'\n');
        let take = newline
            .map_or(buffer.len(), |index| index + 1)
            .min(max_scan_bytes.saturating_sub(consumed));
        if take == 0 {
            return Ok(Some((consumed, true)));
        }
        let remaining = max_bytes.saturating_add(1).saturating_sub(line.len());
        let copy = take.min(remaining);
        line.extend_from_slice(&buffer[..copy]);
        reader.consume(take);
        consumed = consumed.saturating_add(take);
        if line.len() > max_bytes {
            oversized = true;
        }
        if newline.is_some() && take == newline.map_or(0, |index| index + 1) {
            return Ok(Some((consumed, oversized)));
        }
        if consumed >= max_scan_bytes {
            return Ok(Some((consumed, true)));
        }
    }
}

fn failed_with_undo(
    session_id: &str,
    message: String,
    token: &str,
    backup_path: Option<&Path>,
) -> DeleteResult {
    DeleteResult {
        status: DeleteStatus::Failed,
        session_id: session_id.to_string(),
        message,
        undo_token: Some(token.to_string()),
        backup_path: backup_path.map(|path| path.to_string_lossy().to_string()),
    }
}

fn normalize_codex_thread_id(session_id: &str) -> String {
    session_id
        .strip_prefix("local:")
        .unwrap_or(session_id)
        .to_string()
}

fn undo_backups(backup_store: &BackupStore, token: &str) -> anyhow::Result<Vec<Value>> {
    let parsed =
        serde_json::from_str::<Vec<String>>(token).unwrap_or_else(|_| vec![token.to_string()]);
    if parsed.is_empty() {
        anyhow::bail!("empty undo token");
    }
    if parsed.len() > 64 {
        anyhow::bail!("too many backups in one undo operation");
    }
    let mut seen = HashSet::new();
    let tokens = parsed
        .into_iter()
        .filter(|token| seen.insert(token.clone()))
        .collect::<Vec<_>>();
    let backups = tokens
        .iter()
        .map(|token| backup_store.read_backup(token))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let session_id = backups
        .first()
        .and_then(|backup| backup.get("session_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("Backup is missing its session id"))?;
    if backups
        .iter()
        .any(|backup| backup.get("session_id").and_then(Value::as_str) != Some(session_id))
    {
        anyhow::bail!("Grouped undo contains backups for different sessions");
    }
    Ok(backups)
}

fn restore_backups(
    backup_store: &BackupStore,
    backups: &[Value],
    fallback_db_path: &Path,
    allowed_db_paths: &[PathBuf],
    codex_home: Option<&Path>,
) -> anyhow::Result<()> {
    let mut plans = Vec::new();
    let mut source_dbs = HashSet::new();
    let mut session_index_lines = Vec::new();
    for backup in backups {
        let tables = backup["tables"]
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("Backup is missing its table snapshot"))?;
        let source_db = backup_source_db(backup, fallback_db_path, allowed_db_paths)?;
        if !source_dbs.insert(source_db.clone()) {
            anyhow::bail!("Grouped undo contains more than one backup for the same database");
        }
        let db = Connection::open_with_flags(&source_db, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        validate_restore_tables(tables)?;
        let already_restored = restore_tables_exactly_present(&db, tables)?;
        if !already_restored {
            detect_restore_conflicts(&db, tables)?;
            preflight_restore_rows(&db, tables)?;
        }
        detect_file_restore_conflicts(backup_store, backup, tables)?;
        validate_restore_file_paths(tables, codex_home)?;

        let backup_session_id = backup["session_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Backup is missing its session id"))?;
        for line in validated_session_index_lines(tables)? {
            if session_index_thread_id(&line).as_deref() != Some(backup_session_id) {
                anyhow::bail!("Backup session index entry belongs to another session");
            }
            if !session_index_lines.contains(&line) {
                session_index_lines.push(line);
            }
        }
        plans.push((source_db, tables, already_restored, backup));
    }
    if !session_index_lines.is_empty() && codex_home.is_none() {
        anyhow::bail!("Cannot restore session_index.jsonl without the resolved Codex home");
    }

    let mut connections = Vec::with_capacity(plans.len());
    let pending_plans = plans
        .iter()
        .filter(|(_, _, already_restored, _)| !already_restored)
        .collect::<Vec<_>>();
    for (source_db, _, _, _) in &pending_plans {
        let db = Connection::open_with_flags(source_db, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        db.execute_batch("BEGIN IMMEDIATE")?;
        connections.push(db);
    }
    for (db, (_, tables, _, _)) in connections.iter().zip(pending_plans.iter()) {
        restore_rows(db, tables)?;
    }
    for db in &connections {
        db.execute_batch("COMMIT")?;
    }

    let mut restored_files = HashSet::new();
    for (_, tables, _, backup) in &plans {
        restore_backup_files(backup_store, backup, tables, &mut restored_files)?;
    }
    if let Some(home) = codex_home {
        restore_session_index_entries(home, &session_index_lines)?;
    }
    Ok(())
}

fn preflight_restore_rows(db: &Connection, tables: &Map<String, Value>) -> anyhow::Result<()> {
    db.execute_batch("SAVEPOINT mirror_x_restore_preflight")?;
    let restore_result = restore_rows(db, tables);
    let rollback_result = db.execute_batch(
        "ROLLBACK TO mirror_x_restore_preflight; RELEASE mirror_x_restore_preflight",
    );
    restore_result?;
    rollback_result?;
    Ok(())
}

fn restore_rows(db: &Connection, tables: &Map<String, Value>) -> anyhow::Result<()> {
    for (table, rows) in tables {
        if table.starts_with("__") {
            continue;
        }
        let Some(rows) = rows.as_array() else {
            anyhow::bail!("Backup table {table} is not an array");
        };
        for row in rows {
            let row = row
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("Backup table {table} contains a non-object row"))?;
            if table == "agent_job_items" && update_existing_agent_job_item(db, row)? {
                continue;
            }
            insert_row(db, table, row)?;
        }
    }
    Ok(())
}

fn restore_backup_files(
    backup_store: &BackupStore,
    backup: &Value,
    tables: &Map<String, Value>,
    restored_paths: &mut HashSet<String>,
) -> anyhow::Result<()> {
    let Some(files) = tables.get("__files").and_then(Value::as_array) else {
        return Ok(());
    };
    for file in files {
        let path = file
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("Backup file entry is missing its path"))?;
        if !restored_paths.insert(path.to_string()) {
            continue;
        }
        match backup_file_source(backup_store, backup, file)? {
            BackupFileSource::Inline(bytes) => {
                if Path::new(path).is_file() && fs::read(path)? == bytes {
                    continue;
                }
                codex_plus_core::settings::atomic_write(Path::new(path), &bytes)?;
            }
            BackupFileSource::Sidecar {
                path: sidecar_path,
                size,
                sha256,
            } => {
                if Path::new(path).is_file() && files_equal(Path::new(path), &sidecar_path)? {
                    continue;
                }
                copy_sidecar_atomic(&sidecar_path, Path::new(path), size, &sha256)?;
            }
        }
    }
    Ok(())
}

fn backup_source_db(
    backup: &Value,
    fallback_db_path: &Path,
    allowed_db_paths: &[PathBuf],
) -> anyhow::Result<PathBuf> {
    let source_db = backup["source_db"]
        .as_str()
        .filter(|path| !path.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| fallback_db_path.to_path_buf());
    if !source_db.is_file() {
        anyhow::bail!(
            "Backup source database not found: {}",
            source_db.to_string_lossy()
        );
    }
    let source_db = fs::canonicalize(source_db)?;
    let allowed = allowed_db_paths
        .iter()
        .filter_map(|path| fs::canonicalize(path).ok())
        .any(|path| path == source_db);
    if !allowed {
        anyhow::bail!("Backup source database is not an allowed local storage path");
    }
    Ok(source_db)
}

fn session_index_lines_for_thread(
    codex_home: &Path,
    thread_id: &str,
) -> anyhow::Result<Vec<String>> {
    let path = codex_home.join("session_index.jsonl");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = String::from_utf8(fs::read(&path)?)?;
    Ok(text
        .split_inclusive('\n')
        .map(|segment| segment.trim_end_matches(['\r', '\n']))
        .filter(|line| session_index_thread_id(line).as_deref() == Some(thread_id))
        .map(ToString::to_string)
        .collect())
}

fn remove_session_index_entry(codex_home: &Path, thread_id: &str) -> anyhow::Result<usize> {
    let path = codex_home.join("session_index.jsonl");
    if !path.exists() {
        return Ok(0);
    }
    let original_bytes = fs::read(&path)?;
    let original_text = String::from_utf8(original_bytes.clone())?;
    let mut next_text = String::with_capacity(original_text.len());
    let mut removed = 0usize;
    for segment in original_text.split_inclusive('\n') {
        let line = segment.trim_end_matches(['\r', '\n']);
        if session_index_thread_id(line).as_deref() == Some(thread_id) {
            removed += 1;
        } else {
            next_text.push_str(segment);
        }
    }
    if removed == 0 {
        return Ok(0);
    }
    if fs::read(&path)? != original_bytes {
        anyhow::bail!("session_index.jsonl changed while the session was being deleted");
    }
    codex_plus_core::settings::atomic_write(&path, next_text.as_bytes())?;
    Ok(removed)
}

fn restore_session_index_entries(codex_home: &Path, lines: &[String]) -> anyhow::Result<usize> {
    if lines.is_empty() {
        return Ok(0);
    }
    let path = codex_home.join("session_index.jsonl");
    let original_bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    let original_text = String::from_utf8(original_bytes.clone())?;
    let mut existing_ids = original_text
        .split_inclusive('\n')
        .filter_map(|segment| session_index_thread_id(segment.trim_end_matches(['\r', '\n'])))
        .collect::<HashSet<_>>();
    let newline = if original_text.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut next_text = original_text;
    if !next_text.is_empty() && !next_text.ends_with('\n') {
        next_text.push_str(newline);
    }
    let mut appended = 0usize;
    for line in lines {
        let thread_id = session_index_thread_id(line)
            .ok_or_else(|| anyhow::anyhow!("Backup contains an invalid session index entry"))?;
        if !existing_ids.insert(thread_id) {
            continue;
        }
        next_text.push_str(line.trim_end_matches(['\r', '\n']));
        next_text.push_str(newline);
        appended += 1;
    }
    if appended == 0 {
        return Ok(0);
    }
    let current_bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    if current_bytes != original_bytes {
        anyhow::bail!("session_index.jsonl changed while the session was being restored");
    }
    codex_plus_core::settings::atomic_write(&path, next_text.as_bytes())?;
    Ok(appended)
}

fn validated_session_index_lines(tables: &Map<String, Value>) -> anyhow::Result<Vec<String>> {
    let Some(entries) = tables.get("__session_index") else {
        return Ok(Vec::new());
    };
    let entries = entries
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Backup session index snapshot is not an array"))?;
    entries
        .iter()
        .map(|entry| {
            let line = entry
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Backup session index entry is not text"))?;
            if session_index_thread_id(line).is_none() {
                anyhow::bail!("Backup contains an invalid session index entry");
            }
            Ok(line.to_string())
        })
        .collect()
}

fn session_index_thread_id(line: &str) -> Option<String> {
    if line.trim().is_empty() {
        return None;
    }
    serde_json::from_str::<Value>(line)
        .ok()?
        .get("id")?
        .as_str()
        .filter(|id| !id.trim().is_empty())
        .map(ToString::to_string)
}

fn schema_kind(db: &Connection) -> anyhow::Result<Option<SchemaKind>> {
    if has_table(db, "sessions")? && has_columns(db, "sessions", &["id", "title"])? {
        if has_table(db, "messages")? && !has_columns(db, "messages", &["session_id"])? {
            return Ok(None);
        }
        return Ok(Some(SchemaKind::GenericSessions));
    }
    if has_table(db, "threads")? && has_columns(db, "threads", &["id", "title", "rollout_path"])? {
        return Ok(Some(SchemaKind::CodexThreads));
    }
    if has_table(db, "automation_runs")? && has_columns(db, "automation_runs", &["thread_id"])? {
        return Ok(Some(SchemaKind::CodexAutomationRuns));
    }
    Ok(None)
}

fn has_table(db: &Connection, table: &str) -> anyhow::Result<bool> {
    Ok(db
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |_| Ok(()),
        )
        .is_ok())
}

fn has_columns(db: &Connection, table: &str, columns: &[&str]) -> anyhow::Result<bool> {
    let existing: HashSet<String> = table_columns(db, table)?.into_iter().collect();
    Ok(columns.iter().all(|column| existing.contains(*column)))
}

fn table_columns(db: &Connection, table: &str) -> anyhow::Result<Vec<String>> {
    let mut stmt = db.prepare(&format!(
        "PRAGMA table_info(\"{}\")",
        table.replace('"', "\"\"")
    ))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn select_dicts(db: &Connection, sql: &str, params: &[&dyn ToSql]) -> anyhow::Result<Vec<Value>> {
    let mut stmt = db.prepare(sql)?;
    let columns: Vec<String> = stmt
        .column_names()
        .iter()
        .map(|name| name.to_string())
        .collect();
    let rows = stmt.query_map(params, |row| {
        let mut data = Map::new();
        for (index, column) in columns.iter().enumerate() {
            data.insert(column.clone(), sql_value_to_json(row.get_ref(index)?));
        }
        Ok(Value::Object(data))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn validate_restore_tables(tables: &Map<String, Value>) -> anyhow::Result<()> {
    let allowed = [
        "sessions",
        "messages",
        "threads",
        "thread_dynamic_tools",
        "thread_goals",
        "thread_spawn_edges",
        "stage1_outputs",
        "agent_job_items",
        "automation_runs",
        "inbox_items",
        "__files",
        "__session_index",
    ];
    for table in tables.keys() {
        if !allowed.contains(&table.as_str()) {
            anyhow::bail!("unknown restore table: {table}");
        }
    }
    Ok(())
}

fn detect_restore_conflicts(db: &Connection, tables: &Map<String, Value>) -> anyhow::Result<()> {
    for (table, rows) in tables {
        if table.starts_with("__") {
            continue;
        }
        let Some(rows) = rows.as_array() else {
            continue;
        };
        for row in rows {
            let Some(row) = row.as_object() else {
                continue;
            };
            if restore_row_conflicts(db, table, row)? {
                anyhow::bail!("restore conflict: {table} row already exists");
            }
        }
    }
    Ok(())
}

fn restore_tables_exactly_present(
    db: &Connection,
    tables: &Map<String, Value>,
) -> anyhow::Result<bool> {
    let mut saw_row = false;
    for (table, rows) in tables {
        if table.starts_with("__") {
            continue;
        }
        let Some(rows) = rows.as_array() else {
            return Ok(false);
        };
        if !rows.is_empty() && !has_table(db, table)? {
            return Ok(false);
        }
        for row in rows {
            let Some(row) = row.as_object() else {
                return Ok(false);
            };
            if row.is_empty() {
                return Ok(false);
            }
            saw_row = true;
            let required_count = rows
                .iter()
                .filter(|candidate| candidate.as_object() == Some(row))
                .count() as i64;
            if count_exact_restore_rows(db, table, row)? < required_count {
                return Ok(false);
            }
        }
    }
    Ok(saw_row)
}

fn count_exact_restore_rows(
    db: &Connection,
    table: &str,
    row: &Map<String, Value>,
) -> anyhow::Result<i64> {
    let columns = row.keys().collect::<Vec<_>>();
    let where_clause = columns
        .iter()
        .enumerate()
        .map(|(index, column)| format!("\"{}\" IS ?{}", column.replace('"', "\"\""), index + 1))
        .collect::<Vec<_>>()
        .join(" AND ");
    let values = columns
        .iter()
        .map(|column| OwnedSqlValue(json_to_sql_value(&row[*column])))
        .collect::<Vec<_>>();
    let refs = values
        .iter()
        .map(|value| value as &dyn ToSql)
        .collect::<Vec<_>>();
    Ok(db.query_row(
        &format!("SELECT COUNT(*) FROM \"{table}\" WHERE {where_clause}"),
        refs.as_slice(),
        |query_row| query_row.get(0),
    )?)
}

fn restore_row_conflicts(
    db: &Connection,
    table: &str,
    row: &Map<String, Value>,
) -> anyhow::Result<bool> {
    let key_columns = restore_conflict_key_columns(table, row);
    if key_columns.is_empty() || !has_table(db, table)? {
        return Ok(false);
    }
    let where_clause = key_columns
        .iter()
        .enumerate()
        .map(|(index, column)| format!("\"{}\" = ?{}", column.replace('"', "\"\""), index + 1))
        .collect::<Vec<_>>()
        .join(" AND ");
    let values = key_columns
        .iter()
        .map(|column| OwnedSqlValue(json_to_sql_value(&row[*column])))
        .collect::<Vec<_>>();
    let refs = values
        .iter()
        .map(|value| value as &dyn ToSql)
        .collect::<Vec<_>>();
    Ok(db
        .query_row(
            &format!("SELECT 1 FROM \"{table}\" WHERE {where_clause} LIMIT 1"),
            refs.as_slice(),
            |_| Ok(()),
        )
        .is_ok())
}

fn restore_conflict_key_columns<'a>(table: &str, row: &'a Map<String, Value>) -> Vec<&'a String> {
    let wanted: &[&str] = match table {
        "sessions" | "threads" => &["id"],
        "messages" => &["id"],
        "automation_runs" | "inbox_items" => &["thread_id"],
        "thread_dynamic_tools" => &["thread_id", "tool_name"],
        "thread_goals" => &["thread_id", "goal"],
        "thread_spawn_edges" => &["parent_thread_id", "child_thread_id"],
        "stage1_outputs" => &["thread_id"],
        _ => &[],
    };
    let keys = wanted
        .iter()
        .filter_map(|column| row.get_key_value(*column).map(|(key, _)| key))
        .collect::<Vec<_>>();
    if table == "messages" && keys.is_empty() {
        row.get_key_value("session_id")
            .map(|(key, _)| vec![key])
            .unwrap_or_default()
    } else {
        keys
    }
}

fn detect_file_restore_conflicts(
    backup_store: &BackupStore,
    backup: &Value,
    tables: &Map<String, Value>,
) -> anyhow::Result<()> {
    let Some(files) = tables.get("__files").and_then(Value::as_array) else {
        return Ok(());
    };
    let allowed_paths = allowed_backup_file_paths(tables);
    for file in files {
        let path = file
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("Backup file entry is missing its path"))?;
        if !allowed_paths.contains(path) {
            anyhow::bail!("unexpected backup file path: {path}");
        }
        let source = backup_file_source(backup_store, backup, file)?;
        if Path::new(path).exists() {
            let matches = match source {
                BackupFileSource::Inline(bytes) => fs::read(path)? == bytes,
                BackupFileSource::Sidecar {
                    path: sidecar_path, ..
                } => files_equal(Path::new(path), &sidecar_path)?,
            };
            if !matches {
                anyhow::bail!(
                    "restore conflict: file already exists with different contents: {path}"
                );
            }
        }
    }
    Ok(())
}

enum BackupFileSource {
    Inline(Vec<u8>),
    Sidecar {
        path: PathBuf,
        size: u64,
        sha256: String,
    },
}

fn backup_file_source(
    backup_store: &BackupStore,
    backup: &Value,
    file: &Value,
) -> anyhow::Result<BackupFileSource> {
    let inline = file.get("content_b64").and_then(Value::as_str);
    let sidecar = file.get("sidecar").and_then(Value::as_str);
    match (inline, sidecar) {
        (Some(_), Some(_)) => anyhow::bail!("Backup file entry has ambiguous content sources"),
        (Some(content), None) => Ok(BackupFileSource::Inline(base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            content,
        )?)),
        (None, Some(file_name)) => {
            let token = backup
                .get("token")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("Backup is missing its token"))?;
            let size = file
                .get("size")
                .and_then(Value::as_u64)
                .ok_or_else(|| anyhow::anyhow!("Backup sidecar entry is missing its size"))?;
            let sha256 = file
                .get("sha256")
                .and_then(Value::as_str)
                .filter(|value| value.len() == 64 && value.chars().all(|ch| ch.is_ascii_hexdigit()))
                .ok_or_else(|| anyhow::anyhow!("Backup sidecar entry has an invalid checksum"))?
                .to_ascii_lowercase();
            let path = backup_store.sidecar_path(token, file_name)?;
            let metadata = fs::metadata(&path)
                .with_context(|| format!("Backup sidecar not found: {}", path.display()))?;
            if !metadata.is_file() || metadata.len() != size {
                anyhow::bail!(
                    "Backup sidecar size does not match metadata: {}",
                    path.display()
                );
            }
            let actual_sha256 = sha256_file(&path)?;
            if actual_sha256 != sha256 {
                anyhow::bail!("Backup sidecar checksum mismatch: {}", path.display());
            }
            Ok(BackupFileSource::Sidecar { path, size, sha256 })
        }
        (None, None) => anyhow::bail!("Backup file entry is missing its content"),
    }
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let mut file = File::open(path)?;
    let (_, sha256) = copy_with_sha256(&mut file, &mut io::sink())?;
    Ok(sha256)
}

fn files_equal(first: &Path, second: &Path) -> anyhow::Result<bool> {
    if fs::metadata(first)?.len() != fs::metadata(second)?.len() {
        return Ok(false);
    }
    let mut first = BufReader::new(File::open(first)?);
    let mut second = BufReader::new(File::open(second)?);
    let mut first_buffer = [0u8; 64 * 1024];
    let mut second_buffer = [0u8; 64 * 1024];
    loop {
        let first_count = first.read(&mut first_buffer)?;
        let second_count = second.read(&mut second_buffer)?;
        if first_count != second_count
            || first_buffer[..first_count] != second_buffer[..second_count]
        {
            return Ok(false);
        }
        if first_count == 0 {
            return Ok(true);
        }
    }
}

fn copy_sidecar_atomic(
    source: &Path,
    target: &Path,
    expected_size: u64,
    expected_sha256: &str,
) -> anyhow::Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Restore target has no parent: {}", target.display()))?;
    fs::create_dir_all(parent)?;
    let file_name = target
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("rollout");
    let staged = target.with_file_name(format!(".{file_name}.{}.restore", Uuid::new_v4()));
    let result = (|| -> anyhow::Result<()> {
        let mut source_file = File::open(source)?;
        let mut staged_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged)?;
        let (copied, actual_sha256) = copy_with_sha256(&mut source_file, &mut staged_file)?;
        if copied != expected_size || actual_sha256 != expected_sha256 {
            anyhow::bail!("Backup sidecar changed while it was being restored");
        }
        staged_file.sync_all()?;
        drop(staged_file);
        if let Ok(metadata) = fs::metadata(source) {
            let _ = fs::set_permissions(&staged, metadata.permissions());
        }
        codex_plus_core::settings::replace_temp_path(&staged, target)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&staged);
    }
    result
}

fn validate_restore_file_paths(
    tables: &Map<String, Value>,
    codex_home: Option<&Path>,
) -> anyhow::Result<()> {
    let Some(files) = tables.get("__files").and_then(Value::as_array) else {
        return Ok(());
    };
    if files.is_empty() {
        return Ok(());
    }
    let codex_home = codex_home.ok_or_else(|| {
        anyhow::anyhow!("Cannot restore session files without the resolved Codex home")
    })?;
    let canonical_home = fs::canonicalize(codex_home)?;
    for file in files {
        let path = file
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("Backup file entry is missing its path"))?;
        let path = Path::new(path);
        let resolved_path = if path.exists() {
            fs::canonicalize(path)?
        } else {
            let parent = path.parent().ok_or_else(|| {
                anyhow::anyhow!(
                    "Backup file path has no parent directory: {}",
                    path.display()
                )
            })?;
            fs::canonicalize(parent)?.join(path.file_name().ok_or_else(|| {
                anyhow::anyhow!("Backup file path has no file name: {}", path.display())
            })?)
        };
        if !resolved_path.starts_with(&canonical_home) {
            anyhow::bail!(
                "Refusing to restore a session file outside the resolved Codex home: {}",
                resolved_path.to_string_lossy()
            );
        }
    }
    Ok(())
}

fn allowed_backup_file_paths(tables: &Map<String, Value>) -> HashSet<String> {
    tables
        .get("threads")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| row.get("rollout_path").and_then(Value::as_str))
        .filter(|path| !path.trim().is_empty())
        .map(ToString::to_string)
        .collect()
}

fn insert_row(db: &Connection, table: &str, row: &Map<String, Value>) -> anyhow::Result<()> {
    let columns: Vec<&String> = row.keys().collect();
    if columns.is_empty() {
        return Ok(());
    }
    let quoted = columns
        .iter()
        .map(|column| format!("\"{}\"", column.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(", ");
    let marks = (0..columns.len())
        .map(|index| format!("?{}", index + 1))
        .collect::<Vec<_>>()
        .join(", ");
    let values = columns
        .iter()
        .map(|column| OwnedSqlValue(json_to_sql_value(&row[*column])))
        .collect::<Vec<_>>();
    let refs = values
        .iter()
        .map(|value| value as &dyn ToSql)
        .collect::<Vec<_>>();
    db.execute(
        &format!("INSERT INTO \"{table}\" ({quoted}) VALUES ({marks})"),
        refs.as_slice(),
    )?;
    Ok(())
}

fn update_existing_agent_job_item(
    db: &Connection,
    row: &Map<String, Value>,
) -> anyhow::Result<bool> {
    let Some(id) = row.get("id") else {
        return Ok(false);
    };
    if !row.contains_key("assigned_thread_id") || !has_table(db, "agent_job_items")? {
        return Ok(false);
    }
    let id_value = OwnedSqlValue(json_to_sql_value(id));
    let current_assignment = db.query_row(
        "SELECT assigned_thread_id FROM agent_job_items WHERE id = ?1 LIMIT 1",
        [&id_value as &dyn ToSql],
        |row| row.get::<_, Option<String>>(0),
    );
    let current_assignment = match current_assignment {
        Ok(value) => value,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(false),
        Err(err) => return Err(err.into()),
    };
    if current_assignment.is_some() {
        anyhow::bail!("restore conflict: agent_job_items row already assigned");
    }
    let assigned = OwnedSqlValue(json_to_sql_value(&row["assigned_thread_id"]));
    db.execute(
        "UPDATE agent_job_items SET assigned_thread_id = ?1 WHERE id = ?2 AND assigned_thread_id IS NULL",
        [&assigned as &dyn ToSql, &id_value as &dyn ToSql],
    )?;
    Ok(true)
}

fn backup_related_rows(
    db: &Connection,
    tables: &mut Map<String, Value>,
    table: &str,
    where_clause: &str,
    params: &[&dyn ToSql],
) -> anyhow::Result<()> {
    if has_table(db, table)? {
        let rows = select_dicts(
            db,
            &format!("SELECT * FROM \"{table}\" WHERE {where_clause}"),
            params,
        )?;
        tables.insert(table.to_string(), Value::Array(rows));
    }
    Ok(())
}

fn delete_related_rows(
    db: &Connection,
    table: &str,
    where_clause: &str,
    params: &[&dyn ToSql],
) -> anyhow::Result<()> {
    if has_table(db, table)? {
        db.execute(
            &format!("DELETE FROM \"{table}\" WHERE {where_clause}"),
            params,
        )?;
    }
    Ok(())
}

fn rollout_file_paths(thread_rows: Option<&Vec<Value>>) -> Vec<String> {
    let mut seen = HashSet::new();
    thread_rows
        .into_iter()
        .flatten()
        .filter_map(|row| row.get("rollout_path").and_then(Value::as_str))
        .map(str::trim)
        .filter(|path| !path.is_empty() && seen.insert((*path).to_string()))
        .map(ToString::to_string)
        .collect()
}

struct DeleteFileSnapshot {
    path: PathBuf,
    source_lock: File,
    signature: FileSignature,
}

impl DeleteFileSnapshot {
    fn verify_source_unchanged(&self) -> anyhow::Result<()> {
        let path_metadata = fs::metadata(&self.path)
            .with_context(|| format!("无法重新检查 rollout {}", self.path.display()))?;
        let handle_metadata = self
            .source_lock
            .metadata()
            .with_context(|| format!("无法读取已锁定 rollout {} 的元数据", self.path.display()))?;
        if !self.signature.matches(&path_metadata) || !self.signature.matches(&handle_metadata) {
            anyhow::bail!(
                "rollout {} 在删除备份期间发生变化；原文件和数据库均未删除",
                self.path.display()
            );
        }
        Ok(())
    }
}

fn snapshot_rollout_files(
    paths: &[String],
    draft: &BackupDraft,
) -> anyhow::Result<(Vec<Value>, Vec<DeleteFileSnapshot>)> {
    let mut snapshots = Vec::new();
    for path in paths {
        let path = PathBuf::from(path);
        let file = match OpenOptions::new().read(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "无法读取 rollout {}，未删除数据库或会话文件",
                        path.display()
                    )
                });
            }
        };
        file.try_lock_exclusive().with_context(|| {
            format!(
                "rollout {} 正在被 Codex 或其他进程使用；请先结束相关任务再删除",
                path.display()
            )
        })?;
        let signature = FileSignature::from_metadata(&file.metadata()?);
        snapshots.push(DeleteFileSnapshot {
            path,
            source_lock: file,
            signature,
        });
    }

    let planned_bytes = snapshots
        .iter()
        .map(|snapshot| snapshot.signature.len)
        .sum::<u64>();
    codex_plus_core::mirror_access::ensure_storage_headroom(
        draft.sidecar_dir(),
        planned_bytes,
        codex_plus_core::mirror_access::MIN_SAFE_FREE_SPACE_BYTES,
    )
    .context("删除前无法为完整会话备份预留足够磁盘空间")?;

    let mut entries = Vec::with_capacity(snapshots.len());
    for (index, snapshot) in snapshots.iter_mut().enumerate() {
        let sidecar_name = format!("rollout-{index:04}.bin");
        let sidecar_path = draft.sidecar_path(&sidecar_name)?;
        let mut sidecar = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&sidecar_path)
            .with_context(|| format!("无法创建会话备份文件 {}", sidecar_path.display()))?;
        snapshot.source_lock.seek(SeekFrom::Start(0))?;
        let (copied, sha256) = copy_with_sha256(&mut snapshot.source_lock, &mut sidecar)
            .with_context(|| format!("无法完整备份 rollout {}", snapshot.path.display()))?;
        sidecar.sync_all()?;
        if copied != snapshot.signature.len {
            anyhow::bail!(
                "rollout {} 备份长度不一致；未删除数据库或原文件",
                snapshot.path.display()
            );
        }
        if let Ok(metadata) = fs::metadata(&snapshot.path) {
            let _ = fs::set_permissions(&sidecar_path, metadata.permissions());
        }
        snapshot.verify_source_unchanged()?;
        entries.push(json!({
            "path": snapshot.path.to_string_lossy(),
            "sidecar": sidecar_name,
            "size": copied,
            "sha256": sha256,
        }));
    }
    Ok((entries, snapshots))
}

fn verify_delete_file_snapshots(snapshots: &[DeleteFileSnapshot]) -> anyhow::Result<()> {
    for snapshot in snapshots {
        snapshot.verify_source_unchanged()?;
    }
    Ok(())
}

fn copy_with_sha256<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
) -> io::Result<(u64, String)> {
    let mut hasher = Sha256::new();
    let mut copied = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        writer.write_all(&buffer[..count])?;
        hasher.update(&buffer[..count]);
        copied = copied.saturating_add(count as u64);
    }
    Ok((copied, format!("{:x}", hasher.finalize())))
}

fn validate_delete_file_paths(paths: &[String], codex_home: &Path) -> anyhow::Result<()> {
    let canonical_home = fs::canonicalize(codex_home)?;
    for path in paths {
        let path = Path::new(path);
        let resolved_path = if path.exists() {
            fs::canonicalize(path)?
        } else {
            let parent = path.parent().ok_or_else(|| {
                anyhow::anyhow!("Session rollout path has no parent: {}", path.display())
            })?;
            fs::canonicalize(parent)?.join(path.file_name().ok_or_else(|| {
                anyhow::anyhow!("Session rollout path has no file name: {}", path.display())
            })?)
        };
        if !resolved_path.starts_with(&canonical_home) {
            anyhow::bail!(
                "Refusing to delete a session file outside the resolved Codex home: {}",
                resolved_path.to_string_lossy()
            );
        }
    }
    Ok(())
}

enum RolloutWorkspaceUpdate {
    Missing,
    AlreadyCurrent(RolloutWorkspaceStage),
    Staged(RolloutWorkspaceStage),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceMoveJournal {
    version: u32,
    #[serde(default)]
    database_only: bool,
    db_path: PathBuf,
    thread_id: String,
    previous_cwd: String,
    target_cwd: String,
    rollout_path: PathBuf,
    staged_path: PathBuf,
    rollback_path: PathBuf,
}

#[derive(Clone, Copy)]
struct FileSignature {
    len: u64,
    modified: Option<SystemTime>,
}

impl FileSignature {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
        }
    }

    fn matches(self, metadata: &fs::Metadata) -> bool {
        self.len == metadata.len()
            && match (self.modified, metadata.modified().ok()) {
                (Some(expected), Some(current)) => expected == current,
                _ => true,
            }
    }
}

struct RolloutWorkspaceStage {
    target: PathBuf,
    staged: PathBuf,
    rollback: Option<PathBuf>,
    source_signature: FileSignature,
    source_lock: Option<File>,
    retain_rollback: bool,
    journal: Option<PathBuf>,
}

impl RolloutWorkspaceStage {
    fn verify_source_unchanged(&self) -> anyhow::Result<()> {
        let metadata = fs::metadata(&self.target)
            .with_context(|| format!("无法重新检查 rollout {}", self.target.display()))?;
        if !self.source_signature.matches(&metadata) {
            anyhow::bail!(
                "rollout {} 在移动准备期间发生变化；未修改数据库或原文件，请停止当前任务后重试",
                self.target.display()
            );
        }
        Ok(())
    }

    fn prepare_rollback_link(&mut self) -> anyhow::Result<()> {
        self.verify_source_unchanged()?;
        let rollback = unique_sibling_path(&self.target, "workspace-move-rollback");
        if fs::hard_link(&self.target, &rollback).is_ok() {
            self.rollback = Some(rollback);
            return Ok(());
        }

        let parent = self.target.parent().unwrap_or_else(|| Path::new("."));
        codex_plus_core::mirror_access::ensure_storage_headroom(
            parent,
            self.source_signature.len,
            codex_plus_core::mirror_access::MIN_SAFE_FREE_SPACE_BYTES,
        )
        .context("文件系统不支持 hard-link，且创建流式回滚副本的空间不足")?;
        let rollback_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&rollback)
            .with_context(|| format!("无法创建 rollout 回滚副本 {}", rollback.display()))?;
        self.rollback = Some(rollback.clone());
        let copy_result = (|| -> anyhow::Result<()> {
            let mut source = self
                .source_lock
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("rollout 源文件锁已丢失"))?
                .try_clone()?;
            source.seek(SeekFrom::Start(0))?;
            let mut reader = BufReader::new(source);
            let mut writer = BufWriter::new(rollback_file);
            io::copy(&mut reader, &mut writer)?;
            writer.flush()?;
            let rollback_file = writer.into_inner().map_err(|error| error.into_error())?;
            rollback_file.sync_all()?;
            if let Ok(metadata) = fs::metadata(&self.target) {
                let _ = fs::set_permissions(&rollback, metadata.permissions());
            }
            Ok(())
        })();
        copy_result.with_context(|| {
            format!(
                "创建 rollout 流式回滚副本失败 {}；未修改数据库或原文件",
                rollback.display()
            )
        })?;
        Ok(())
    }

    fn replace_target(&mut self) -> anyhow::Result<()> {
        self.verify_source_unchanged()?;
        codex_plus_core::settings::replace_temp_path(&self.staged, &self.target)?;
        Ok(())
    }

    fn write_recovery_journal(
        &mut self,
        db_path: &Path,
        thread_id: &str,
        previous_cwd: &str,
        target_cwd: &str,
    ) -> anyhow::Result<()> {
        let rollback_path = self
            .rollback
            .clone()
            .ok_or_else(|| anyhow::anyhow!("rollout 回滚副本不存在，无法创建恢复 journal"))?;
        let journal_path = unique_workspace_move_journal_path(db_path);
        let journal = WorkspaceMoveJournal {
            version: WORKSPACE_MOVE_JOURNAL_VERSION,
            database_only: false,
            db_path: db_path.to_path_buf(),
            thread_id: thread_id.to_string(),
            previous_cwd: previous_cwd.to_string(),
            target_cwd: target_cwd.to_string(),
            rollout_path: self.target.clone(),
            staged_path: self.staged.clone(),
            rollback_path,
        };
        let bytes = serde_json::to_vec_pretty(&journal)?;
        codex_plus_core::settings::atomic_write(&journal_path, &bytes).with_context(|| {
            format!(
                "无法持久化会话移动恢复 journal {}；未替换 rollout",
                journal_path.display()
            )
        })?;
        self.journal = Some(journal_path);
        Ok(())
    }

    fn restore_original(&mut self) -> anyhow::Result<()> {
        let rollback = self
            .rollback
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("rollout 回滚副本不存在"))?;
        if let Err(error) = codex_plus_core::settings::replace_temp_path(rollback, &self.target) {
            self.retain_rollback = true;
            return Err(error.into());
        }
        self.rollback = None;
        Ok(())
    }

    fn rollback_path_display(&self) -> String {
        self.rollback
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "<missing>".to_string())
    }

    fn clear_recovery_journal(&mut self) -> anyhow::Result<()> {
        let Some(journal) = self.journal.as_ref() else {
            return Ok(());
        };
        match fs::remove_file(journal) {
            Ok(()) => self.journal = None,
            Err(error) if error.kind() == io::ErrorKind::NotFound => self.journal = None,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("无法清理会话移动恢复 journal {}", journal.display())
                });
            }
        }
        Ok(())
    }

    fn finish(mut self) -> String {
        self.source_lock.take();
        let mut errors = Vec::new();
        if let Some(rollback) = self.rollback.as_ref() {
            match fs::remove_file(rollback) {
                Ok(()) => self.rollback = None,
                Err(error) if error.kind() == io::ErrorKind::NotFound => self.rollback = None,
                Err(error) => errors.push(format!(
                    "临时回滚链接清理失败：{}（{}）",
                    rollback.display(),
                    error
                )),
            }
        }
        if self.rollback.is_none()
            && let Err(error) = self.clear_recovery_journal()
        {
            errors.push(error.to_string());
        }
        if errors.is_empty() {
            String::new()
        } else {
            format!(
                "移动已完成，但恢复临时文件未完全清理：{}",
                errors.join("；")
            )
        }
    }
}

impl Drop for RolloutWorkspaceStage {
    fn drop(&mut self) {
        if self.journal.is_some() {
            return;
        }
        let _ = fs::remove_file(&self.staged);
        if !self.retain_rollback
            && let Some(rollback) = &self.rollback
        {
            let _ = fs::remove_file(rollback);
        }
    }
}

fn write_database_only_workspace_move_journal(
    db_path: &Path,
    thread_id: &str,
    previous_cwd: &str,
    target_cwd: &str,
    rollout_path: &str,
    staged_path: &Path,
) -> anyhow::Result<PathBuf> {
    let journal_path = unique_workspace_move_journal_path(db_path);
    let journal = WorkspaceMoveJournal {
        version: WORKSPACE_MOVE_JOURNAL_VERSION,
        database_only: true,
        db_path: db_path.to_path_buf(),
        thread_id: thread_id.to_string(),
        previous_cwd: previous_cwd.to_string(),
        target_cwd: target_cwd.to_string(),
        rollout_path: PathBuf::from(rollout_path),
        staged_path: staged_path.to_path_buf(),
        rollback_path: PathBuf::new(),
    };
    codex_plus_core::settings::atomic_write(&journal_path, &serde_json::to_vec_pretty(&journal)?)
        .with_context(|| {
        format!(
            "无法持久化仅数据库移动的恢复 journal {}",
            journal_path.display()
        )
    })?;
    Ok(journal_path)
}

fn recover_workspace_moves_for_db(db_path: &Path) -> anyhow::Result<()> {
    for journal_path in workspace_move_journal_paths(db_path)? {
        recover_workspace_move_journal(db_path, &journal_path)
            .with_context(|| format!("无法恢复会话移动 journal {}", journal_path.display()))?;
    }
    Ok(())
}

fn workspace_move_journal_paths(db_path: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let parent = db_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("数据库路径没有父目录：{}", db_path.display()))?;
    let file_name = db_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state.sqlite");
    let prefix = format!(".{file_name}.workspace-move-journal.");
    let entries = fs::read_dir(parent)
        .with_context(|| format!("无法扫描数据库目录中的恢复 journal：{}", parent.display()))?;
    let mut journals = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| {
            format!("扫描数据库目录中的恢复 journal 失败：{}", parent.display())
        })?;
        let path = entry.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".json"))
        {
            journals.push(path);
        }
    }
    journals.sort();
    Ok(journals)
}

fn recover_workspace_move_journal(db_path: &Path, journal_path: &Path) -> anyhow::Result<()> {
    validate_workspace_move_journal_path(db_path, journal_path)?;
    let (journal, journal_file) = match read_workspace_move_journal(journal_path) {
        Ok(journal) => journal,
        Err(error)
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|error| error.kind() == io::ErrorKind::NotFound) =>
        {
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    validate_workspace_move_journal(db_path, &journal)?;

    let mut db = Connection::open(db_path)?;
    db.busy_timeout(SQLITE_READ_BUSY_TIMEOUT)?;
    let transaction = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current: Option<(String, String)> = transaction
        .query_row(
            "SELECT cwd, rollout_path FROM threads WHERE id = ?1",
            [&journal.thread_id],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                    row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                ))
            },
        )
        .optional()?;
    let Some((database_cwd, database_rollout_path)) = current else {
        anyhow::bail!(
            "恢复 journal 对应的会话 {} 已不存在；已保留恢复文件",
            journal.thread_id
        );
    };
    if !paths_refer_to_same_file(Path::new(&database_rollout_path), &journal.rollout_path) {
        anyhow::bail!(
            "会话 {} 的 rollout 路径已变化；已保留恢复文件",
            journal.thread_id
        );
    }

    let (rollout_file, rollout_cwd) = match File::open(&journal.rollout_path) {
        Ok(file) => {
            FileExt::try_lock_exclusive(&file).with_context(|| {
                format!(
                    "rollout {} 正在被 Codex 或其他进程使用；恢复 journal 已保留",
                    journal.rollout_path.display()
                )
            })?;
            let cwd =
                rollout_workspace_cwd_from_reader(&mut BufReader::new(&file), &journal.thread_id)?
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "rollout {} 中未找到会话 {} 的 session_meta；恢复 journal 已保留",
                            journal.rollout_path.display(),
                            journal.thread_id
                        )
                    })?;
            (Some(file), Some(cwd))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => (None, None),
        Err(error) => {
            return Err(error).with_context(|| {
                format!("恢复时无法读取 rollout {}", journal.rollout_path.display())
            });
        }
    };

    if journal.database_only {
        if rollout_cwd.as_deref() != Some(journal.target_cwd.as_str()) {
            anyhow::bail!(
                "仅数据库移动要求 rollout 保持目标 cwd（{}），当前为 {:?}；恢复 journal 已保留",
                journal.target_cwd,
                rollout_cwd
            );
        }
        if database_cwd == journal.previous_cwd {
            let updated = transaction.execute(
                "UPDATE threads SET cwd = ?1 WHERE id = ?2 AND cwd = ?3",
                (
                    journal.target_cwd.as_str(),
                    journal.thread_id.as_str(),
                    journal.previous_cwd.as_str(),
                ),
            )?;
            if updated != 1 {
                anyhow::bail!("仅数据库移动前滚失败；恢复 journal 已保留");
            }
        } else if database_cwd != journal.target_cwd {
            anyhow::bail!(
                "仅数据库移动遇到未知数据库 cwd（{}）；恢复 journal 已保留",
                database_cwd
            );
        }
        transaction.commit()?;
        drop(rollout_file);
        cleanup_workspace_move_artifact(&journal.staged_path)?;
        drop(journal_file);
        cleanup_workspace_move_artifact(journal_path)?;
        return Ok(());
    }

    match (database_cwd.as_str(), rollout_cwd.as_deref()) {
        (db, Some(file)) if db == journal.previous_cwd && file == journal.previous_cwd => {}
        (db, Some(file)) if db == journal.target_cwd && file == journal.target_cwd => {}
        (db, Some(_)) if journal.previous_cwd == journal.target_cwd && db == journal.target_cwd => {
            let staged_cwd = rollout_workspace_cwd(&journal.staged_path, &journal.thread_id)?;
            if staged_cwd.as_deref() != Some(journal.target_cwd.as_str()) {
                anyhow::bail!(
                    "数据库已是目标 cwd，但用于修复 rollout 的 staging 缺失或不匹配；恢复 journal 已保留"
                );
            }
            codex_plus_core::settings::replace_temp_path(
                &journal.staged_path,
                &journal.rollout_path,
            )
            .context("数据库已是目标 cwd，补齐 rollout 修复失败")?;
        }
        (db, Some(file)) if db == journal.previous_cwd && file == journal.target_cwd => {
            let rollback_cwd = rollout_workspace_cwd(&journal.rollback_path, &journal.thread_id)?;
            if rollback_cwd.as_deref() != Some(journal.previous_cwd.as_str()) {
                anyhow::bail!(
                    "数据库尚未提交，但 rollout 回滚副本缺失或不匹配；恢复 journal 已保留"
                );
            }
            codex_plus_core::settings::replace_temp_path(
                &journal.rollback_path,
                &journal.rollout_path,
            )
            .context("数据库未提交，恢复原 rollout 失败")?;
        }
        (db, Some(file)) if db == journal.target_cwd && file == journal.previous_cwd => {
            let staged_cwd = rollout_workspace_cwd(&journal.staged_path, &journal.thread_id)?;
            if staged_cwd.as_deref() == Some(journal.target_cwd.as_str()) {
                codex_plus_core::settings::replace_temp_path(
                    &journal.staged_path,
                    &journal.rollout_path,
                )
                .context("数据库已提交，补齐新 rollout 失败")?;
            } else {
                let updated = transaction.execute(
                    "UPDATE threads SET cwd = ?1 WHERE id = ?2 AND cwd = ?3",
                    (
                        journal.previous_cwd.as_str(),
                        journal.thread_id.as_str(),
                        journal.target_cwd.as_str(),
                    ),
                )?;
                if updated != 1 {
                    anyhow::bail!("新 rollout staging 缺失，且数据库回退失败；恢复 journal 已保留");
                }
            }
        }
        (db, None) if db == journal.previous_cwd => {
            let rollback_cwd = rollout_workspace_cwd(&journal.rollback_path, &journal.thread_id)?;
            if rollback_cwd.as_deref() != Some(journal.previous_cwd.as_str()) {
                anyhow::bail!("rollout 目标缺失，且回滚副本缺失或不匹配；恢复 journal 已保留");
            }
            codex_plus_core::settings::replace_temp_path(
                &journal.rollback_path,
                &journal.rollout_path,
            )
            .context("数据库未提交，补回原 rollout 失败")?;
        }
        (db, None) if db == journal.target_cwd => {
            let staged_cwd = rollout_workspace_cwd(&journal.staged_path, &journal.thread_id)?;
            if staged_cwd.as_deref() == Some(journal.target_cwd.as_str()) {
                codex_plus_core::settings::replace_temp_path(
                    &journal.staged_path,
                    &journal.rollout_path,
                )
                .context("数据库已提交，补齐缺失 rollout 失败")?;
            } else {
                let rollback_cwd =
                    rollout_workspace_cwd(&journal.rollback_path, &journal.thread_id)?;
                if rollback_cwd.as_deref() != Some(journal.previous_cwd.as_str()) {
                    anyhow::bail!(
                        "rollout 目标和新 staging 均缺失，且回滚副本不可用；恢复 journal 已保留"
                    );
                }
                codex_plus_core::settings::replace_temp_path(
                    &journal.rollback_path,
                    &journal.rollout_path,
                )
                .context("新 rollout 缺失，补回原 rollout 失败")?;
                let updated = transaction.execute(
                    "UPDATE threads SET cwd = ?1 WHERE id = ?2 AND cwd = ?3",
                    (
                        journal.previous_cwd.as_str(),
                        journal.thread_id.as_str(),
                        journal.target_cwd.as_str(),
                    ),
                )?;
                if updated != 1 {
                    anyhow::bail!("补回原 rollout 后数据库回退失败；恢复 journal 已保留");
                }
            }
        }
        (db, Some(file)) if db == file => {}
        (_, None) => {
            anyhow::bail!(
                "rollout 目标缺失，且数据库 cwd（{}）不符合恢复 journal；已保留恢复文件",
                database_cwd
            );
        }
        (_, Some(rollout_cwd)) => {
            anyhow::bail!(
                "数据库 cwd（{}）与 rollout cwd（{}）均不符合恢复 journal；已保留恢复文件",
                database_cwd,
                rollout_cwd
            );
        }
    }

    transaction.commit()?;
    drop(rollout_file);
    cleanup_workspace_move_artifact(&journal.staged_path)?;
    cleanup_workspace_move_artifact(&journal.rollback_path)?;
    drop(journal_file);
    cleanup_workspace_move_artifact(journal_path)?;
    Ok(())
}

fn read_workspace_move_journal(
    journal_path: &Path,
) -> anyhow::Result<(WorkspaceMoveJournal, File)> {
    let lock_deadline = std::time::Instant::now() + WORKSPACE_MOVE_JOURNAL_LOCK_TIMEOUT;
    let mut file = loop {
        match OpenOptions::new().read(true).write(true).open(journal_path) {
            Ok(file) => break file,
            Err(error)
                if workspace_move_lock_is_contended(&error)
                    && std::time::Instant::now() < lock_deadline =>
            {
                std::thread::sleep(WORKSPACE_MOVE_JOURNAL_LOCK_RETRY);
            }
            Err(error) => return Err(error.into()),
        }
    };
    loop {
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => break,
            Err(error)
                if workspace_move_lock_is_contended(&error)
                    && std::time::Instant::now() < lock_deadline =>
            {
                std::thread::sleep(WORKSPACE_MOVE_JOURNAL_LOCK_RETRY);
            }
            Err(error) => {
                return Err(error).context("恢复 journal 正在被另一个进程处理");
            }
        }
    }
    let metadata = file.metadata()?;
    if metadata.len() > MAX_WORKSPACE_MOVE_JOURNAL_BYTES {
        anyhow::bail!("恢复 journal 超过允许大小");
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    Read::by_ref(&mut file)
        .take(MAX_WORKSPACE_MOVE_JOURNAL_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_WORKSPACE_MOVE_JOURNAL_BYTES {
        anyhow::bail!("恢复 journal 超过允许大小");
    }
    let journal = serde_json::from_slice(&bytes).context("恢复 journal JSON 无效")?;
    Ok((journal, file))
}

fn workspace_move_lock_is_contended(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::WouldBlock || matches!(error.raw_os_error(), Some(32 | 33))
}

fn validate_workspace_move_journal_path(db_path: &Path, journal_path: &Path) -> anyhow::Result<()> {
    let db_parent = db_path.parent().unwrap_or_else(|| Path::new("."));
    let journal_parent = journal_path.parent().unwrap_or_else(|| Path::new("."));
    let same_parent = matches!(
        (
            fs::canonicalize(db_parent),
            fs::canonicalize(journal_parent)
        ),
        (Ok(db_parent), Ok(journal_parent)) if db_parent == journal_parent
    );
    if !same_parent {
        anyhow::bail!("恢复 journal 不在数据库目录内");
    }
    let db_name = db_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state.sqlite");
    let expected_prefix = format!(".{db_name}.workspace-move-journal.");
    let valid_name = journal_path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(&expected_prefix) && name.ends_with(".json"));
    if !valid_name {
        anyhow::bail!("恢复 journal 文件名不符合预期");
    }
    Ok(())
}

fn validate_workspace_move_journal(
    db_path: &Path,
    journal: &WorkspaceMoveJournal,
) -> anyhow::Result<()> {
    if journal.version != WORKSPACE_MOVE_JOURNAL_VERSION {
        anyhow::bail!("不支持的恢复 journal 版本：{}", journal.version);
    }
    if !paths_refer_to_same_file(db_path, &journal.db_path) {
        anyhow::bail!("恢复 journal 指向其他数据库");
    }
    if journal.thread_id.trim().is_empty() || journal.rollout_path.as_os_str().is_empty() {
        anyhow::bail!("恢复 journal 内容不符合预期");
    }
    if journal.database_only {
        if journal.previous_cwd == journal.target_cwd
            || !journal.rollback_path.as_os_str().is_empty()
            || !workspace_move_artifact_path_matches(
                &journal.rollout_path,
                &journal.staged_path,
                "workspace-move-stage",
            )
        {
            anyhow::bail!("仅数据库移动 journal 内容不符合预期");
        }
    } else if !workspace_move_artifact_path_matches(
        &journal.rollout_path,
        &journal.staged_path,
        "workspace-move-stage",
    ) || !workspace_move_artifact_path_matches(
        &journal.rollout_path,
        &journal.rollback_path,
        "workspace-move-rollback",
    ) {
        anyhow::bail!("恢复 journal 内容不符合预期");
    }
    Ok(())
}

fn workspace_move_artifact_path_matches(target: &Path, artifact: &Path, label: &str) -> bool {
    let target_parent = target.parent().unwrap_or_else(|| Path::new("."));
    let artifact_parent = artifact.parent().unwrap_or_else(|| Path::new("."));
    let same_parent = matches!(
        (
            fs::canonicalize(target_parent),
            fs::canonicalize(artifact_parent)
        ),
        (Ok(target_parent), Ok(artifact_parent)) if target_parent == artifact_parent
    );
    if !same_parent {
        return false;
    }
    let target_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("rollout.jsonl");
    let expected_prefix = format!(".{target_name}.{label}.");
    artifact
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(&expected_prefix) && name.ends_with(".tmp"))
}

fn paths_refer_to_same_file(left: &Path, right: &Path) -> bool {
    left == right
        || matches!(
            (fs::canonicalize(left), fs::canonicalize(right)),
            (Ok(left), Ok(right)) if left == right
        )
}

fn rollout_workspace_cwd(path: &Path, thread_id: &str) -> anyhow::Result<Option<String>> {
    if !path.is_file() {
        return Ok(None);
    }
    let mut reader = BufReader::new(File::open(path)?);
    rollout_workspace_cwd_from_reader(&mut reader, thread_id)
}

fn rollout_workspace_cwd_from_reader<R: BufRead>(
    reader: &mut R,
    thread_id: &str,
) -> anyhow::Result<Option<String>> {
    let mut line = Vec::with_capacity(8 * 1024);
    loop {
        line.clear();
        let mut oversized = false;
        let mut read_any = false;
        loop {
            let buffer = reader.fill_buf()?;
            if buffer.is_empty() {
                if !read_any || oversized {
                    return Ok(None);
                }
                return Ok(workspace_cwd_from_meta_line(&line, thread_id));
            }
            let newline = buffer.iter().position(|byte| *byte == b'\n');
            let take = newline.map_or(buffer.len(), |index| index + 1);
            read_any = true;
            if !oversized && line.len().saturating_add(take) <= MAX_WORKSPACE_META_LINE_BYTES {
                line.extend_from_slice(&buffer[..take]);
            } else {
                oversized = true;
                line.clear();
            }
            reader.consume(take);
            if newline.is_some() {
                if !oversized && let Some(cwd) = workspace_cwd_from_meta_line(&line, thread_id) {
                    return Ok(Some(cwd));
                }
                break;
            }
        }
    }
}

fn workspace_cwd_from_meta_line(line: &[u8], thread_id: &str) -> Option<String> {
    let body = line
        .strip_suffix(b"\r\n")
        .or_else(|| line.strip_suffix(b"\n"))
        .unwrap_or(line);
    let item = serde_json::from_slice::<Value>(body).ok()?;
    let payload = item.get("payload")?;
    (item.get("type").and_then(Value::as_str) == Some("session_meta")
        && payload.get("id").and_then(Value::as_str) == Some(thread_id))
    .then(|| {
        payload
            .get("cwd")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    })
}

fn cleanup_workspace_move_artifact(path: &Path) -> anyhow::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("无法清理会话移动恢复文件 {}", path.display()))
        }
    }
}

fn prepare_rollout_workspace_update<F>(
    rollout_path: &str,
    thread_id: &str,
    target_cwd: &str,
    ensure_headroom: &F,
) -> anyhow::Result<RolloutWorkspaceUpdate>
where
    F: Fn(&Path, u64) -> anyhow::Result<()>,
{
    if rollout_path.trim().is_empty() {
        return Ok(RolloutWorkspaceUpdate::Missing);
    }
    let target = PathBuf::from(rollout_path);
    if !target.is_file() {
        return Ok(RolloutWorkspaceUpdate::Missing);
    }
    let parent = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("rollout 路径没有父目录：{}", target.display()))?;
    let source = File::open(&target)
        .with_context(|| format!("无法读取 rollout {}；未修改数据库", target.display()))?;
    let source_metadata = source.metadata()?;
    let source_signature = FileSignature::from_metadata(&source_metadata);
    ensure_headroom(
        parent,
        source_signature
            .len
            .saturating_add(WORKSPACE_REWRITE_OVERHEAD_BYTES),
    )?;
    FileExt::try_lock_exclusive(&source).with_context(|| {
        format!(
            "rollout {} 正在被 Codex 或其他进程使用；未修改数据库或原文件",
            target.display()
        )
    })?;

    let staged = unique_sibling_path(&target, "workspace-move-stage");
    let staged_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staged)
        .with_context(|| format!("无法创建 rollout staging 文件 {}", staged.display()))?;
    let stage = RolloutWorkspaceStage {
        target,
        staged,
        rollback: None,
        source_signature,
        source_lock: Some(source),
        retain_rollback: false,
        journal: None,
    };
    let (matched, changed) = {
        let mut reader = BufReader::new(
            stage
                .source_lock
                .as_ref()
                .expect("workspace rollout source lock"),
        );
        let mut writer = BufWriter::new(staged_file);
        let result =
            stream_rollout_workspace_update(&mut reader, &mut writer, thread_id, target_cwd)?;
        writer.flush()?;
        let staged_file = writer.into_inner().map_err(|error| error.into_error())?;
        staged_file.sync_all()?;
        result
    };
    if !matched {
        anyhow::bail!(
            "rollout {} 中未找到会话 {} 的 session_meta；未修改数据库或原文件",
            stage.target.display(),
            thread_id
        );
    }
    stage.verify_source_unchanged()?;
    let _ = fs::set_permissions(&stage.staged, source_metadata.permissions());
    if !changed {
        return Ok(RolloutWorkspaceUpdate::AlreadyCurrent(stage));
    }
    Ok(RolloutWorkspaceUpdate::Staged(stage))
}

fn stream_rollout_workspace_update<R: BufRead, W: Write>(
    reader: &mut R,
    writer: &mut W,
    thread_id: &str,
    target_cwd: &str,
) -> anyhow::Result<(bool, bool)> {
    let mut matched = false;
    let mut changed = false;
    let mut line = Vec::with_capacity(8 * 1024);
    loop {
        line.clear();
        let mut oversized = false;
        let mut read_any = false;
        loop {
            let buffer = reader.fill_buf()?;
            if buffer.is_empty() {
                if !read_any {
                    return Ok((matched, changed));
                }
                if !oversized {
                    let result = write_workspace_meta_line(writer, &line, thread_id, target_cwd)?;
                    matched |= result.0;
                    changed |= result.1;
                }
                return Ok((matched, changed));
            }
            let newline = buffer.iter().position(|byte| *byte == b'\n');
            let take = newline.map_or(buffer.len(), |index| index + 1);
            read_any = true;
            if !oversized && line.len().saturating_add(take) <= MAX_WORKSPACE_META_LINE_BYTES {
                line.extend_from_slice(&buffer[..take]);
            } else {
                if !oversized {
                    writer.write_all(&line)?;
                    line.clear();
                    oversized = true;
                }
                writer.write_all(&buffer[..take])?;
            }
            reader.consume(take);
            if newline.is_some() {
                if !oversized {
                    let result = write_workspace_meta_line(writer, &line, thread_id, target_cwd)?;
                    matched |= result.0;
                    changed |= result.1;
                }
                break;
            }
        }
    }
}

fn write_workspace_meta_line<W: Write>(
    writer: &mut W,
    line: &[u8],
    thread_id: &str,
    target_cwd: &str,
) -> anyhow::Result<(bool, bool)> {
    let (body, ending): (&[u8], &[u8]) = if line.ends_with(b"\r\n") {
        (&line[..line.len() - 2], b"\r\n")
    } else if line.ends_with(b"\n") {
        (&line[..line.len() - 1], b"\n")
    } else {
        (line, b"")
    };
    let Ok(mut item) = serde_json::from_slice::<Value>(body) else {
        writer.write_all(line)?;
        return Ok((false, false));
    };
    let is_match = item.get("type").and_then(Value::as_str) == Some("session_meta")
        && item
            .get("payload")
            .and_then(|payload| payload.get("id"))
            .and_then(Value::as_str)
            == Some(thread_id);
    if !is_match {
        writer.write_all(line)?;
        return Ok((false, false));
    }
    let already_current = item
        .get("payload")
        .and_then(|payload| payload.get("cwd"))
        .and_then(Value::as_str)
        == Some(target_cwd);
    if already_current {
        writer.write_all(line)?;
        return Ok((true, false));
    }
    let payload = item
        .get_mut("payload")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| anyhow::anyhow!("session_meta payload 不是对象"))?;
    payload.insert("cwd".to_string(), json!(target_cwd));
    serde_json::to_writer(&mut *writer, &item)?;
    writer.write_all(ending)?;
    Ok((true, true))
}

fn unique_sibling_path(target: &Path, label: &str) -> PathBuf {
    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("rollout.jsonl");
    target.with_file_name(format!(".{file_name}.{label}.{}.tmp", Uuid::new_v4()))
}

fn unique_workspace_move_journal_path(db_path: &Path) -> PathBuf {
    let file_name = db_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state.sqlite");
    db_path.with_file_name(format!(
        ".{file_name}.workspace-move-journal.{}.json",
        Uuid::new_v4()
    ))
}

fn codex_thread_timestamp_columns(db: &Connection) -> anyhow::Result<Vec<String>> {
    let existing: HashSet<String> = table_columns(db, "threads")?.into_iter().collect();
    Ok(["updated_at", "updated_at_ms", "created_at_ms"]
        .iter()
        .filter(|column| existing.contains(**column))
        .map(|column| column.to_string())
        .collect())
}

fn fetch_thread_timestamp_payload(
    db: &Connection,
    thread_id: &str,
) -> anyhow::Result<Option<Map<String, Value>>> {
    let timestamp_columns = codex_thread_timestamp_columns(db)?;
    let mut columns = vec!["id".to_string()];
    columns.extend(timestamp_columns);
    let sql = format!("SELECT {} FROM threads WHERE id = ?1", columns.join(", "));
    let mut stmt = db.prepare(&sql)?;
    let row = stmt.query_row([thread_id], |row| {
        let mut selected = Map::new();
        for (index, column) in columns.iter().enumerate() {
            selected.insert(column.clone(), sql_value_to_json(row.get_ref(index)?));
        }
        Ok(selected)
    });
    match row {
        Ok(row) => {
            let mut payload = Map::new();
            add_timestamp_payload(&mut payload, &row);
            Ok(Some(payload))
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn add_timestamp_payload(payload: &mut Map<String, Value>, row: &Map<String, Value>) {
    for column in ["updated_at", "updated_at_ms", "created_at_ms"] {
        payload.insert(
            column.to_string(),
            row.get(column).cloned().unwrap_or(Value::Null),
        );
    }
}

fn sql_value_to_json(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(value) => json!(value),
        ValueRef::Real(value) => json!(value),
        ValueRef::Text(value) => json!(String::from_utf8_lossy(value).to_string()),
        ValueRef::Blob(value) => json!(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            value
        )),
    }
}

fn json_to_sql_value(value: &Value) -> SqlValue {
    match value {
        Value::Null => SqlValue::Null,
        Value::Bool(value) => SqlValue::Integer(i64::from(*value)),
        Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                SqlValue::Integer(value)
            } else if let Some(value) = number.as_f64() {
                SqlValue::Real(value)
            } else {
                SqlValue::Text(number.to_string())
            }
        }
        Value::String(value) => SqlValue::Text(value.clone()),
        other => SqlValue::Text(other.to_string()),
    }
}

#[cfg(test)]
mod workspace_move_tests {
    use super::*;

    fn create_workspace_thread_db(db_path: &Path, rollout_path: &Path) {
        let db = Connection::open(db_path).unwrap();
        db.execute(
            "CREATE TABLE threads (
                id TEXT PRIMARY KEY,
                rollout_path TEXT,
                title TEXT,
                cwd TEXT,
                updated_at INTEGER
            )",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO threads VALUES ('t1', ?1, 'Thread', '/old/project', 1)",
            [rollout_path.to_string_lossy().to_string()],
        )
        .unwrap();
    }

    #[test]
    fn workspace_move_low_space_preflight_leaves_db_and_rollout_unchanged() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("state.sqlite");
        let rollout_path = temp.path().join("rollout.jsonl");
        let original =
            b"{\"type\":\"session_meta\",\"payload\":{\"id\":\"t1\",\"cwd\":\"/old/project\"}}\n";
        fs::write(&rollout_path, original).unwrap();
        create_workspace_thread_db(&db_path, &rollout_path);
        let adapter =
            SQLiteStorageAdapter::new(&db_path, BackupStore::new(temp.path().join("backups")));

        let result = adapter.move_codex_thread_workspace_with_headroom(
            &SessionRef::new("local:t1", "Thread").unwrap(),
            "/new/project",
            |_path, _planned| anyhow::bail!("模拟磁盘空间不足"),
        );

        assert_eq!(result["status"], "failed");
        assert!(result["message"].as_str().unwrap().contains("磁盘空间不足"));
        let db = Connection::open(&db_path).unwrap();
        let cwd: String = db
            .query_row("SELECT cwd FROM threads WHERE id = 't1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(cwd, "/old/project");
        assert_eq!(fs::read(&rollout_path).unwrap(), original);
        assert_eq!(
            fs::read_dir(temp.path())
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

    #[test]
    fn workspace_move_detects_rollout_append_during_staging() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("state.sqlite");
        let rollout_path = temp.path().join("rollout.jsonl");
        let original =
            b"{\"type\":\"session_meta\",\"payload\":{\"id\":\"t1\",\"cwd\":\"/old/project\"}}\n";
        let appended = b"{\"type\":\"event_msg\",\"payload\":{\"message\":\"new while moving\"}}\n";
        fs::write(&rollout_path, original).unwrap();
        create_workspace_thread_db(&db_path, &rollout_path);
        let adapter =
            SQLiteStorageAdapter::new(&db_path, BackupStore::new(temp.path().join("backups")));

        let result = adapter.move_codex_thread_workspace_with_headroom(
            &SessionRef::new("local:t1", "Thread").unwrap(),
            "/new/project",
            |_path, _planned| {
                OpenOptions::new()
                    .append(true)
                    .open(&rollout_path)?
                    .write_all(appended)?;
                Ok(())
            },
        );

        assert_eq!(result["status"], "failed");
        assert!(result["message"].as_str().unwrap().contains("发生变化"));
        let db = Connection::open(&db_path).unwrap();
        let cwd: String = db
            .query_row("SELECT cwd FROM threads WHERE id = 't1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(cwd, "/old/project");
        assert_eq!(
            fs::read(&rollout_path).unwrap(),
            [original.as_slice(), appended.as_slice()].concat()
        );
        assert_eq!(
            fs::read_dir(temp.path())
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

    #[test]
    fn workspace_move_does_not_overwrite_database_change_during_staging() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("state.sqlite");
        let rollout_path = temp.path().join("rollout.jsonl");
        let original = workspace_rollout("/old/project");
        fs::write(&rollout_path, &original).unwrap();
        create_workspace_thread_db(&db_path, &rollout_path);
        let adapter =
            SQLiteStorageAdapter::new(&db_path, BackupStore::new(temp.path().join("backups")));

        let result = adapter.move_codex_thread_workspace_with_headroom(
            &SessionRef::new("local:t1", "Thread").unwrap(),
            "/new/project",
            |_path, _planned| {
                Connection::open(&db_path)?.execute(
                    "UPDATE threads SET cwd = '/external/project' WHERE id = 't1'",
                    [],
                )?;
                Ok(())
            },
        );

        assert_eq!(result["status"], "failed");
        assert!(result["message"].as_str().unwrap().contains("发生变化"));
        assert_eq!(workspace_db_cwd(&db_path), "/external/project");
        assert_eq!(fs::read(&rollout_path).unwrap(), original);
        assert_workspace_move_artifacts_cleaned(temp.path());
    }

    #[test]
    fn workspace_move_commit_busy_restores_rollout_and_cleans_journal() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("state.sqlite");
        let rollout_path = temp.path().join("rollout.jsonl");
        let original = workspace_rollout("/old/project");
        fs::write(&rollout_path, &original).unwrap();
        create_workspace_thread_db(&db_path, &rollout_path);

        let reader = Connection::open(&db_path).unwrap();
        reader.execute_batch("BEGIN").unwrap();
        let _: String = reader
            .query_row("SELECT cwd FROM threads WHERE id = 't1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        let adapter =
            SQLiteStorageAdapter::new(&db_path, BackupStore::new(temp.path().join("backups")));

        let result = adapter.move_codex_thread_workspace_with_headroom(
            &SessionRef::new("local:t1", "Thread").unwrap(),
            "/new/project",
            |_path, _planned| Ok(()),
        );

        assert_eq!(result["status"], "failed");
        assert!(
            result["message"]
                .as_str()
                .unwrap()
                .contains("数据库提交失败")
        );
        assert_eq!(workspace_db_cwd(&db_path), "/old/project");
        assert_eq!(fs::read(&rollout_path).unwrap(), original);
        assert_workspace_move_artifacts_cleaned(temp.path());
        reader.execute_batch("ROLLBACK").unwrap();
    }

    fn workspace_rollout(cwd: &str) -> Vec<u8> {
        format!(
            "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"t1\",\"cwd\":{}}}}}\n",
            serde_json::to_string(cwd).unwrap()
        )
        .into_bytes()
    }

    fn write_workspace_move_journal_fixture(
        db_path: &Path,
        rollout_path: &Path,
        staged_path: &Path,
        rollback_path: &Path,
    ) -> PathBuf {
        write_workspace_move_journal_fixture_with_cwds(
            db_path,
            rollout_path,
            staged_path,
            rollback_path,
            "/old/project",
            "/new/project",
        )
    }

    fn write_workspace_move_journal_fixture_with_cwds(
        db_path: &Path,
        rollout_path: &Path,
        staged_path: &Path,
        rollback_path: &Path,
        previous_cwd: &str,
        target_cwd: &str,
    ) -> PathBuf {
        let journal_path = unique_workspace_move_journal_path(db_path);
        let journal = WorkspaceMoveJournal {
            version: WORKSPACE_MOVE_JOURNAL_VERSION,
            database_only: false,
            db_path: db_path.to_path_buf(),
            thread_id: "t1".to_string(),
            previous_cwd: previous_cwd.to_string(),
            target_cwd: target_cwd.to_string(),
            rollout_path: rollout_path.to_path_buf(),
            staged_path: staged_path.to_path_buf(),
            rollback_path: rollback_path.to_path_buf(),
        };
        codex_plus_core::settings::atomic_write(
            &journal_path,
            &serde_json::to_vec_pretty(&journal).unwrap(),
        )
        .unwrap();
        journal_path
    }

    fn assert_workspace_move_artifacts_cleaned(directory: &Path) {
        assert_eq!(
            fs::read_dir(directory)
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

    fn workspace_db_cwd(db_path: &Path) -> String {
        Connection::open(db_path)
            .unwrap()
            .query_row("SELECT cwd FROM threads WHERE id = 't1'", [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    #[test]
    fn workspace_move_startup_recovery_restores_rollout_when_db_was_not_committed() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("state.sqlite");
        let rollout_path = temp.path().join("rollout.jsonl");
        fs::write(&rollout_path, workspace_rollout("/old/project")).unwrap();
        create_workspace_thread_db(&db_path, &rollout_path);

        let staged_path = unique_sibling_path(&rollout_path, "workspace-move-stage");
        let rollback_path = unique_sibling_path(&rollout_path, "workspace-move-rollback");
        fs::write(&staged_path, workspace_rollout("/new/project")).unwrap();
        fs::copy(&rollout_path, &rollback_path).unwrap();
        write_workspace_move_journal_fixture(&db_path, &rollout_path, &staged_path, &rollback_path);
        codex_plus_core::settings::replace_temp_path(&staged_path, &rollout_path).unwrap();

        let adapter =
            SQLiteStorageAdapter::new(&db_path, BackupStore::new(temp.path().join("backups")));
        let sessions = adapter.list_local_sessions_limited(10).unwrap();

        assert_eq!(sessions[0].cwd, "/old/project");
        assert_eq!(
            rollout_workspace_cwd(&rollout_path, "t1").unwrap(),
            Some("/old/project".to_string())
        );
        assert_workspace_move_artifacts_cleaned(temp.path());
        adapter.list_local_sessions_limited(10).unwrap();
        assert_workspace_move_artifacts_cleaned(temp.path());
    }

    #[test]
    fn workspace_move_recovery_cleans_journal_left_before_rollout_replace() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("state.sqlite");
        let rollout_path = temp.path().join("rollout.jsonl");
        fs::write(&rollout_path, workspace_rollout("/old/project")).unwrap();
        create_workspace_thread_db(&db_path, &rollout_path);

        let staged_path = unique_sibling_path(&rollout_path, "workspace-move-stage");
        let rollback_path = unique_sibling_path(&rollout_path, "workspace-move-rollback");
        fs::write(&staged_path, workspace_rollout("/new/project")).unwrap();
        fs::copy(&rollout_path, &rollback_path).unwrap();
        write_workspace_move_journal_fixture(&db_path, &rollout_path, &staged_path, &rollback_path);

        recover_workspace_moves_for_db(&db_path).unwrap();

        assert_eq!(workspace_db_cwd(&db_path), "/old/project");
        assert_eq!(
            rollout_workspace_cwd(&rollout_path, "t1").unwrap(),
            Some("/old/project".to_string())
        );
        assert_workspace_move_artifacts_cleaned(temp.path());
    }

    #[test]
    fn workspace_move_startup_recovery_finishes_rollout_when_db_was_committed_first() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("state.sqlite");
        let rollout_path = temp.path().join("rollout.jsonl");
        fs::write(&rollout_path, workspace_rollout("/old/project")).unwrap();
        create_workspace_thread_db(&db_path, &rollout_path);

        let staged_path = unique_sibling_path(&rollout_path, "workspace-move-stage");
        let rollback_path = unique_sibling_path(&rollout_path, "workspace-move-rollback");
        fs::write(&staged_path, workspace_rollout("/new/project")).unwrap();
        fs::copy(&rollout_path, &rollback_path).unwrap();
        write_workspace_move_journal_fixture(&db_path, &rollout_path, &staged_path, &rollback_path);
        Connection::open(&db_path)
            .unwrap()
            .execute(
                "UPDATE threads SET cwd = '/new/project' WHERE id = 't1'",
                [],
            )
            .unwrap();

        let adapter =
            SQLiteStorageAdapter::new(&db_path, BackupStore::new(temp.path().join("backups")));
        let sessions = adapter.list_local_sessions_limited(10).unwrap();

        assert_eq!(sessions[0].cwd, "/new/project");
        assert_eq!(
            rollout_workspace_cwd(&rollout_path, "t1").unwrap(),
            Some("/new/project".to_string())
        );
        assert_workspace_move_artifacts_cleaned(temp.path());
        adapter.list_local_sessions_limited(10).unwrap();
        assert_workspace_move_artifacts_cleaned(temp.path());
    }

    #[test]
    fn workspace_move_startup_recovery_rolls_back_db_when_staging_is_missing() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("state.sqlite");
        let rollout_path = temp.path().join("rollout.jsonl");
        fs::write(&rollout_path, workspace_rollout("/old/project")).unwrap();
        create_workspace_thread_db(&db_path, &rollout_path);

        let staged_path = unique_sibling_path(&rollout_path, "workspace-move-stage");
        let rollback_path = unique_sibling_path(&rollout_path, "workspace-move-rollback");
        fs::copy(&rollout_path, &rollback_path).unwrap();
        write_workspace_move_journal_fixture(&db_path, &rollout_path, &staged_path, &rollback_path);
        Connection::open(&db_path)
            .unwrap()
            .execute(
                "UPDATE threads SET cwd = '/new/project' WHERE id = 't1'",
                [],
            )
            .unwrap();

        let adapter =
            SQLiteStorageAdapter::new(&db_path, BackupStore::new(temp.path().join("backups")));
        let sessions = adapter.list_local_sessions_limited(10).unwrap();

        assert_eq!(sessions[0].cwd, "/old/project");
        assert_eq!(
            rollout_workspace_cwd(&rollout_path, "t1").unwrap(),
            Some("/old/project".to_string())
        );
        assert_workspace_move_artifacts_cleaned(temp.path());
    }

    #[test]
    fn workspace_move_recovery_restores_missing_rollout_before_db_commit() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("state.sqlite");
        let rollout_path = temp.path().join("rollout.jsonl");
        fs::write(&rollout_path, workspace_rollout("/old/project")).unwrap();
        create_workspace_thread_db(&db_path, &rollout_path);

        let staged_path = unique_sibling_path(&rollout_path, "workspace-move-stage");
        let rollback_path = unique_sibling_path(&rollout_path, "workspace-move-rollback");
        fs::write(&staged_path, workspace_rollout("/new/project")).unwrap();
        fs::copy(&rollout_path, &rollback_path).unwrap();
        write_workspace_move_journal_fixture(&db_path, &rollout_path, &staged_path, &rollback_path);
        fs::remove_file(&rollout_path).unwrap();

        recover_workspace_moves_for_db(&db_path).unwrap();

        assert_eq!(workspace_db_cwd(&db_path), "/old/project");
        assert_eq!(
            rollout_workspace_cwd(&rollout_path, "t1").unwrap(),
            Some("/old/project".to_string())
        );
        assert_workspace_move_artifacts_cleaned(temp.path());
    }

    #[test]
    fn workspace_move_recovery_finishes_missing_rollout_after_db_commit() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("state.sqlite");
        let rollout_path = temp.path().join("rollout.jsonl");
        fs::write(&rollout_path, workspace_rollout("/old/project")).unwrap();
        create_workspace_thread_db(&db_path, &rollout_path);

        let staged_path = unique_sibling_path(&rollout_path, "workspace-move-stage");
        let rollback_path = unique_sibling_path(&rollout_path, "workspace-move-rollback");
        fs::write(&staged_path, workspace_rollout("/new/project")).unwrap();
        fs::copy(&rollout_path, &rollback_path).unwrap();
        write_workspace_move_journal_fixture(&db_path, &rollout_path, &staged_path, &rollback_path);
        fs::remove_file(&rollout_path).unwrap();
        Connection::open(&db_path)
            .unwrap()
            .execute(
                "UPDATE threads SET cwd = '/new/project' WHERE id = 't1'",
                [],
            )
            .unwrap();

        recover_workspace_moves_for_db(&db_path).unwrap();

        assert_eq!(workspace_db_cwd(&db_path), "/new/project");
        assert_eq!(
            rollout_workspace_cwd(&rollout_path, "t1").unwrap(),
            Some("/new/project".to_string())
        );
        assert_workspace_move_artifacts_cleaned(temp.path());
    }

    #[test]
    fn workspace_move_recovery_rolls_back_missing_rollout_when_staging_is_missing() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("state.sqlite");
        let rollout_path = temp.path().join("rollout.jsonl");
        fs::write(&rollout_path, workspace_rollout("/old/project")).unwrap();
        create_workspace_thread_db(&db_path, &rollout_path);

        let staged_path = unique_sibling_path(&rollout_path, "workspace-move-stage");
        let rollback_path = unique_sibling_path(&rollout_path, "workspace-move-rollback");
        fs::copy(&rollout_path, &rollback_path).unwrap();
        write_workspace_move_journal_fixture(&db_path, &rollout_path, &staged_path, &rollback_path);
        fs::remove_file(&rollout_path).unwrap();
        Connection::open(&db_path)
            .unwrap()
            .execute(
                "UPDATE threads SET cwd = '/new/project' WHERE id = 't1'",
                [],
            )
            .unwrap();

        recover_workspace_moves_for_db(&db_path).unwrap();

        assert_eq!(workspace_db_cwd(&db_path), "/old/project");
        assert_eq!(
            rollout_workspace_cwd(&rollout_path, "t1").unwrap(),
            Some("/old/project".to_string())
        );
        assert_workspace_move_artifacts_cleaned(temp.path());
    }

    #[test]
    fn workspace_move_recovery_preserves_journal_when_rollback_is_missing() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("state.sqlite");
        let rollout_path = temp.path().join("rollout.jsonl");
        fs::write(&rollout_path, workspace_rollout("/old/project")).unwrap();
        create_workspace_thread_db(&db_path, &rollout_path);

        let staged_path = unique_sibling_path(&rollout_path, "workspace-move-stage");
        let rollback_path = unique_sibling_path(&rollout_path, "workspace-move-rollback");
        let journal_path = write_workspace_move_journal_fixture(
            &db_path,
            &rollout_path,
            &staged_path,
            &rollback_path,
        );
        fs::write(&rollout_path, workspace_rollout("/new/project")).unwrap();

        let error = recover_workspace_moves_for_db(&db_path).unwrap_err();

        assert!(format!("{error:#}").contains("回滚副本"));
        assert!(journal_path.is_file());
        assert_eq!(workspace_db_cwd(&db_path), "/old/project");
        assert_eq!(
            rollout_workspace_cwd(&rollout_path, "t1").unwrap(),
            Some("/new/project".to_string())
        );

        fs::write(&rollback_path, workspace_rollout("/old/project")).unwrap();
        recover_workspace_moves_for_db(&db_path).unwrap();
        assert_eq!(
            rollout_workspace_cwd(&rollout_path, "t1").unwrap(),
            Some("/old/project".to_string())
        );
        assert_workspace_move_artifacts_cleaned(temp.path());
    }

    #[test]
    fn workspace_move_recovery_rolls_back_db_when_staging_is_corrupt() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("state.sqlite");
        let rollout_path = temp.path().join("rollout.jsonl");
        fs::write(&rollout_path, workspace_rollout("/old/project")).unwrap();
        create_workspace_thread_db(&db_path, &rollout_path);

        let staged_path = unique_sibling_path(&rollout_path, "workspace-move-stage");
        let rollback_path = unique_sibling_path(&rollout_path, "workspace-move-rollback");
        fs::write(&staged_path, workspace_rollout("/other/project")).unwrap();
        fs::copy(&rollout_path, &rollback_path).unwrap();
        write_workspace_move_journal_fixture(&db_path, &rollout_path, &staged_path, &rollback_path);
        Connection::open(&db_path)
            .unwrap()
            .execute(
                "UPDATE threads SET cwd = '/new/project' WHERE id = 't1'",
                [],
            )
            .unwrap();

        recover_workspace_moves_for_db(&db_path).unwrap();

        assert_eq!(workspace_db_cwd(&db_path), "/old/project");
        assert_eq!(
            rollout_workspace_cwd(&rollout_path, "t1").unwrap(),
            Some("/old/project".to_string())
        );
        assert_workspace_move_artifacts_cleaned(temp.path());
    }

    #[test]
    fn workspace_move_recovery_repairs_rollout_when_db_was_already_at_target() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("state.sqlite");
        let rollout_path = temp.path().join("rollout.jsonl");
        fs::write(&rollout_path, workspace_rollout("/old/project")).unwrap();
        create_workspace_thread_db(&db_path, &rollout_path);
        Connection::open(&db_path)
            .unwrap()
            .execute(
                "UPDATE threads SET cwd = '/new/project' WHERE id = 't1'",
                [],
            )
            .unwrap();

        let staged_path = unique_sibling_path(&rollout_path, "workspace-move-stage");
        let rollback_path = unique_sibling_path(&rollout_path, "workspace-move-rollback");
        fs::write(&staged_path, workspace_rollout("/new/project")).unwrap();
        fs::copy(&rollout_path, &rollback_path).unwrap();
        write_workspace_move_journal_fixture_with_cwds(
            &db_path,
            &rollout_path,
            &staged_path,
            &rollback_path,
            "/new/project",
            "/new/project",
        );

        recover_workspace_moves_for_db(&db_path).unwrap();

        assert_eq!(workspace_db_cwd(&db_path), "/new/project");
        assert_eq!(
            rollout_workspace_cwd(&rollout_path, "t1").unwrap(),
            Some("/new/project".to_string())
        );
        assert_workspace_move_artifacts_cleaned(temp.path());
    }

    #[test]
    fn workspace_move_database_only_journal_forwards_db_after_crash() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("state.sqlite");
        let rollout_path = temp.path().join("rollout.jsonl");
        fs::write(&rollout_path, workspace_rollout("/new/project")).unwrap();
        create_workspace_thread_db(&db_path, &rollout_path);
        let staged_path = unique_sibling_path(&rollout_path, "workspace-move-stage");
        fs::write(&staged_path, workspace_rollout("/new/project")).unwrap();
        let journal_path = write_database_only_workspace_move_journal(
            &db_path,
            "t1",
            "/old/project",
            "/new/project",
            rollout_path.to_str().unwrap(),
            &staged_path,
        )
        .unwrap();

        recover_workspace_moves_for_db(&db_path).unwrap();

        assert_eq!(workspace_db_cwd(&db_path), "/new/project");
        assert!(!journal_path.exists());
        assert_eq!(
            rollout_workspace_cwd(&rollout_path, "t1").unwrap(),
            Some("/new/project".to_string())
        );
        recover_workspace_moves_for_db(&db_path).unwrap();
        assert_workspace_move_artifacts_cleaned(temp.path());
    }

    #[test]
    fn workspace_move_database_only_commit_busy_remains_recoverable() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("state.sqlite");
        let rollout_path = temp.path().join("rollout.jsonl");
        fs::write(&rollout_path, workspace_rollout("/new/project")).unwrap();
        create_workspace_thread_db(&db_path, &rollout_path);

        let reader = Connection::open(&db_path).unwrap();
        reader.execute_batch("BEGIN").unwrap();
        let _: String = reader
            .query_row("SELECT cwd FROM threads WHERE id = 't1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        let adapter =
            SQLiteStorageAdapter::new(&db_path, BackupStore::new(temp.path().join("backups")));

        let result = adapter.move_codex_thread_workspace_with_headroom(
            &SessionRef::new("local:t1", "Thread").unwrap(),
            "/new/project",
            |_path, _planned| Ok(()),
        );

        assert_eq!(result["status"], "failed");
        assert!(
            result["message"]
                .as_str()
                .unwrap()
                .contains("数据库提交失败")
        );
        assert_eq!(workspace_db_cwd(&db_path), "/old/project");
        assert_eq!(
            rollout_workspace_cwd(&rollout_path, "t1").unwrap(),
            Some("/new/project".to_string())
        );
        let journals = workspace_move_journal_paths(&db_path).unwrap();
        assert_eq!(journals.len(), 1);
        let (journal, journal_file) = read_workspace_move_journal(&journals[0]).unwrap();
        assert!(journal.database_only);
        assert!(!journal.staged_path.exists());
        drop(journal_file);

        reader.execute_batch("ROLLBACK").unwrap();
        recover_workspace_moves_for_db(&db_path).unwrap();

        assert_eq!(workspace_db_cwd(&db_path), "/new/project");
        assert_workspace_move_artifacts_cleaned(temp.path());
    }

    #[test]
    fn workspace_move_already_current_rollout_updates_database_and_cleans_journal() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("state.sqlite");
        let rollout_path = temp.path().join("rollout.jsonl");
        fs::write(&rollout_path, workspace_rollout("/new/project")).unwrap();
        create_workspace_thread_db(&db_path, &rollout_path);
        let adapter =
            SQLiteStorageAdapter::new(&db_path, BackupStore::new(temp.path().join("backups")));

        let result = adapter.move_codex_thread_workspace_with_headroom(
            &SessionRef::new("local:t1", "Thread").unwrap(),
            "/new/project",
            |_path, _planned| Ok(()),
        );

        assert_eq!(result["status"], "moved");
        assert_eq!(result["rollout_updated"], false);
        assert_eq!(workspace_db_cwd(&db_path), "/new/project");
        assert_eq!(
            rollout_workspace_cwd(&rollout_path, "t1").unwrap(),
            Some("/new/project".to_string())
        );
        assert_workspace_move_artifacts_cleaned(temp.path());
    }

    #[test]
    fn workspace_move_recovery_preserves_corrupt_journal_without_writes() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("state.sqlite");
        let rollout_path = temp.path().join("rollout.jsonl");
        let original = workspace_rollout("/old/project");
        fs::write(&rollout_path, &original).unwrap();
        create_workspace_thread_db(&db_path, &rollout_path);
        let journal_path = unique_workspace_move_journal_path(&db_path);
        fs::write(&journal_path, b"{not-json").unwrap();

        let error = recover_workspace_moves_for_db(&db_path).unwrap_err();

        assert!(format!("{error:#}").contains("JSON"));
        assert!(journal_path.is_file());
        assert_eq!(workspace_db_cwd(&db_path), "/old/project");
        assert_eq!(fs::read(&rollout_path).unwrap(), original);

        let adapter =
            SQLiteStorageAdapter::new(&db_path, BackupStore::new(temp.path().join("backups")));
        let blocked = adapter.move_codex_thread_workspace(
            &SessionRef::new("local:t1", "Thread").unwrap(),
            "/new/project",
        );
        assert_eq!(blocked["status"], "failed");
        assert!(blocked["message"].as_str().unwrap().contains("JSON 无效"));
        assert_eq!(workspace_db_cwd(&db_path), "/old/project");
        assert_eq!(fs::read(&rollout_path).unwrap(), original);
    }

    #[test]
    fn workspace_move_recovery_retries_after_cleanup_failure() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("state.sqlite");
        let rollout_path = temp.path().join("rollout.jsonl");
        fs::write(&rollout_path, workspace_rollout("/new/project")).unwrap();
        create_workspace_thread_db(&db_path, &rollout_path);
        Connection::open(&db_path)
            .unwrap()
            .execute(
                "UPDATE threads SET cwd = '/new/project' WHERE id = 't1'",
                [],
            )
            .unwrap();

        let staged_path = unique_sibling_path(&rollout_path, "workspace-move-stage");
        let rollback_path = unique_sibling_path(&rollout_path, "workspace-move-rollback");
        fs::create_dir(&staged_path).unwrap();
        fs::write(&rollback_path, workspace_rollout("/old/project")).unwrap();
        let journal_path = write_workspace_move_journal_fixture(
            &db_path,
            &rollout_path,
            &staged_path,
            &rollback_path,
        );

        let error = recover_workspace_moves_for_db(&db_path).unwrap_err();

        assert!(format!("{error:#}").contains("无法清理"));
        assert!(journal_path.is_file());
        assert!(staged_path.is_dir());
        assert!(rollback_path.is_file());
        assert_eq!(workspace_db_cwd(&db_path), "/new/project");

        fs::remove_dir(&staged_path).unwrap();
        recover_workspace_moves_for_db(&db_path).unwrap();
        assert_workspace_move_artifacts_cleaned(temp.path());
    }

    #[test]
    fn workspace_move_recovery_waits_for_journal_lock_then_completes() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("state.sqlite");
        let rollout_path = temp.path().join("rollout.jsonl");
        fs::write(&rollout_path, workspace_rollout("/old/project")).unwrap();
        create_workspace_thread_db(&db_path, &rollout_path);

        let staged_path = unique_sibling_path(&rollout_path, "workspace-move-stage");
        let rollback_path = unique_sibling_path(&rollout_path, "workspace-move-rollback");
        fs::write(&staged_path, workspace_rollout("/new/project")).unwrap();
        fs::copy(&rollout_path, &rollback_path).unwrap();
        let journal_path = write_workspace_move_journal_fixture(
            &db_path,
            &rollout_path,
            &staged_path,
            &rollback_path,
        );
        let journal_lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&journal_path)
            .unwrap();
        FileExt::lock_exclusive(&journal_lock).unwrap();

        let recovery_db_path = db_path.clone();
        let recovery =
            std::thread::spawn(move || recover_workspace_moves_for_db(&recovery_db_path));
        std::thread::sleep(Duration::from_millis(100));
        FileExt::unlock(&journal_lock).unwrap();
        drop(journal_lock);

        recovery.join().unwrap().unwrap();
        assert_eq!(workspace_db_cwd(&db_path), "/old/project");
        assert_workspace_move_artifacts_cleaned(temp.path());
    }

    #[cfg(windows)]
    #[test]
    fn workspace_move_recovery_retries_transient_journal_sharing_violation() {
        use std::os::windows::fs::OpenOptionsExt;

        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("state.sqlite");
        let rollout_path = temp.path().join("rollout.jsonl");
        fs::write(&rollout_path, workspace_rollout("/old/project")).unwrap();
        create_workspace_thread_db(&db_path, &rollout_path);

        let staged_path = unique_sibling_path(&rollout_path, "workspace-move-stage");
        let rollback_path = unique_sibling_path(&rollout_path, "workspace-move-rollback");
        fs::write(&staged_path, workspace_rollout("/new/project")).unwrap();
        fs::copy(&rollout_path, &rollback_path).unwrap();
        let journal_path = write_workspace_move_journal_fixture(
            &db_path,
            &rollout_path,
            &staged_path,
            &rollback_path,
        );
        let journal_handle = OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&journal_path)
            .unwrap();

        let recovery_db_path = db_path.clone();
        let recovery =
            std::thread::spawn(move || recover_workspace_moves_for_db(&recovery_db_path));
        std::thread::sleep(Duration::from_millis(100));
        drop(journal_handle);

        recovery.join().unwrap().unwrap();
        assert_eq!(workspace_db_cwd(&db_path), "/old/project");
        assert_workspace_move_artifacts_cleaned(temp.path());
    }

    #[test]
    fn workspace_move_recovery_preserves_state_while_rollout_is_locked() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("state.sqlite");
        let rollout_path = temp.path().join("rollout.jsonl");
        fs::write(&rollout_path, workspace_rollout("/old/project")).unwrap();
        create_workspace_thread_db(&db_path, &rollout_path);

        let staged_path = unique_sibling_path(&rollout_path, "workspace-move-stage");
        let rollback_path = unique_sibling_path(&rollout_path, "workspace-move-rollback");
        fs::write(&staged_path, workspace_rollout("/new/project")).unwrap();
        fs::copy(&rollout_path, &rollback_path).unwrap();
        let journal_path = write_workspace_move_journal_fixture(
            &db_path,
            &rollout_path,
            &staged_path,
            &rollback_path,
        );
        let rollout_lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&rollout_path)
            .unwrap();
        FileExt::lock_exclusive(&rollout_lock).unwrap();

        let error = recover_workspace_moves_for_db(&db_path).unwrap_err();

        assert!(format!("{error:#}").contains("正在被 Codex 或其他进程使用"));
        assert!(journal_path.is_file());
        assert_eq!(workspace_db_cwd(&db_path), "/old/project");

        FileExt::unlock(&rollout_lock).unwrap();
        drop(rollout_lock);
        assert_eq!(
            rollout_workspace_cwd(&rollout_path, "t1").unwrap(),
            Some("/old/project".to_string())
        );
        recover_workspace_moves_for_db(&db_path).unwrap();
        assert_workspace_move_artifacts_cleaned(temp.path());
    }

    #[cfg(windows)]
    #[test]
    fn workspace_move_supports_database_and_rollout_on_different_volumes() {
        fn volume_name(path: &Path) -> Option<String> {
            fs::canonicalize(path)
                .ok()?
                .components()
                .next()
                .map(|component| component.as_os_str().to_string_lossy().to_ascii_lowercase())
        }

        let db_temp = tempfile::tempdir().unwrap();
        let rollout_temp = tempfile::Builder::new()
            .prefix("mirrorx-workspace-move-cross-volume-")
            .tempdir_in(std::env::current_dir().unwrap())
            .unwrap();
        if volume_name(db_temp.path()) == volume_name(rollout_temp.path()) {
            return;
        }

        let db_path = db_temp.path().join("state.sqlite");
        let rollout_path = rollout_temp.path().join("rollout.jsonl");
        fs::write(&rollout_path, workspace_rollout("/old/project")).unwrap();
        create_workspace_thread_db(&db_path, &rollout_path);
        let adapter =
            SQLiteStorageAdapter::new(&db_path, BackupStore::new(db_temp.path().join("backups")));

        let result = adapter.move_codex_thread_workspace_with_headroom(
            &SessionRef::new("local:t1", "Thread").unwrap(),
            "/new/project",
            |_path, _planned| Ok(()),
        );

        assert_eq!(result["status"], "moved");
        assert_eq!(workspace_db_cwd(&db_path), "/new/project");
        assert_eq!(
            rollout_workspace_cwd(&rollout_path, "t1").unwrap(),
            Some("/new/project".to_string())
        );
        assert_workspace_move_artifacts_cleaned(db_temp.path());
        assert_workspace_move_artifacts_cleaned(rollout_temp.path());
    }

    #[test]
    fn workspace_move_recovery_returns_ok_when_scanned_journal_disappears() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("state.sqlite");
        let rollout_path = temp.path().join("rollout.jsonl");
        fs::write(&rollout_path, workspace_rollout("/old/project")).unwrap();
        create_workspace_thread_db(&db_path, &rollout_path);
        let missing_journal = unique_workspace_move_journal_path(&db_path);

        recover_workspace_move_journal(&db_path, &missing_journal).unwrap();

        assert_eq!(workspace_db_cwd(&db_path), "/old/project");
        assert_eq!(
            rollout_workspace_cwd(&rollout_path, "t1").unwrap(),
            Some("/old/project".to_string())
        );
    }
}
