use anyhow::Context;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const DEFAULT_PROVIDER: &str = "openai";
const SESSION_DIRS: [&str; 2] = ["sessions", "archived_sessions"];
const BACKUP_KEEP_COUNT: usize = 5;
const BACKUP_TOTAL_BUDGET_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const BACKUP_ESTIMATE_OVERHEAD_BYTES: u64 = 1024 * 1024;
const STALE_LOCK_MAX_AGE_SECS: u64 = 60 * 10;
const MAX_BUFFERED_JSONL_RECORD_BYTES: usize = 8 * 1024 * 1024;

fn default_codex_home_dir() -> PathBuf {
    codex_plus_core::codex_home::default_codex_home_dir()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderSyncStatus {
    Disabled,
    Skipped,
    Partial,
    Synced,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderSyncResult {
    pub status: ProviderSyncStatus,
    pub message: String,
    pub target_provider: String,
    pub backup_dir: Option<PathBuf>,
    pub changed_session_files: usize,
    pub skipped_locked_rollout_files: Vec<PathBuf>,
    pub sqlite_rows_updated: usize,
    pub sqlite_provider_rows_updated: usize,
    pub sqlite_user_event_rows_updated: usize,
    pub sqlite_cwd_rows_updated: usize,
    pub updated_workspace_roots: usize,
    pub encrypted_content_warning: Option<String>,
    #[serde(default)]
    pub repair_audit: ProviderSyncAudit,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSyncAudit {
    pub catalog_only_sessions: usize,
    pub catalog_only_with_current_rollout: usize,
    pub catalog_only_with_backup_database: usize,
    pub catalog_only_without_recovery_source: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderSyncTargetSource {
    Config,
    Rollout,
    Sqlite,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSyncTargetOption {
    pub id: String,
    pub sources: Vec<ProviderSyncTargetSource>,
    pub is_current_provider: bool,
    pub is_manual: bool,
    pub is_saved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSyncTargetList {
    pub current_provider: String,
    pub targets: Vec<ProviderSyncTargetOption>,
}

#[derive(Debug, Clone)]
struct SessionChange {
    path: PathBuf,
    original_session_meta_lines: Vec<String>,
    thread_id: Option<String>,
    cwd: Option<String>,
    has_user_event: bool,
    rewrite_needed: bool,
    original_mtime: Option<SystemTime>,
    original_len: Option<u64>,
}

#[derive(Debug, Default)]
struct RolloutRewrite {
    rewrite_needed: bool,
    thread_id: Option<String>,
    cwd: Option<String>,
    providers: Vec<String>,
    original_session_meta_lines: Vec<String>,
    session_meta_count: usize,
    marks_non_root_agent: bool,
    has_user_event: bool,
    has_encrypted_content: bool,
}

#[derive(Debug, Default)]
struct JsonlRecordSignals {
    session_meta: bool,
    user_event: bool,
    encrypted_content: bool,
}

enum BoundedJsonlRecord {
    Complete(String),
    Oversized(JsonlRecordSignals),
}

#[derive(Debug, Default)]
struct SessionChanges {
    changes: Vec<SessionChange>,
    skipped_locked_rollout_files: Vec<PathBuf>,
    encrypted_content_counts: HashMap<String, usize>,
    subagent_thread_ids: HashSet<String>,
}

#[derive(Debug, Default)]
struct ProviderSyncThreadKinds {
    subagent_thread_ids: HashSet<String>,
    explicit_user_thread_ids: HashSet<String>,
}

#[derive(Debug, Default)]
struct AppliedSessionChanges {
    changes: Vec<SessionChange>,
    skipped_locked_rollout_files: Vec<PathBuf>,
}

struct AtomicStage {
    path: PathBuf,
    file: Option<File>,
}

impl AtomicStage {
    fn create(parent: &Path, prefix: &str) -> std::io::Result<Self> {
        fs::create_dir_all(parent)?;
        for _ in 0..8 {
            let path = parent.join(format!(".{prefix}-{}.tmp", Uuid::new_v4()));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => {
                    return Ok(Self {
                        path,
                        file: Some(file),
                    });
                }
                Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(std::io::Error::new(
            ErrorKind::AlreadyExists,
            "failed to allocate a unique atomic staging file",
        ))
    }

    fn file_mut(&mut self) -> &mut File {
        self.file.as_mut().expect("atomic stage file is open")
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn commit(mut self, target: &Path) -> std::io::Result<()> {
        if let Some(file) = self.file.take() {
            file.sync_all()?;
            drop(file);
        }
        codex_plus_core::settings::replace_temp_path(&self.path, target)
    }
}

impl Drop for AtomicStage {
    fn drop(&mut self) {
        self.file.take();
        let _ = fs::remove_file(&self.path);
    }
}

#[derive(Debug, Default)]
struct SqliteUpdateCounts {
    provider_rows: usize,
    user_event_rows: usize,
    cwd_rows: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LockOwner {
    pid: u32,
}

impl SqliteUpdateCounts {
    fn total(&self) -> usize {
        self.provider_rows + self.user_event_rows + self.cwd_rows
    }

    fn add(&mut self, other: Self) {
        self.provider_rows += other.provider_rows;
        self.user_event_rows += other.user_event_rows;
        self.cwd_rows += other.cwd_rows;
    }
}

pub fn run_provider_sync(codex_home: Option<&Path>) -> ProviderSyncResult {
    run_provider_sync_with_target(codex_home, None)
}

pub fn run_provider_sync_with_target(
    codex_home: Option<&Path>,
    explicit_target_provider: Option<&str>,
) -> ProviderSyncResult {
    if let Err(error) = codex_plus_core::codex_sqlite::validate_codex_sqlite_home_environment() {
        return result(
            ProviderSyncStatus::Skipped,
            format!("Codex SQLite home is invalid: {error}"),
            DEFAULT_PROVIDER,
            None,
            0,
            0,
        );
    }
    let home = codex_home
        .map(Path::to_path_buf)
        .unwrap_or_else(default_codex_home_dir);
    if !home.exists() {
        return result(
            ProviderSyncStatus::Skipped,
            format!("Codex home not found: {}", home.to_string_lossy()),
            DEFAULT_PROVIDER,
            None,
            0,
            0,
        );
    }
    let target_provider =
        match resolve_target_provider(&home.join("config.toml"), explicit_target_provider) {
            Ok(provider) => provider,
            Err(message) => {
                return result(
                    ProviderSyncStatus::Skipped,
                    message,
                    DEFAULT_PROVIDER,
                    None,
                    0,
                    0,
                );
            }
        };
    let lock_dir = home.join("tmp/provider-sync.lock");
    if acquire_lock(&lock_dir).is_err() {
        return result(
            ProviderSyncStatus::Skipped,
            format!("Provider sync lock exists: {}", lock_dir.to_string_lossy()),
            &target_provider,
            None,
            0,
            0,
        );
    }
    let sync_result = (|| -> anyhow::Result<ProviderSyncResult> {
        let sqlite_paths = codex_plus_core::codex_sqlite::codex_session_db_paths_from_home(&home);
        let thread_kinds = sqlite_provider_sync_thread_kinds(&sqlite_paths)?;
        let repair_audit = match audit_provider_sync_state(&home, &sqlite_paths) {
            Ok(audit) => audit,
            Err(error) => {
                let _ = codex_plus_core::diagnostic_log::append_diagnostic_log(
                    "provider_sync.repair_audit_failed",
                    json!({
                        "error": error.to_string(),
                        "backup_root": home
                            .join("backups_state/provider-sync")
                            .to_string_lossy(),
                    }),
                );
                ProviderSyncAudit::default()
            }
        };
        let collected = collect_session_changes(
            &home,
            &target_provider,
            &thread_kinds.subagent_thread_ids,
            &thread_kinds.explicit_user_thread_ids,
        )?;
        let mut subagent_thread_ids = thread_kinds.subagent_thread_ids;
        subagent_thread_ids.extend(collected.subagent_thread_ids.iter().cloned());
        let encrypted_content_warning =
            build_encrypted_content_warning(&collected.encrypted_content_counts, &target_provider);
        let rewrite_changes = collected
            .changes
            .iter()
            .filter(|change| change.rewrite_needed)
            .cloned()
            .collect::<Vec<_>>();
        let thread_ids_with_user_events = collected
            .changes
            .iter()
            .filter(|change| change.has_user_event)
            .filter_map(|change| change.thread_id.clone())
            .collect::<HashSet<_>>();
        let projectless_thread_ids =
            load_projectless_thread_ids(&home.join(".codex-global-state.json"))?;
        let cwd_by_thread_id = collected
            .changes
            .iter()
            .filter_map(|change| Some((change.thread_id.clone()?, change.cwd.clone()?)))
            .filter(|(thread_id, _)| !projectless_thread_ids.contains(thread_id))
            .collect::<HashMap<_, _>>();
        let sqlite_update_count = count_sqlite_updates_for_paths(
            &sqlite_paths,
            &target_provider,
            &thread_ids_with_user_events,
            &cwd_by_thread_id,
            &subagent_thread_ids,
        )?;
        let global_state_update_count =
            count_global_state_updates(&home.join(".codex-global-state.json"))?;
        if rewrite_changes.is_empty() && sqlite_update_count == 0 && global_state_update_count == 0
        {
            let mut synced = result(
                ProviderSyncStatus::Synced,
                "Provider sync already up to date",
                &target_provider,
                None,
                0,
                0,
            );
            synced.skipped_locked_rollout_files = collected.skipped_locked_rollout_files;
            synced.encrypted_content_warning = encrypted_content_warning;
            if !synced.skipped_locked_rollout_files.is_empty() {
                synced.status = ProviderSyncStatus::Partial;
                synced.message = format!(
                    "Provider sync partial: {} rollout file(s) are locked",
                    synced.skipped_locked_rollout_files.len()
                );
            }
            synced.repair_audit = repair_audit;
            synced.message =
                provider_sync_message_with_audit(&synced.message, &synced.repair_audit);
            return Ok(synced);
        }
        let estimated_backup_bytes = estimate_backup_and_working_bytes(&home, &rewrite_changes);
        ensure_sqlite_working_headroom(&sqlite_paths)?;
        codex_plus_core::mirror_access::ensure_storage_headroom(
            &home,
            estimated_backup_bytes,
            codex_plus_core::mirror_access::MIN_SAFE_FREE_SPACE_BYTES,
        )?;
        let backup_dir = create_backup(&home, &target_provider, &rewrite_changes)?;
        let applied =
            apply_session_changes(&home, &backup_dir, &rewrite_changes, &target_provider)?;
        let apply_result = (|| -> anyhow::Result<(SqliteUpdateCounts, usize)> {
            let sqlite_updates = apply_sqlite_update_for_paths(
                &sqlite_paths,
                &target_provider,
                &thread_ids_with_user_events,
                &cwd_by_thread_id,
                &subagent_thread_ids,
            )?;
            let updated_workspace_roots =
                apply_global_state_update(&home.join(".codex-global-state.json"))?;
            prune_backups(&home)?;
            Ok((sqlite_updates, updated_workspace_roots))
        })();
        let (sqlite_updates, updated_workspace_roots) = match apply_result {
            Ok(counts) => counts,
            Err(err) => {
                return match restore_provider_sync_backup(
                    &home,
                    &backup_dir,
                    &applied.changes,
                ) {
                    Ok(()) => Err(err).context(
                        "Provider sync failed after writes began; all modified artifacts were restored",
                    ),
                    Err(rollback_error) => Err(err).context(format!(
                        "Provider sync failed and automatic rollback was incomplete: {rollback_error}; recovery backup retained at {}",
                        backup_dir.display()
                    )),
                };
            }
        };
        let mut synced = result(
            ProviderSyncStatus::Synced,
            "Provider sync complete",
            &target_provider,
            Some(backup_dir),
            applied.changes.len(),
            sqlite_updates.total(),
        );
        synced.skipped_locked_rollout_files = collected.skipped_locked_rollout_files;
        synced
            .skipped_locked_rollout_files
            .extend(applied.skipped_locked_rollout_files);
        synced.skipped_locked_rollout_files.sort();
        synced.skipped_locked_rollout_files.dedup();
        if !synced.skipped_locked_rollout_files.is_empty() {
            synced.status = ProviderSyncStatus::Partial;
            synced.message = format!(
                "Provider sync partial: {} rollout file(s) are locked",
                synced.skipped_locked_rollout_files.len()
            );
        }
        synced.sqlite_provider_rows_updated = sqlite_updates.provider_rows;
        synced.sqlite_user_event_rows_updated = sqlite_updates.user_event_rows;
        synced.sqlite_cwd_rows_updated = sqlite_updates.cwd_rows;
        synced.updated_workspace_roots = updated_workspace_roots;
        synced.encrypted_content_warning = encrypted_content_warning;
        synced.repair_audit = repair_audit;
        synced.message = provider_sync_message_with_audit(&synced.message, &synced.repair_audit);
        Ok(synced)
    })();
    let _ = release_lock(&lock_dir);
    sync_result.unwrap_or_else(|err| {
        result(
            ProviderSyncStatus::Skipped,
            format!("Provider sync skipped: {err}"),
            &target_provider,
            None,
            0,
            0,
        )
    })
}

fn result(
    status: ProviderSyncStatus,
    message: impl Into<String>,
    target_provider: &str,
    backup_dir: Option<PathBuf>,
    changed_session_files: usize,
    sqlite_rows_updated: usize,
) -> ProviderSyncResult {
    ProviderSyncResult {
        status,
        message: message.into(),
        target_provider: target_provider.to_string(),
        backup_dir,
        changed_session_files,
        skipped_locked_rollout_files: Vec::new(),
        sqlite_rows_updated,
        sqlite_provider_rows_updated: 0,
        sqlite_user_event_rows_updated: 0,
        sqlite_cwd_rows_updated: 0,
        updated_workspace_roots: 0,
        encrypted_content_warning: None,
        repair_audit: ProviderSyncAudit::default(),
    }
}

fn provider_sync_message_with_audit(message: &str, audit: &ProviderSyncAudit) -> String {
    if audit.catalog_only_sessions == 0 {
        return message.to_string();
    }
    format!(
        "{message}；审计发现 {} 条仅存在于本地会话目录的记录，其中 {} 条仍有当前 rollout、{} 条只能在历史数据库备份中找到，{} 条没有可用恢复来源；未自动重建缺失的 canonical 会话。",
        audit.catalog_only_sessions,
        audit.catalog_only_with_current_rollout,
        audit.catalog_only_with_backup_database,
        audit.catalog_only_without_recovery_source,
    )
}

fn audit_provider_sync_state(
    home: &Path,
    sqlite_paths: &[PathBuf],
) -> anyhow::Result<ProviderSyncAudit> {
    let mut canonical_thread_ids = HashSet::new();
    let mut catalog_thread_ids = HashSet::new();
    for path in sqlite_paths {
        canonical_thread_ids.extend(sqlite_table_ids(path, "threads", "id")?);
        catalog_thread_ids.extend(sqlite_user_thread_ids(path)?);
    }

    let catalog_only = catalog_thread_ids
        .difference(&canonical_thread_ids)
        .cloned()
        .collect::<HashSet<_>>();
    if catalog_only.is_empty() {
        return Ok(ProviderSyncAudit::default());
    }

    let current_rollout_ids = rollout_files(home)?
        .into_iter()
        .filter_map(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .and_then(rollout_thread_id_from_filename)
        })
        .collect::<HashSet<_>>();
    let backup_database_ids = backup_database_thread_ids(home)?;
    let with_current_rollout = catalog_only
        .iter()
        .filter(|thread_id| current_rollout_ids.contains(*thread_id))
        .count();
    let with_backup_database = catalog_only
        .iter()
        .filter(|thread_id| {
            !current_rollout_ids.contains(*thread_id) && backup_database_ids.contains(*thread_id)
        })
        .count();

    Ok(ProviderSyncAudit {
        catalog_only_sessions: catalog_only.len(),
        catalog_only_with_current_rollout: with_current_rollout,
        catalog_only_with_backup_database: with_backup_database,
        catalog_only_without_recovery_source: catalog_only
            .iter()
            .filter(|thread_id| {
                !current_rollout_ids.contains(*thread_id)
                    && !backup_database_ids.contains(*thread_id)
            })
            .count(),
    })
}

fn backup_database_thread_ids(home: &Path) -> anyhow::Result<HashSet<String>> {
    let root = home.join("backups_state/provider-sync");
    let mut ids = HashSet::new();
    if !root.exists() {
        return Ok(ids);
    }
    let mut files = Vec::new();
    collect_files_recursive(&root, &mut files)?;
    for path in files {
        if !matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("sqlite" | "db")
        ) {
            continue;
        }
        if let Ok(thread_ids) = sqlite_table_ids(&path, "threads", "id") {
            ids.extend(thread_ids);
        }
    }
    Ok(ids)
}

fn collect_files_recursive(root: &Path, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            collect_files_recursive(&path, files)?;
        } else if file_type.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

pub fn load_provider_sync_targets(codex_home: Option<&Path>) -> ProviderSyncTargetList {
    let home = codex_home
        .map(Path::to_path_buf)
        .unwrap_or_else(default_codex_home_dir);
    let current_provider = read_current_provider(&home.join("config.toml"));
    let mut sources: HashMap<String, HashSet<ProviderSyncTargetSource>> = HashMap::new();

    fn add_sources(
        sources: &mut HashMap<String, HashSet<ProviderSyncTargetSource>>,
        ids: impl IntoIterator<Item = String>,
        source: ProviderSyncTargetSource,
    ) {
        for id in ids {
            if !is_valid_provider_id_for_discovery(&id) {
                continue;
            }
            sources.entry(id).or_default().insert(source);
        }
    }

    add_sources(
        &mut sources,
        list_configured_provider_ids(&home.join("config.toml")),
        ProviderSyncTargetSource::Config,
    );
    add_sources(
        &mut sources,
        [current_provider.clone()],
        ProviderSyncTargetSource::Config,
    );
    if let Ok(ids) = rollout_provider_ids(&home) {
        add_sources(&mut sources, ids, ProviderSyncTargetSource::Rollout);
    }
    for db_path in codex_plus_core::codex_sqlite::codex_session_db_paths_from_home(&home) {
        if let Ok(ids) = sqlite_provider_ids(&db_path) {
            add_sources(&mut sources, ids, ProviderSyncTargetSource::Sqlite);
        }
    }

    let mut targets = sources
        .into_iter()
        .map(|(id, source_set)| {
            let mut source_list = source_set.into_iter().collect::<Vec<_>>();
            source_list.sort();
            ProviderSyncTargetOption {
                is_current_provider: id == current_provider,
                is_manual: source_list.contains(&ProviderSyncTargetSource::Manual),
                is_saved: false,
                id,
                sources: source_list,
            }
        })
        .collect::<Vec<_>>();
    targets.sort_by(|left, right| {
        right
            .is_current_provider
            .cmp(&left.is_current_provider)
            .then_with(|| left.id.cmp(&right.id))
    });

    ProviderSyncTargetList {
        current_provider,
        targets,
    }
}

fn read_current_provider(path: &Path) -> String {
    let Ok(text) = fs::read_to_string(path) else {
        return DEFAULT_PROVIDER.to_string();
    };
    let provider = root_toml_string_value(&text, "model_provider").unwrap_or_default();
    if provider.trim().is_empty() {
        DEFAULT_PROVIDER.to_string()
    } else {
        provider
    }
}

fn resolve_target_provider(
    config_path: &Path,
    explicit_target_provider: Option<&str>,
) -> Result<String, String> {
    if let Some(raw) = explicit_target_provider {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(read_current_provider(config_path));
        }
        if !is_valid_explicit_provider_id(trimmed) {
            return Err(format!("Invalid provider sync target: {trimmed:?}"));
        }
        return Ok(trimmed.to_string());
    }
    Ok(read_current_provider(config_path))
}

fn is_valid_explicit_provider_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
}

fn list_configured_provider_ids(path: &Path) -> Vec<String> {
    let mut ids = HashSet::new();
    ids.insert(DEFAULT_PROVIDER.to_string());
    let Ok(text) = fs::read_to_string(path) else {
        return sorted_provider_ids(ids);
    };
    for line in text.lines() {
        let stripped = line.trim();
        let Some(section) = stripped
            .strip_prefix("[model_providers.")
            .and_then(|rest| rest.strip_suffix(']'))
        else {
            continue;
        };
        let id = section.trim();
        if is_valid_provider_id_for_discovery(id) {
            ids.insert(id.to_string());
        }
    }
    sorted_provider_ids(ids)
}

fn sorted_provider_ids(ids: HashSet<String>) -> Vec<String> {
    let mut ids = ids
        .into_iter()
        .filter(|id| !id.trim().is_empty())
        .collect::<Vec<_>>();
    ids.sort();
    ids
}

fn is_valid_provider_id_for_discovery(value: &str) -> bool {
    !value.trim().is_empty() && !value.chars().any(char::is_control)
}

fn root_toml_string_value(text: &str, key: &str) -> Option<String> {
    for line in text.lines() {
        let stripped = line.trim();
        if stripped.starts_with('[') {
            break;
        }
        let Some(raw) = toml_key_raw_value(stripped, key) else {
            continue;
        };
        return toml_string_value(raw);
    }
    None
}

fn toml_key_raw_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(key)?.trim_start();
    rest.strip_prefix('=').map(str::trim_start)
}

fn toml_string_value(raw: &str) -> Option<String> {
    let quote = raw.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let mut value = String::new();
    let mut escaping = false;
    for ch in raw[quote.len_utf8()..].chars() {
        if quote == '"' && escaping {
            value.push(ch);
            escaping = false;
        } else if quote == '"' && ch == '\\' {
            escaping = true;
        } else if ch == quote {
            return Some(value);
        } else {
            value.push(ch);
        }
    }
    None
}

fn acquire_lock(path: &Path) -> std::io::Result<()> {
    acquire_lock_with(path, write_lock_owner)
}

fn acquire_lock_with(
    path: &Path,
    write_owner: impl Fn(&Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    fs::create_dir_all(path.parent().unwrap_or_else(|| Path::new(".")))?;
    let created = match fs::create_dir(path) {
        Ok(()) => true,
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            if reclaim_stale_lock(path)? {
                fs::create_dir(path)?;
                true
            } else {
                return Err(error);
            }
        }
        Err(error) => return Err(error),
    };
    debug_assert!(created);
    if let Err(error) = write_owner(path) {
        let _ = fs::remove_dir_all(path);
        return Err(error);
    }
    Ok(())
}

fn release_lock(path: &Path) -> std::io::Result<()> {
    if path.exists() {
        fs::remove_dir_all(path)?;
    }
    Ok(())
}

fn write_lock_owner(path: &Path) -> std::io::Result<()> {
    fs::write(
        path.join("owner.json"),
        json!({"pid": std::process::id(), "startedAt": now_secs()}).to_string(),
    )
}

fn reclaim_stale_lock(path: &Path) -> std::io::Result<bool> {
    match read_lock_owner(path)? {
        Some(owner) if !process_is_running(owner.pid) => {
            fs::remove_dir_all(path)?;
            Ok(true)
        }
        None if lock_is_stale_by_age(path) => {
            fs::remove_dir_all(path)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn read_lock_owner(path: &Path) -> std::io::Result<Option<LockOwner>> {
    let owner_path = path.join("owner.json");
    let text = match fs::read_to_string(owner_path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok(serde_json::from_str::<LockOwner>(&text).ok())
}

fn lock_is_stale_by_age(path: &Path) -> bool {
    fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age.as_secs() > STALE_LOCK_MAX_AGE_SECS)
}

#[cfg(windows)]
fn process_is_running(pid: u32) -> bool {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    if pid == 0 {
        return false;
    }
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .trim_start()
                .starts_with('"')
        })
        .unwrap_or(true)
}

#[cfg(not(windows))]
fn process_is_running(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|status| status.success())
        .unwrap_or(true)
}

fn collect_session_changes(
    home: &Path,
    target_provider: &str,
    excluded_thread_ids: &HashSet<String>,
    explicit_user_thread_ids: &HashSet<String>,
) -> anyhow::Result<SessionChanges> {
    let mut collected = SessionChanges::default();
    for path in rollout_files(home)? {
        let rewrite = match inspect_rollout_file(&path, target_provider) {
            Ok(rewrite) => rewrite,
            Err(error) if is_locked_io_error(&error) => {
                collected.skipped_locked_rollout_files.push(path);
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        if rewrite.session_meta_count == 0 {
            continue;
        }
        let is_explicit_user = rewrite
            .thread_id
            .as_ref()
            .is_some_and(|thread_id| explicit_user_thread_ids.contains(thread_id));
        if rewrite.marks_non_root_agent {
            if let Some(thread_id) = &rewrite.thread_id {
                collected.subagent_thread_ids.insert(thread_id.clone());
            }
            continue;
        }
        if !is_explicit_user
            && rewrite
                .thread_id
                .as_ref()
                .is_some_and(|thread_id| excluded_thread_ids.contains(thread_id))
        {
            continue;
        }
        if rewrite.has_encrypted_content {
            for provider in &rewrite.providers {
                *collected
                    .encrypted_content_counts
                    .entry(provider.clone())
                    .or_insert(0) += 1;
            }
        }
        let metadata = fs::metadata(&path)?;
        collected.changes.push(SessionChange {
            path,
            original_session_meta_lines: rewrite.original_session_meta_lines,
            thread_id: rewrite.thread_id,
            cwd: rewrite.cwd,
            has_user_event: rewrite.has_user_event,
            rewrite_needed: rewrite.rewrite_needed,
            original_mtime: metadata.modified().ok(),
            original_len: Some(metadata.len()),
        });
    }
    Ok(collected)
}

fn inspect_rollout_file(path: &Path, target_provider: &str) -> std::io::Result<RolloutRewrite> {
    let file = File::open(path)?;
    inspect_rollout(BufReader::new(file), target_provider)
}

fn inspect_rollout(
    mut reader: impl BufRead,
    target_provider: &str,
) -> std::io::Result<RolloutRewrite> {
    let mut rewrite = RolloutRewrite::default();
    let mut sink = std::io::sink();
    while let Some(record) = read_bounded_jsonl_record(&mut reader, &mut sink)? {
        match record {
            BoundedJsonlRecord::Complete(line) => {
                rewrite.has_user_event |=
                    line.contains("\"user_message\"") || line.contains("\"user_input\"");
                rewrite.has_encrypted_content |= line.contains("encrypted_content");
                let (record_text, _) = split_line_ending(&line);
                inspect_session_meta_line(record_text, target_provider, &mut rewrite);
            }
            BoundedJsonlRecord::Oversized(signals) => {
                rewrite.has_user_event |= signals.user_event;
                rewrite.has_encrypted_content |= signals.encrypted_content;
                if signals.session_meta {
                    return Err(oversized_session_meta_error());
                }
            }
        }
    }
    Ok(rewrite)
}

fn inspect_session_meta_line(line: &str, target_provider: &str, rewrite: &mut RolloutRewrite) {
    if line.trim().is_empty() || !line.contains("session_meta") {
        return;
    }
    let Ok(record) = serde_json::from_str::<Value>(line) else {
        return;
    };
    if record.get("type").and_then(Value::as_str) != Some("session_meta") {
        return;
    }
    let Some(payload) = record.get("payload").and_then(Value::as_object) else {
        return;
    };
    rewrite.session_meta_count += 1;
    rewrite.original_session_meta_lines.push(line.to_string());
    if rewrite.thread_id.is_none() {
        rewrite.thread_id = payload
            .get("id")
            .and_then(Value::as_str)
            .map(ToString::to_string);
    }
    if rewrite.cwd.is_none() {
        rewrite.cwd = payload
            .get("cwd")
            .and_then(Value::as_str)
            .and_then(to_desktop_workspace_path);
    }
    rewrite.marks_non_root_agent |= payload
        .get("source")
        .is_some_and(source_value_marks_non_root_agent);
    let provider = payload
        .get("model_provider")
        .and_then(Value::as_str)
        .unwrap_or("(missing)")
        .to_string();
    rewrite.providers.push(provider);
    rewrite.rewrite_needed |=
        payload.get("model_provider").and_then(Value::as_str) != Some(target_provider);
}

fn rollout_files(home: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for dirname in SESSION_DIRS {
        let root = home.join(dirname);
        if root.exists() {
            collect_rollout_files(&root, &mut files)?;
        }
    }
    files.sort();
    Ok(files)
}

fn rollout_provider_ids(home: &Path) -> anyhow::Result<Vec<String>> {
    let mut ids = HashSet::new();
    for path in rollout_files(home)? {
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if is_locked_io_error(&error) => continue,
            Err(error) => return Err(error.into()),
        };
        let mut reader = BufReader::new(file);
        let mut sink = std::io::sink();
        while let Some(record) = read_bounded_jsonl_record(&mut reader, &mut sink)? {
            match record {
                BoundedJsonlRecord::Complete(line) => {
                    let (line, _) = split_line_ending(&line);
                    let Ok(record) = serde_json::from_str::<Value>(line) else {
                        continue;
                    };
                    if record.get("type").and_then(Value::as_str) != Some("session_meta") {
                        continue;
                    }
                    let Some(provider) = record
                        .get("payload")
                        .and_then(Value::as_object)
                        .and_then(|payload| payload.get("model_provider"))
                        .and_then(Value::as_str)
                    else {
                        continue;
                    };
                    if is_valid_provider_id_for_discovery(provider) {
                        ids.insert(provider.to_string());
                    }
                }
                BoundedJsonlRecord::Oversized(signals) if signals.session_meta => {
                    return Err(oversized_session_meta_error().into());
                }
                BoundedJsonlRecord::Oversized(_) => {}
            }
        }
    }
    Ok(sorted_provider_ids(ids))
}

fn collect_rollout_files(root: &Path, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_rollout_files(&path, files)?;
        } else if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
        {
            files.push(path);
        }
    }
    Ok(())
}

fn rollout_thread_id_from_filename(name: &str) -> Option<String> {
    let stem = name.strip_prefix("rollout-")?.strip_suffix(".jsonl")?;
    if stem.len() < 36 {
        return None;
    }
    let candidate = &stem[stem.len() - 36..];
    let valid = candidate
        .chars()
        .enumerate()
        .all(|(index, ch)| match index {
            8 | 13 | 18 | 23 => ch == '-',
            _ => ch.is_ascii_hexdigit(),
        });
    valid.then(|| candidate.to_string())
}

fn split_line_ending(segment: &str) -> (&str, &str) {
    if let Some(line) = segment.strip_suffix("\r\n") {
        (line, "\r\n")
    } else if let Some(line) = segment.strip_suffix('\n') {
        (line, "\n")
    } else {
        (segment, "")
    }
}

fn read_bounded_jsonl_record(
    reader: &mut impl BufRead,
    oversized_sink: &mut impl Write,
) -> std::io::Result<Option<BoundedJsonlRecord>> {
    read_bounded_jsonl_record_with_limit(reader, oversized_sink, MAX_BUFFERED_JSONL_RECORD_BYTES)
}

fn read_bounded_jsonl_record_with_limit(
    reader: &mut impl BufRead,
    oversized_sink: &mut impl Write,
    max_buffered_bytes: usize,
) -> std::io::Result<Option<BoundedJsonlRecord>> {
    const NEEDLES: [&[u8]; 4] = [
        b"\"session_meta\"",
        b"\"user_message\"",
        b"\"user_input\"",
        b"encrypted_content",
    ];
    const SIGNAL_TAIL_BYTES: usize = 31;

    let mut buffered = Vec::with_capacity(8192);
    let mut oversized = false;
    let mut signals = JsonlRecordSignals::default();
    let mut signal_tail = Vec::with_capacity(SIGNAL_TAIL_BYTES);
    let mut saw_bytes = false;

    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            break;
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|index| index + 1)
            .unwrap_or(available.len());
        let chunk = &available[..take];
        let ends_record = chunk.last() == Some(&b'\n');
        saw_bytes = true;

        let mut searchable = Vec::with_capacity(signal_tail.len() + chunk.len());
        searchable.extend_from_slice(&signal_tail);
        searchable.extend_from_slice(chunk);
        signals.session_meta |= contains_bytes(&searchable, NEEDLES[0]);
        signals.user_event |=
            contains_bytes(&searchable, NEEDLES[1]) || contains_bytes(&searchable, NEEDLES[2]);
        signals.encrypted_content |= contains_bytes(&searchable, NEEDLES[3]);
        let tail_start = searchable.len().saturating_sub(SIGNAL_TAIL_BYTES);
        signal_tail.clear();
        signal_tail.extend_from_slice(&searchable[tail_start..]);

        if oversized {
            oversized_sink.write_all(chunk)?;
        } else if buffered.len().saturating_add(chunk.len()) <= max_buffered_bytes {
            buffered.extend_from_slice(chunk);
        } else {
            oversized = true;
            oversized_sink.write_all(&buffered)?;
            oversized_sink.write_all(chunk)?;
            buffered.clear();
        }
        reader.consume(take);
        if ends_record {
            break;
        }
    }

    if !saw_bytes {
        return Ok(None);
    }
    if oversized {
        return Ok(Some(BoundedJsonlRecord::Oversized(signals)));
    }
    let line = String::from_utf8(buffered).map_err(|error| {
        std::io::Error::new(
            ErrorKind::InvalidData,
            format!("rollout JSONL contains invalid UTF-8: {error}"),
        )
    })?;
    Ok(Some(BoundedJsonlRecord::Complete(line)))
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn oversized_session_meta_error() -> std::io::Error {
    std::io::Error::new(
        ErrorKind::InvalidData,
        format!(
            "rollout session_meta record exceeds the {} MiB safe rewrite limit; original file was not changed",
            MAX_BUFFERED_JSONL_RECORD_BYTES / (1024 * 1024)
        ),
    )
}

fn to_desktop_workspace_path(value: &str) -> Option<String> {
    let stripped = value.trim();
    if stripped.is_empty() {
        return None;
    }
    let lower = stripped.to_ascii_lowercase();
    if lower.starts_with(r"\\?\unc\") {
        return Some(format!(r"\\{}", stripped[8..].replace('/', r"\")));
    }
    if stripped.starts_with(r"\\?\") {
        return Some(stripped[4..].replace('\\', "/"));
    }
    Some(stripped.to_string())
}

fn is_locked_io_error(error: &std::io::Error) -> bool {
    matches!(error.kind(), std::io::ErrorKind::PermissionDenied)
        || matches!(error.raw_os_error(), Some(32 | 33))
}

fn is_locked_anyhow_error(error: &anyhow::Error) -> bool {
    error
        .chain()
        .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
        .any(is_locked_io_error)
}

fn build_encrypted_content_warning(
    encrypted_content_counts: &HashMap<String, usize>,
    target_provider: &str,
) -> Option<String> {
    let risky_providers = encrypted_content_counts
        .iter()
        .filter(|(provider, count)| provider.as_str() != target_provider && **count > 0)
        .map(|(provider, _)| provider.as_str())
        .collect::<Vec<_>>();
    if risky_providers.is_empty() {
        return None;
    }
    let total = encrypted_content_counts.values().sum::<usize>();
    Some(format!(
        "检测到 {total} 个会话文件包含来自 {} 的 encrypted_content。可见会话元数据已同步到 {target_provider}，但继续或压缩这些历史可能出现 invalid_encrypted_content；需要可靠续聊时请切回原供应商/账号或开启新会话。",
        risky_providers.join(", ")
    ))
}

fn create_backup(
    home: &Path,
    target_provider: &str,
    changes: &[SessionChange],
) -> anyhow::Result<PathBuf> {
    let backup_root = home.join("backups_state/provider-sync");
    let mut backup_dir = backup_root.join(timestamp_name());
    let mut suffix = 0;
    while backup_dir.exists() {
        suffix += 1;
        backup_dir = backup_root.join(format!("{}-{suffix}", timestamp_name()));
    }
    fs::create_dir_all(&backup_dir)?;
    let create_result = (|| -> anyhow::Result<()> {
        let mut state_files = Vec::new();
        for name in [
            "config.toml",
            ".codex-global-state.json",
            ".codex-global-state.json.bak",
        ] {
            let source = home.join(name);
            if source.exists() {
                fs::copy(&source, backup_dir.join(name))?;
                state_files.push(name.to_string());
            }
        }
        let db_dir = backup_dir.join("db");
        let mut db_files = Vec::new();
        for db_path in codex_plus_core::codex_sqlite::codex_session_db_paths_from_home(home) {
            for source in codex_plus_core::codex_sqlite::codex_sqlite_sidecar_paths(&db_path) {
                if !source.exists() {
                    continue;
                }
                let relative = codex_plus_core::codex_sqlite::relative_to_codex_home(home, &source);
                let target = db_dir.join(&relative);
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::copy(&source, &target)?;
                db_files.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
        let session_files_dir = backup_dir.join("session-files");
        for change in changes {
            let relative = change.path.strip_prefix(home).with_context(|| {
                format!(
                    "session path is outside Codex home: {}",
                    change.path.display()
                )
            })?;
            let target = session_files_dir.join(relative);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&change.path, target)?;
        }
        let manifest = changes
            .iter()
            .map(|change| {
                json!({
                    "path": change.path.to_string_lossy(),
                    "originalSessionMetaLines": change.original_session_meta_lines,
                })
            })
            .collect::<Vec<_>>();
        codex_plus_core::settings::atomic_write(
            &backup_dir.join("session-meta-backup.json"),
            serde_json::to_string_pretty(&manifest)?.as_bytes(),
        )?;
        codex_plus_core::settings::atomic_write(
            &backup_dir.join("metadata.json"),
            serde_json::to_string_pretty(&json!({
                "version": 2,
                "namespace": "provider-sync",
                "codexHome": home.to_string_lossy(),
                "targetProvider": target_provider,
                "createdAt": chrono::Utc::now().to_rfc3339(),
                "stateFiles": state_files,
                "dbFiles": db_files,
                "changedSessionFiles": changes.len(),
                "managedBy": "mirror+ provider sync"
            }))?
            .as_bytes(),
        )?;
        Ok(())
    })();
    if let Err(error) = create_result {
        let cleanup_error = fs::remove_dir_all(&backup_dir).err();
        return match cleanup_error {
            Some(cleanup_error) => Err(error).context(format!(
                "Provider sync backup failed, and its incomplete directory could not be removed: {cleanup_error}"
            )),
            None => Err(error).context("Provider sync backup failed; incomplete backup was removed"),
        };
    }
    Ok(backup_dir)
}

fn estimate_backup_and_working_bytes(home: &Path, changes: &[SessionChange]) -> u64 {
    let mut backup_bytes = 0u64;
    let mut sqlite_working_bytes = 0u64;
    let mut largest_atomic_rewrite = 0u64;

    for name in [
        "config.toml",
        ".codex-global-state.json",
        ".codex-global-state.json.bak",
    ] {
        let len = file_len(&home.join(name));
        backup_bytes = backup_bytes.saturating_add(len);
        largest_atomic_rewrite = largest_atomic_rewrite.max(len);
    }
    for db_path in codex_plus_core::codex_sqlite::codex_session_db_paths_from_home(home) {
        sqlite_working_bytes = sqlite_working_bytes.saturating_add(file_len(&db_path));
        for source in codex_plus_core::codex_sqlite::codex_sqlite_sidecar_paths(&db_path) {
            backup_bytes = backup_bytes.saturating_add(file_len(&source));
        }
    }
    for change in changes {
        let len = file_len(&change.path);
        backup_bytes = backup_bytes.saturating_add(len);
        largest_atomic_rewrite = largest_atomic_rewrite.max(len);
    }

    backup_bytes
        .saturating_add(sqlite_working_bytes.max(largest_atomic_rewrite))
        .saturating_add(BACKUP_ESTIMATE_OVERHEAD_BYTES)
}

fn ensure_sqlite_working_headroom(paths: &[PathBuf]) -> anyhow::Result<()> {
    for (directory, estimated_working_bytes) in sqlite_working_bytes_by_directory(paths) {
        codex_plus_core::mirror_access::ensure_storage_headroom(
            &directory,
            estimated_working_bytes,
            codex_plus_core::mirror_access::MIN_SAFE_FREE_SPACE_BYTES,
        )?;
    }
    Ok(())
}

fn sqlite_working_bytes_by_directory(paths: &[PathBuf]) -> HashMap<PathBuf, u64> {
    let mut estimates = HashMap::new();
    for path in paths.iter().filter(|path| path.is_file()) {
        let Some(directory) = path.parent() else {
            continue;
        };
        // A transaction can create a WAL or rollback journal approaching the
        // size of the main database. Existing sidecars also remain live until
        // SQLite checkpoints them, so account for both on the database volume.
        let database_bytes = file_len(path);
        let sidecar_bytes = codex_plus_core::codex_sqlite::codex_sqlite_sidecar_paths(path)
            .into_iter()
            .skip(1)
            .map(|sidecar| file_len(&sidecar))
            .sum::<u64>();
        let planned = database_bytes
            .saturating_add(sidecar_bytes)
            .saturating_add(BACKUP_ESTIMATE_OVERHEAD_BYTES);
        estimates
            .entry(directory.to_path_buf())
            .and_modify(|total: &mut u64| *total = total.saturating_add(planned))
            .or_insert(planned);
    }
    estimates
}

fn file_len(path: &Path) -> u64 {
    fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

fn apply_session_changes(
    home: &Path,
    backup_dir: &Path,
    changes: &[SessionChange],
    target_provider: &str,
) -> anyhow::Result<AppliedSessionChanges> {
    let mut applied = AppliedSessionChanges::default();
    for change in changes {
        if !change.rewrite_needed {
            continue;
        }
        match rewrite_rollout_file(&change.path, target_provider, change) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(error) if is_locked_anyhow_error(&error) => {
                applied
                    .skipped_locked_rollout_files
                    .push(change.path.clone());
                continue;
            }
            Err(error) => {
                return rollback_partial_session_changes(home, backup_dir, &applied.changes, error);
            }
        }
        restore_file_mtime(&change.path, change.original_mtime);
        applied.changes.push(change.clone());
    }
    Ok(applied)
}

fn rewrite_rollout_file(
    path: &Path,
    target_provider: &str,
    expected: &SessionChange,
) -> anyhow::Result<bool> {
    let metadata = verify_rollout_unchanged(path, expected)?;

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temp = AtomicStage::create(parent, "mirror-x-rollout")
        .with_context(|| format!("failed to create rollout temp file near {}", path.display()))?;
    let input = File::open(path)?;
    let mut reader = BufReader::new(input);
    let mut writer = BufWriter::new(temp.file_mut());
    let mut changed = false;
    while let Some(record) = read_bounded_jsonl_record(&mut reader, &mut writer)? {
        match record {
            BoundedJsonlRecord::Complete(line) => {
                let (raw_line, line_ending) = split_line_ending(&line);
                if let Some(next_line) = rewrite_session_meta_line(raw_line, target_provider)? {
                    writer.write_all(next_line.as_bytes())?;
                    writer.write_all(line_ending.as_bytes())?;
                    changed = true;
                } else {
                    writer.write_all(line.as_bytes())?;
                }
            }
            BoundedJsonlRecord::Oversized(signals) if signals.session_meta => {
                return Err(oversized_session_meta_error().into());
            }
            BoundedJsonlRecord::Oversized(_) => {}
        }
    }
    writer.flush()?;
    drop(writer);
    drop(reader);
    if !changed {
        return Ok(false);
    }
    // Large rollout files can take seconds to stream. Recheck immediately
    // before commit so messages appended during the rewrite are never lost.
    verify_rollout_unchanged(path, expected)?;
    let _ = fs::set_permissions(temp.path(), metadata.permissions());
    temp.commit(path)
        .with_context(|| format!("failed to replace rollout {}", path.display()))?;
    Ok(true)
}

fn verify_rollout_unchanged(path: &Path, expected: &SessionChange) -> anyhow::Result<fs::Metadata> {
    let metadata =
        fs::metadata(path).with_context(|| format!("failed to stat rollout {}", path.display()))?;
    if expected
        .original_len
        .is_some_and(|len| len != metadata.len())
        || expected
            .original_mtime
            .zip(metadata.modified().ok())
            .is_some_and(|(expected, actual)| expected != actual)
    {
        anyhow::bail!(
            "rollout changed while provider sync was preparing it: {}",
            path.display()
        );
    }
    Ok(metadata)
}

fn rewrite_session_meta_line(line: &str, target_provider: &str) -> anyhow::Result<Option<String>> {
    if line.trim().is_empty() || !line.contains("session_meta") {
        return Ok(None);
    }
    let Ok(mut record) = serde_json::from_str::<Value>(line) else {
        return Ok(None);
    };
    if record.get("type").and_then(Value::as_str) != Some("session_meta") {
        return Ok(None);
    }
    let Some(payload) = record.get_mut("payload").and_then(Value::as_object_mut) else {
        return Ok(None);
    };
    if payload.get("model_provider").and_then(Value::as_str) == Some(target_provider) {
        return Ok(None);
    }
    payload.insert("model_provider".to_string(), json!(target_provider));
    Ok(Some(serde_json::to_string(&record)?))
}

fn rollback_partial_session_changes(
    home: &Path,
    backup_dir: &Path,
    applied: &[SessionChange],
    error: anyhow::Error,
) -> anyhow::Result<AppliedSessionChanges> {
    match restore_session_changes(home, backup_dir, applied) {
        Ok(()) => Err(error).context("Session rewrite failed; earlier rollout changes were restored"),
        Err(rollback_error) => Err(error).context(format!(
            "Session rewrite failed and rollout rollback was incomplete: {rollback_error}; recovery backup retained at {}",
            backup_dir.display()
        )),
    }
}

fn restore_session_changes(
    home: &Path,
    backup_dir: &Path,
    changes: &[SessionChange],
) -> anyhow::Result<()> {
    for change in changes {
        let relative = change.path.strip_prefix(home).with_context(|| {
            format!(
                "session path is outside Codex home: {}",
                change.path.display()
            )
        })?;
        copy_file_atomic(
            &backup_dir.join("session-files").join(relative),
            &change.path,
        )?;
        restore_file_mtime(&change.path, change.original_mtime);
    }
    Ok(())
}

fn restore_provider_sync_backup(
    home: &Path,
    backup_dir: &Path,
    session_changes: &[SessionChange],
) -> anyhow::Result<()> {
    validate_provider_sync_backup_location(home, backup_dir)?;
    let metadata: Value = serde_json::from_slice(&fs::read(backup_dir.join("metadata.json"))?)
        .with_context(|| "Provider sync backup metadata is unreadable")?;
    if metadata.get("namespace").and_then(Value::as_str) != Some("provider-sync")
        || metadata.get("managedBy").and_then(Value::as_str) != Some("mirror+ provider sync")
        || metadata.get("codexHome").and_then(Value::as_str)
            != Some(home.to_string_lossy().as_ref())
    {
        anyhow::bail!("Provider sync backup metadata does not match this Codex home");
    }
    let state_files = metadata
        .get("stateFiles")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            anyhow::anyhow!("Provider sync backup does not record state file existence")
        })?
        .iter()
        .filter_map(Value::as_str)
        .collect::<HashSet<_>>();
    let db_files = metadata
        .get("dbFiles")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("Provider sync backup does not record database files"))?
        .iter()
        .filter_map(Value::as_str)
        .map(ToString::to_string)
        .collect::<HashSet<_>>();
    let mut errors = Vec::new();

    if let Err(error) = restore_session_changes(home, backup_dir, session_changes) {
        errors.push(format!("rollout: {error}"));
    }

    for name in [".codex-global-state.json", ".codex-global-state.json.bak"] {
        let target = home.join(name);
        let result: anyhow::Result<()> = if state_files.contains(name) {
            copy_file_atomic(&backup_dir.join(name), &target)
        } else {
            remove_optional_file(&target).map_err(Into::into)
        };
        if let Err(error) = result {
            errors.push(format!("{name}: {error}"));
        }
    }

    let mut current_db_artifacts = Vec::new();
    for db_path in codex_plus_core::codex_sqlite::codex_session_db_paths_from_home(home) {
        current_db_artifacts.extend(codex_plus_core::codex_sqlite::codex_sqlite_sidecar_paths(
            &db_path,
        ));
    }
    current_db_artifacts.sort();
    current_db_artifacts.dedup();

    // Remove sidecars that SQLite created during the failed operation before
    // restoring the main databases and any sidecars that existed beforehand.
    for target in &current_db_artifacts {
        let relative = codex_plus_core::codex_sqlite::relative_to_codex_home(home, target);
        let normalized = normalized_backup_relative_path(&relative)?;
        let is_sidecar = normalized.ends_with("-wal") || normalized.ends_with("-shm");
        if is_sidecar
            && !db_files.contains(&normalized)
            && let Err(error) = remove_optional_file(target)
        {
            errors.push(format!("{}: {error}", target.display()));
        }
    }
    let mut ordered_db_files = db_files.into_iter().collect::<Vec<_>>();
    ordered_db_files.sort_by_key(|path| path.ends_with("-wal") || path.ends_with("-shm"));
    for relative in ordered_db_files {
        let relative_path = validated_backup_relative_path(&relative)?;
        let source = backup_dir.join("db").join(&relative_path);
        let target = codex_plus_core::codex_sqlite::path_from_backup_relative(home, &relative_path);
        let result = copy_file_atomic(&source, &target);
        if let Err(error) = result {
            errors.push(format!("{}: {error}", target.display()));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(errors.join("; "))
    }
}

fn validate_provider_sync_backup_location(home: &Path, backup_dir: &Path) -> anyhow::Result<()> {
    let root = home.join("backups_state/provider-sync");
    if !backup_dir.starts_with(&root) || backup_dir == root {
        anyhow::bail!("Provider sync backup path is outside its managed backup root");
    }
    Ok(())
}

fn validated_backup_relative_path(value: &str) -> anyhow::Result<PathBuf> {
    let path = PathBuf::from(value);
    let normalized = normalized_backup_relative_path(&path)?;
    Ok(PathBuf::from(normalized))
}

fn normalized_backup_relative_path(path: &Path) -> anyhow::Result<String> {
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        anyhow::bail!("Unsafe path in Provider sync backup: {}", path.display());
    }
    Ok(path.to_string_lossy().replace('\\', "/"))
}

fn copy_file_atomic(source: &Path, target: &Path) -> anyhow::Result<()> {
    let metadata = fs::metadata(source)
        .with_context(|| format!("failed to stat backup source {}", source.display()))?;
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let mut temp = AtomicStage::create(parent, "mirror-x-restore")
        .with_context(|| format!("failed to create restore temp near {}", target.display()))?;
    {
        let mut reader = BufReader::new(File::open(source)?);
        let mut writer = BufWriter::new(temp.file_mut());
        std::io::copy(&mut reader, &mut writer)?;
        writer.flush()?;
    }
    let _ = fs::set_permissions(temp.path(), metadata.permissions());
    temp.commit(target)
        .with_context(|| format!("failed to restore {}", target.display()))?;
    Ok(())
}

fn remove_optional_file(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn restore_file_mtime(path: &Path, mtime: Option<SystemTime>) {
    let Some(mtime) = mtime else { return };
    let Ok(file) = fs::File::options().write(true).open(path) else {
        return;
    };
    let times = std::fs::FileTimes::new().set_modified(mtime);
    let _ = file.set_times(times);
}

fn table_columns(db: &Connection, table: &str) -> anyhow::Result<HashSet<String>> {
    let mut stmt = db.prepare(&format!(
        "PRAGMA table_info(\"{}\")",
        table.replace('"', "\"\"")
    ))?;
    Ok(stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<HashSet<_>>>()?)
}

fn sqlite_provider_sync_thread_kinds(paths: &[PathBuf]) -> anyhow::Result<ProviderSyncThreadKinds> {
    let mut kinds = ProviderSyncThreadKinds::default();
    for path in paths {
        if !path.exists() {
            continue;
        }
        let db = Connection::open(path)?;
        for (table, column) in [
            ("thread_spawn_edges", "child_thread_id"),
            ("agent_job_items", "assigned_thread_id"),
        ] {
            if !table_columns(&db, table)?.contains(column) {
                continue;
            }
            let sql =
                format!("SELECT DISTINCT {column} FROM {table} WHERE COALESCE({column}, '') <> ''");
            kinds.subagent_thread_ids.extend(
                db.prepare(&sql)?
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<HashSet<_>>>()?,
            );
        }

        for (table, id_column, source_column) in [
            ("threads", "id", "source"),
            ("local_thread_catalog", "thread_id", "source_kind"),
        ] {
            let columns = table_columns(&db, table)?;
            if !columns.contains(id_column) {
                continue;
            }
            let source_expr = if columns.contains(source_column) {
                format!("COALESCE({source_column}, '')")
            } else {
                "''".to_string()
            };
            let thread_source_expr = if columns.contains("thread_source") {
                "thread_source".to_string()
            } else {
                "NULL".to_string()
            };
            let sql = format!(
                "SELECT {id_column}, {source_expr}, {thread_source_expr} FROM {table} WHERE COALESCE({id_column}, '') <> ''"
            );
            let mut stmt = db.prepare(&sql)?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1).unwrap_or_default(),
                    row.get::<_, Option<String>>(2).unwrap_or(None),
                ))
            })?;
            for row in rows {
                let (thread_id, source, thread_source) = row?;
                if source_structured_marks_non_root_agent(&source)
                    || thread_source_marks_non_root(thread_source.as_deref())
                {
                    kinds.subagent_thread_ids.insert(thread_id);
                } else if thread_source_is_user(thread_source.as_deref()) {
                    kinds.explicit_user_thread_ids.insert(thread_id);
                } else if source_marks_non_root_agent(&source) {
                    kinds.subagent_thread_ids.insert(thread_id);
                }
            }
        }
    }
    kinds
        .subagent_thread_ids
        .retain(|thread_id| !kinds.explicit_user_thread_ids.contains(thread_id));
    Ok(kinds)
}

fn thread_source_is_user(thread_source: Option<&str>) -> bool {
    thread_source
        .map(str::trim)
        .is_some_and(|value| value.eq_ignore_ascii_case("user"))
}

fn thread_source_marks_non_root(thread_source: Option<&str>) -> bool {
    thread_source.map(str::trim).is_some_and(|value| {
        value.eq_ignore_ascii_case("subagent") || value.eq_ignore_ascii_case("memory_consolidation")
    })
}

fn source_marks_non_root_agent(source: &str) -> bool {
    let source = source.trim();
    if source_text_marks_non_root_agent(source) {
        return true;
    }
    source_structured_marks_non_root_agent(source)
}

fn source_structured_marks_non_root_agent(source: &str) -> bool {
    serde_json::from_str::<Value>(source.trim())
        .is_ok_and(|source| source_value_marks_non_root_agent(&source))
}

fn source_value_marks_non_root_agent(source: &Value) -> bool {
    match source {
        Value::Object(object) => {
            object.contains_key("sub_agent")
                || object.contains_key("subagent")
                || object.contains_key("internal")
        }
        Value::String(value) => source_text_marks_non_root_agent(value),
        _ => false,
    }
}

fn source_text_marks_non_root_agent(source: &str) -> bool {
    let source = source.trim().to_ascii_lowercase();
    source == "subagent"
        || source == "internal"
        || source.starts_with("subagent_")
        || source.starts_with("internal_")
}

fn sqlite_table_ids(path: &Path, table: &str, column: &str) -> anyhow::Result<HashSet<String>> {
    if !path.exists() {
        return Ok(HashSet::new());
    }
    let db = Connection::open(path)?;
    if !table_columns(&db, table)?.contains(column) {
        return Ok(HashSet::new());
    }
    let sql = format!("SELECT DISTINCT {column} FROM {table} WHERE COALESCE({column}, '') <> ''");
    Ok(db
        .prepare(&sql)?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<HashSet<_>>>()?)
}

fn sqlite_user_thread_ids(path: &Path) -> anyhow::Result<HashSet<String>> {
    if !path.exists() {
        return Ok(HashSet::new());
    }
    let db = Connection::open(path)?;
    let columns = table_columns(&db, "local_thread_catalog")?;
    if !columns.contains("thread_id") {
        return Ok(HashSet::new());
    }
    let source_kind = if columns.contains("source_kind") {
        "COALESCE(source_kind, '')"
    } else {
        "''"
    };
    let thread_source = if columns.contains("thread_source") {
        "thread_source"
    } else {
        "NULL"
    };
    let sql = format!(
        "SELECT thread_id, {source_kind}, {thread_source} FROM local_thread_catalog WHERE COALESCE(thread_id, '') <> ''"
    );
    let mut ids = HashSet::new();
    for row in db.prepare(&sql)?.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1).unwrap_or_default(),
            row.get::<_, Option<String>>(2).unwrap_or(None),
        ))
    })? {
        let (thread_id, source_kind, thread_source) = row?;
        let structured_non_root = source_structured_marks_non_root_agent(&source_kind)
            || thread_source_marks_non_root(thread_source.as_deref());
        if !structured_non_root
            && (thread_source_is_user(thread_source.as_deref())
                || !source_marks_non_root_agent(&source_kind))
        {
            ids.insert(thread_id);
        }
    }
    Ok(ids)
}

fn sqlite_provider_ids(path: &Path) -> anyhow::Result<Vec<String>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let db = Connection::open(path)?;
    let columns = table_columns(&db, "threads")?;
    if !columns.contains("model_provider") {
        return Ok(Vec::new());
    }
    let mut stmt = db.prepare(
        "SELECT DISTINCT COALESCE(model_provider, '') FROM threads WHERE COALESCE(model_provider, '') <> ''",
    )?;
    let mut ids = HashSet::new();
    for item in stmt.query_map([], |row| row.get::<_, String>(0))? {
        let id = item?;
        if is_valid_provider_id_for_discovery(&id) {
            ids.insert(id);
        }
    }
    Ok(sorted_provider_ids(ids))
}

fn count_sqlite_updates(
    path: &Path,
    target_provider: &str,
    user_event_thread_ids: &HashSet<String>,
    cwd_by_thread_id: &HashMap<String, String>,
    excluded_thread_ids: &HashSet<String>,
) -> anyhow::Result<usize> {
    if !path.exists() {
        return Ok(0);
    }
    let db = Connection::open(path)?;
    let columns = table_columns(&db, "threads")?;
    let catalog_columns = table_columns(&db, "local_thread_catalog")?;
    let mut total = 0;
    if columns.contains("id") && columns.contains("model_provider") {
        total +=
            provider_update_thread_ids(&db, "threads", "id", target_provider, excluded_thread_ids)?
                .len();
    }
    if catalog_columns.contains("thread_id") && catalog_columns.contains("model_provider") {
        total += provider_update_thread_ids(
            &db,
            "local_thread_catalog",
            "thread_id",
            target_provider,
            excluded_thread_ids,
        )?
        .len();
    }
    if columns.contains("has_user_event") {
        for thread_id in user_event_thread_ids {
            if excluded_thread_ids.contains(thread_id) {
                continue;
            }
            total += db.query_row(
                "SELECT COUNT(*) FROM threads WHERE id = ?1 AND COALESCE(has_user_event, 0) <> 1",
                [thread_id],
                |row| row.get::<_, i64>(0),
            )? as usize;
        }
    }
    if columns.contains("cwd") {
        for (thread_id, cwd) in cwd_by_thread_id {
            if excluded_thread_ids.contains(thread_id) {
                continue;
            }
            total += db.query_row(
                "SELECT COUNT(*) FROM threads WHERE id = ?1 AND COALESCE(cwd, '') <> ?2",
                (thread_id, cwd),
                |row| row.get::<_, i64>(0),
            )? as usize;
        }
    }
    Ok(total)
}

fn count_sqlite_updates_for_paths(
    paths: &[PathBuf],
    target_provider: &str,
    user_event_thread_ids: &HashSet<String>,
    cwd_by_thread_id: &HashMap<String, String>,
    excluded_thread_ids: &HashSet<String>,
) -> anyhow::Result<usize> {
    let mut total = 0;
    for path in paths {
        total += count_sqlite_updates(
            path,
            target_provider,
            user_event_thread_ids,
            cwd_by_thread_id,
            excluded_thread_ids,
        )?;
    }
    Ok(total)
}

fn apply_sqlite_update(
    path: &Path,
    target_provider: &str,
    user_event_thread_ids: &HashSet<String>,
    cwd_by_thread_id: &HashMap<String, String>,
    excluded_thread_ids: &HashSet<String>,
) -> anyhow::Result<SqliteUpdateCounts> {
    if !path.exists() {
        return Ok(SqliteUpdateCounts::default());
    }
    let mut db = Connection::open(path)?;
    let columns = table_columns(&db, "threads")?;
    let catalog_columns = table_columns(&db, "local_thread_catalog")?;
    if !columns.contains("model_provider") && !catalog_columns.contains("model_provider") {
        return Ok(SqliteUpdateCounts::default());
    }
    let tx = db.transaction()?;
    let mut counts = SqliteUpdateCounts::default();
    if columns.contains("id") && columns.contains("model_provider") {
        for thread_id in
            provider_update_thread_ids(&tx, "threads", "id", target_provider, excluded_thread_ids)?
        {
            counts.provider_rows += tx.execute(
                "UPDATE threads SET model_provider = ?1 WHERE id = ?2 AND COALESCE(model_provider, '') <> ?1",
                (target_provider, thread_id),
            )?;
        }
    }
    if catalog_columns.contains("thread_id") && catalog_columns.contains("model_provider") {
        for thread_id in provider_update_thread_ids(
            &tx,
            "local_thread_catalog",
            "thread_id",
            target_provider,
            excluded_thread_ids,
        )? {
            counts.provider_rows += tx.execute(
                "UPDATE local_thread_catalog SET model_provider = ?1 WHERE thread_id = ?2 AND COALESCE(model_provider, '') <> ?1",
                (target_provider, thread_id),
            )?;
        }
    }
    if columns.contains("has_user_event") {
        for thread_id in user_event_thread_ids {
            if excluded_thread_ids.contains(thread_id) {
                continue;
            }
            counts.user_event_rows += tx.execute(
                "UPDATE threads SET has_user_event = 1 WHERE id = ?1 AND COALESCE(has_user_event, 0) <> 1",
                [thread_id],
            )?;
        }
    }
    if columns.contains("cwd") {
        for (thread_id, cwd) in cwd_by_thread_id {
            if excluded_thread_ids.contains(thread_id) {
                continue;
            }
            counts.cwd_rows += tx.execute(
                "UPDATE threads SET cwd = ?1 WHERE id = ?2 AND COALESCE(cwd, '') <> ?1",
                (cwd, thread_id),
            )?;
        }
    }
    tx.commit()?;
    Ok(counts)
}

fn provider_update_thread_ids(
    db: &Connection,
    table: &str,
    id_column: &str,
    target_provider: &str,
    excluded_thread_ids: &HashSet<String>,
) -> anyhow::Result<Vec<String>> {
    let sql = format!(
        "SELECT DISTINCT {id_column} FROM {table} WHERE COALESCE(model_provider, '') <> ?1 AND COALESCE({id_column}, '') <> ''"
    );
    let mut stmt = db.prepare(&sql)?;
    let thread_ids = stmt
        .query_map([target_provider], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(thread_ids
        .into_iter()
        .filter(|thread_id| !excluded_thread_ids.contains(thread_id))
        .collect())
}

fn apply_sqlite_update_for_paths(
    paths: &[PathBuf],
    target_provider: &str,
    user_event_thread_ids: &HashSet<String>,
    cwd_by_thread_id: &HashMap<String, String>,
    excluded_thread_ids: &HashSet<String>,
) -> anyhow::Result<SqliteUpdateCounts> {
    let mut total = SqliteUpdateCounts::default();
    for path in paths {
        total.add(apply_sqlite_update(
            path,
            target_provider,
            user_event_thread_ids,
            cwd_by_thread_id,
            excluded_thread_ids,
        )?);
    }
    Ok(total)
}

fn load_global_state(path: &Path) -> anyhow::Result<Map<String, Value>> {
    if !path.exists() {
        return Ok(Map::new());
    }
    Ok(serde_json::from_str::<Value>(&fs::read_to_string(path)?)?
        .as_object()
        .cloned()
        .unwrap_or_default())
}

fn load_projectless_thread_ids(path: &Path) -> anyhow::Result<HashSet<String>> {
    let state = load_global_state(path)?;
    let mut ids = HashSet::new();
    if let Some(items) = state
        .get("projectless-thread-ids")
        .and_then(Value::as_array)
    {
        for item in items {
            if let Some(id) = item.as_str().filter(|id| !id.trim().is_empty()) {
                ids.insert(id.to_string());
            }
        }
    }
    Ok(ids)
}

fn normalized_global_state(state: &Map<String, Value>) -> Map<String, Value> {
    let mut next = Map::new();
    if let Some(value) = state.get("electron-saved-workspace-roots") {
        next.insert(
            "electron-saved-workspace-roots".to_string(),
            json!(dedupe_paths(path_array(value))),
        );
    }
    if let Some(value) = state.get("project-order") {
        next.insert(
            "project-order".to_string(),
            json!(dedupe_paths(path_array(value))),
        );
    }
    if let Some(value) = state.get("active-workspace-roots") {
        let normalized = dedupe_paths(path_array(value));
        let next_value = if value.is_array() {
            json!(normalized)
        } else if let Some(first) = normalized.first() {
            json!(first)
        } else {
            value.clone()
        };
        next.insert("active-workspace-roots".to_string(), next_value);
    }
    if let Some(value) = state
        .get("electron-workspace-root-labels")
        .and_then(Value::as_object)
    {
        let mut labels = Map::new();
        for (key, item) in value {
            labels.insert(
                to_desktop_workspace_path(key).unwrap_or_else(|| key.clone()),
                item.clone(),
            );
        }
        next.insert(
            "electron-workspace-root-labels".to_string(),
            Value::Object(labels),
        );
    }
    if let Some(open_targets) = state
        .get("open-in-target-preferences")
        .and_then(Value::as_object)
    {
        let mut next_open_targets = open_targets.clone();
        if let Some(per_path) =
            copy_resolved_object_keys(open_targets.get("perPath").and_then(Value::as_object))
        {
            next_open_targets.insert("perPath".to_string(), Value::Object(per_path));
        }
        next.insert(
            "open-in-target-preferences".to_string(),
            Value::Object(next_open_targets),
        );
    }
    next
}

fn copy_resolved_object_keys(value: Option<&Map<String, Value>>) -> Option<Map<String, Value>> {
    let value = value?;
    let mut next = Map::new();
    for (key, item) in value {
        next.insert(
            to_desktop_workspace_path(key).unwrap_or_else(|| key.clone()),
            item.clone(),
        );
    }
    Some(next)
}

fn count_global_state_updates(path: &Path) -> anyhow::Result<usize> {
    let state = load_global_state(path)?;
    let next = normalized_global_state(&state);
    Ok(next
        .iter()
        .filter(|(key, value)| state.get(*key) != Some(*value))
        .count())
}

fn apply_global_state_update(path: &Path) -> anyhow::Result<usize> {
    let mut state = load_global_state(path)?;
    let next = normalized_global_state(&state);
    let count = next
        .iter()
        .filter(|(key, value)| state.get(*key) != Some(*value))
        .count();
    if count > 0 {
        for (key, value) in next {
            state.insert(key, value);
        }
        let text = serde_json::to_string_pretty(&Value::Object(state))?;
        atomic_replace(path, text.as_bytes())?;
        if let Some(parent) = path.parent() {
            atomic_replace(
                &parent.join(".codex-global-state.json.bak"),
                text.as_bytes(),
            )?;
        }
    }
    Ok(count)
}

fn atomic_replace(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temp = AtomicStage::create(parent, "mirror-x-atomic")?;
    temp.file_mut().write_all(bytes)?;
    temp.commit(path)?;
    Ok(())
}

fn path_array(value: &Value) -> Vec<String> {
    if let Some(items) = value.as_array() {
        items
            .iter()
            .filter_map(Value::as_str)
            .filter(|item| !item.trim().is_empty())
            .map(ToString::to_string)
            .collect()
    } else if let Some(value) = value.as_str().filter(|item| !item.trim().is_empty()) {
        vec![value.to_string()]
    } else {
        Vec::new()
    }
}

fn dedupe_paths(paths: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for path in paths {
        let Some(desktop) = to_desktop_workspace_path(&path) else {
            continue;
        };
        let comparable = desktop
            .replace('/', r"\")
            .trim_end_matches('\\')
            .to_ascii_lowercase();
        if seen.insert(comparable) {
            result.push(desktop);
        }
    }
    result
}

fn prune_backups(home: &Path) -> anyhow::Result<()> {
    prune_backups_with_limits(home, BACKUP_KEEP_COUNT, BACKUP_TOTAL_BUDGET_BYTES)
}

fn prune_backups_with_limits(
    home: &Path,
    keep_count: usize,
    total_budget_bytes: u64,
) -> anyhow::Result<()> {
    let root = home.join("backups_state/provider-sync");
    if !root.exists() {
        return Ok(());
    }
    let mut managed = Vec::new();
    for entry in fs::read_dir(&root)? {
        let path = entry?.path();
        if !path.is_dir() {
            continue;
        }
        let Ok(text) = fs::read_to_string(path.join("metadata.json")) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if value.get("managedBy").and_then(Value::as_str) == Some("mirror+ provider sync") {
            managed.push(path);
        }
    }
    managed.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    let mut kept_count = 0usize;
    let mut kept_bytes = 0u64;
    for path in managed {
        let size = directory_size_bytes(&path).unwrap_or(total_budget_bytes);
        let keep_latest = kept_count == 0;
        let within_count = kept_count < keep_count.max(1);
        let within_budget = kept_bytes.saturating_add(size) <= total_budget_bytes;
        if keep_latest || (within_count && within_budget) {
            kept_count += 1;
            kept_bytes = kept_bytes.saturating_add(size);
        } else {
            let _ = fs::remove_dir_all(path);
        }
    }
    Ok(())
}

fn directory_size_bytes(root: &Path) -> std::io::Result<u64> {
    let mut total = 0u64;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            total = total.saturating_add(directory_size_bytes(&entry.path())?);
        } else {
            total = total.saturating_add(entry.metadata()?.len());
        }
    }
    Ok(total)
}

fn timestamp_name() -> String {
    chrono::Local::now().format("%Y%m%d%H%M%S").to_string()
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod backup_safety_tests {
    use super::*;

    fn test_change(path: PathBuf) -> SessionChange {
        SessionChange {
            path,
            original_session_meta_lines: Vec::new(),
            thread_id: None,
            cwd: None,
            has_user_event: false,
            rewrite_needed: true,
            original_mtime: None,
            original_len: None,
        }
    }

    #[test]
    fn bounded_jsonl_reader_streams_oversized_records_without_losing_bytes() {
        let input = b"prefix-\"user_message\"-encrypted_content-suffix\n";
        let mut reader = BufReader::with_capacity(5, std::io::Cursor::new(input));
        let mut streamed = Vec::new();

        let record = read_bounded_jsonl_record_with_limit(&mut reader, &mut streamed, 12)
            .unwrap()
            .unwrap();

        let BoundedJsonlRecord::Oversized(signals) = record else {
            panic!("record should exceed the test limit");
        };
        assert!(signals.user_event);
        assert!(signals.encrypted_content);
        assert!(!signals.session_meta);
        assert_eq!(streamed, input);
    }

    #[test]
    fn bounded_jsonl_reader_detects_session_meta_across_small_chunks() {
        let input = b"{\"type\":\"session_meta\",\"payload\":{}}\n";
        let mut reader = BufReader::with_capacity(3, std::io::Cursor::new(input));
        let mut streamed = Vec::new();

        let record = read_bounded_jsonl_record_with_limit(&mut reader, &mut streamed, 8)
            .unwrap()
            .unwrap();

        let BoundedJsonlRecord::Oversized(signals) = record else {
            panic!("record should exceed the test limit");
        };
        assert!(signals.session_meta);
        assert_eq!(streamed, input);
    }

    #[test]
    fn backup_pruning_respects_byte_budget_and_always_keeps_latest_restore_point() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("backups_state/provider-sync");
        for (name, bytes) in [("20260101000001", 9usize), ("20260101000000", 4usize)] {
            let backup = root.join(name);
            fs::create_dir_all(&backup).unwrap();
            fs::write(
                backup.join("metadata.json"),
                json!({"managedBy": "mirror+ provider sync"}).to_string(),
            )
            .unwrap();
            fs::write(backup.join("payload.bin"), vec![0u8; bytes]).unwrap();
        }

        prune_backups_with_limits(temp.path(), 5, 8).unwrap();

        assert!(root.join("20260101000001").is_dir());
        assert!(!root.join("20260101000000").exists());
    }

    #[test]
    fn backup_estimate_includes_copy_and_atomic_rewrite_headroom() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let rollout = home.join("sessions/rollout.jsonl");
        fs::create_dir_all(rollout.parent().unwrap()).unwrap();
        fs::write(&rollout, vec![b'x'; 4096]).unwrap();

        let estimated = estimate_backup_and_working_bytes(home, &[test_change(rollout)]);

        assert!(estimated >= BACKUP_ESTIMATE_OVERHEAD_BYTES + 8192);
    }

    #[test]
    fn failed_lock_owner_write_removes_the_new_lock_directory() {
        let temp = tempfile::tempdir().unwrap();
        let lock = temp.path().join("tmp/provider-sync.lock");

        let error = acquire_lock_with(&lock, |_| {
            Err(std::io::Error::new(
                ErrorKind::PermissionDenied,
                "injected owner write failure",
            ))
        })
        .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert!(!lock.exists());
    }

    #[test]
    fn sqlite_headroom_estimate_is_charged_to_each_database_directory() {
        let temp = tempfile::tempdir().unwrap();
        let first_dir = temp.path().join("first");
        let second_dir = temp.path().join("second");
        fs::create_dir_all(&first_dir).unwrap();
        fs::create_dir_all(&second_dir).unwrap();
        let first = first_dir.join("codex.db");
        let second = second_dir.join("state.sqlite");
        fs::write(&first, vec![0_u8; 4096]).unwrap();
        fs::write(format!("{}-wal", first.to_string_lossy()), vec![0_u8; 2048]).unwrap();
        fs::write(&second, vec![0_u8; 8192]).unwrap();

        let estimates = sqlite_working_bytes_by_directory(&[first, second]);

        assert_eq!(
            estimates[&first_dir],
            BACKUP_ESTIMATE_OVERHEAD_BYTES + 4096 + 2048
        );
        assert_eq!(
            estimates[&second_dir],
            BACKUP_ESTIMATE_OVERHEAD_BYTES + 8192
        );
    }

    #[test]
    fn rollout_change_guard_detects_content_appended_after_scan() {
        let temp = tempfile::tempdir().unwrap();
        let rollout = temp.path().join("rollout.jsonl");
        fs::write(&rollout, "original\n").unwrap();
        let metadata = fs::metadata(&rollout).unwrap();
        let mut change = test_change(rollout.clone());
        change.original_len = Some(metadata.len());
        change.original_mtime = metadata.modified().ok();

        fs::OpenOptions::new()
            .append(true)
            .open(&rollout)
            .unwrap()
            .write_all(b"appended\n")
            .unwrap();

        let error = verify_rollout_unchanged(&rollout, &change).unwrap_err();
        assert!(error.to_string().contains("rollout changed"));
    }

    #[test]
    fn failed_backup_removes_only_its_incomplete_directory() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex");
        let outside = temp.path().join("outside-rollout.jsonl");
        fs::create_dir_all(&home).unwrap();
        fs::write(&outside, "{}\n").unwrap();

        let error = create_backup(&home, "mirrorplus", &[test_change(outside)]).unwrap_err();
        let backup_root = home.join("backups_state/provider-sync");

        assert!(error.to_string().contains("incomplete backup was removed"));
        assert_eq!(fs::read_dir(backup_root).unwrap().count(), 0);
    }

    #[test]
    fn rollback_restores_rollout_database_and_global_state_as_one_operation() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex");
        let rollout = home.join("sessions/rollout.jsonl");
        let global_state = home.join(".codex-global-state.json");
        let global_backup = home.join(".codex-global-state.json.bak");
        let db_path = home.join("state_5.sqlite");
        fs::create_dir_all(rollout.parent().unwrap()).unwrap();
        fs::write(&rollout, "original rollout\n").unwrap();
        fs::write(&global_state, r#"{"project-order":["D:/original"]}"#).unwrap();
        let db = Connection::open(&db_path).unwrap();
        db.execute_batch(
            "CREATE TABLE threads (id TEXT PRIMARY KEY, model_provider TEXT);\
             INSERT INTO threads (id, model_provider) VALUES ('thread-1', 'openai');",
        )
        .unwrap();
        drop(db);
        let change = test_change(rollout.clone());
        let backup_dir = create_backup(&home, "mirrorplus", std::slice::from_ref(&change))
            .expect("create complete backup");

        fs::write(&rollout, "changed rollout\n").unwrap();
        let db = Connection::open(&db_path).unwrap();
        db.execute(
            "UPDATE threads SET model_provider = 'mirrorplus' WHERE id = 'thread-1'",
            [],
        )
        .unwrap();
        drop(db);
        fs::write(&global_state, r#"{"project-order":["D:/changed"]}"#).unwrap();
        fs::write(&global_backup, r#"{"project-order":["D:/changed"]}"#).unwrap();
        let new_wal = PathBuf::from(format!("{}-wal", db_path.to_string_lossy()));
        fs::write(&new_wal, b"created during failed sync").unwrap();

        restore_provider_sync_backup(&home, &backup_dir, &[change]).expect("rollback all files");

        assert_eq!(fs::read_to_string(&rollout).unwrap(), "original rollout\n");
        assert_eq!(
            fs::read_to_string(&global_state).unwrap(),
            r#"{"project-order":["D:/original"]}"#
        );
        assert!(!global_backup.exists());
        assert!(!new_wal.exists());
        let db = Connection::open(&db_path).unwrap();
        let provider: String = db
            .query_row(
                "SELECT model_provider FROM threads WHERE id = 'thread-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(provider, "openai");
    }

    #[test]
    fn sqlite_update_count_propagates_invalid_thread_ids() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("state.sqlite");
        let db = Connection::open(&db_path).unwrap();
        db.execute_batch(
            "CREATE TABLE threads (id BLOB, model_provider TEXT);\
             INSERT INTO threads (id, model_provider) VALUES (x'FF', 'openai');",
        )
        .unwrap();
        drop(db);

        let error = count_sqlite_updates(
            &db_path,
            "mirrorplus",
            &HashSet::new(),
            &HashMap::new(),
            &HashSet::new(),
        )
        .unwrap_err();

        assert!(error.to_string().contains("Invalid column type"));
    }
}
