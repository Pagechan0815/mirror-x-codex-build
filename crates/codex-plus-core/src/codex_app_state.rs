use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use serde_json::{Map, Value, json};

const GLOBAL_STATE_FILE: &str = ".codex-global-state.json";
const GLOBAL_STATE_BACKUP_FILE: &str = ".codex-global-state.json.bak";
const BACKUP_ROOT: &str = "backups_state/app-state-sync";
const SNAPSHOT_FILE: &str = "latest-safe-state.json";
const SNAPSHOT_VERSION: u64 = 1;
const BACKUP_KEEP_COUNT: usize = 12;
const BACKUP_TOTAL_BUDGET_BYTES: u64 = 64 * 1024 * 1024;
const BACKUP_MANAGED_BY: &str = "Mirror X Codex app state sync";

const WORKSPACE_PATH_ARRAY_KEYS: &[&str] = &["electron-saved-workspace-roots", "project-order"];

const ACTIVE_WORKSPACE_ROOTS_KEY: &str = "active-workspace-roots";

const WORKSPACE_PATH_MAP_KEYS: &[&str] = &["electron-workspace-root-labels"];

const THREAD_STATE_MAP_KEYS: &[&str] = &[
    "thread-workspace-root-hints",
    "thread-projectless-output-directories",
    "thread-writable-roots",
];

const THREAD_ID_ARRAY_KEYS: &[&str] = &["projectless-thread-ids"];

const SAFE_TOP_LEVEL_KEYS: &[&str] = &[
    "electron-avatar-overlay-bounds",
    "electron-avatar-overlay-open",
    "electron-main-window-bounds",
];

const SAFE_ATOM_KEYS: &[&str] = &[
    "default-service-tier",
    "avatar-overlay-mascot-width-px",
    "composer-auto-context-enabled",
    "diff-filter",
    "enter-behavior",
    "first-awake-pet-notification-avatar-ids",
    "has-seen-codex-mobile-announcement",
    "has-seen-multi-agent-composer-banner",
    "has-user-changed-service-tier",
    "last_completed_onboarding",
    "preferred-non-full-access-agent-mode-by-host-id",
    "seen-model-upgrade-list",
    "sidebar-collapsed-groups",
    "sidebar-collapsed-sections-v1",
    "sidebar-width",
    "thread-summary-panel-section-expanded-progress",
    "unread-thread-ids-by-host-v1",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppStateSyncResult {
    pub changed: bool,
    pub changed_keys: Vec<String>,
    pub backup_path: Option<PathBuf>,
    pub snapshot_path: Option<PathBuf>,
}

pub fn capture_app_state_snapshot(home: &Path) -> anyhow::Result<Option<PathBuf>> {
    let Some(state) = load_global_state(home)? else {
        return Ok(None);
    };
    let snapshot = safe_snapshot_from_state(&state);
    let snapshot_state = snapshot
        .get("state")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if snapshot_state.is_empty() {
        return Ok(None);
    }
    let path = snapshot_path(home);
    let text = serde_json::to_string_pretty(&snapshot)?;
    prune_backups(home)?;
    ensure_write_headroom(home, text.len() as u64)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    crate::settings::atomic_write(&path, text.as_bytes())?;
    Ok(Some(path))
}

pub fn capture_app_state_snapshot_nonfatal(home: &Path, source: &str) {
    if let Err(error) = capture_app_state_snapshot(home) {
        let _ = crate::diagnostic_log::append_diagnostic_log(
            "codex_app_state.snapshot_failed",
            json!({
                "source": source,
                "error": error.to_string(),
            }),
        );
    }
}

pub fn sync_app_state_after_provider_switch(home: &Path) -> anyhow::Result<AppStateSyncResult> {
    let Some(mut state) = load_global_state(home)? else {
        return Ok(AppStateSyncResult {
            changed: false,
            changed_keys: Vec::new(),
            backup_path: None,
            snapshot_path: None,
        });
    };
    let original = Value::Object(state.clone());
    let mut changed_keys = BTreeSet::new();

    normalize_current_state(&mut state, &mut changed_keys);
    if let Some(snapshot) = load_snapshot(home)? {
        merge_safe_snapshot(&mut state, &snapshot, &mut changed_keys);
    }

    let next = Value::Object(state);
    if next == original {
        let snapshot_path = capture_app_state_snapshot(home)?;
        return Ok(AppStateSyncResult {
            changed: false,
            changed_keys: Vec::new(),
            backup_path: None,
            snapshot_path,
        });
    }

    let text = serde_json::to_string_pretty(&next)?;
    let original_text = serde_json::to_string_pretty(&original)?;
    prune_backups(home)?;
    ensure_write_headroom(
        home,
        (original_text.len() as u64).saturating_add((text.len() as u64).saturating_mul(2)),
    )?;
    let backup_path = create_backup(home, &original_text)?;
    write_state_pair_with_rollback(home, text.as_bytes())?;
    let snapshot_path = capture_app_state_snapshot(home)?;
    prune_backups(home)?;

    Ok(AppStateSyncResult {
        changed: true,
        changed_keys: changed_keys.into_iter().collect(),
        backup_path: Some(backup_path),
        snapshot_path,
    })
}

pub fn sync_app_state_after_provider_switch_nonfatal(home: &Path, source: &str) {
    match sync_app_state_after_provider_switch(home) {
        Ok(result) => {
            if result.changed {
                let _ = crate::diagnostic_log::append_diagnostic_log(
                    "codex_app_state.synced",
                    json!({
                        "source": source,
                        "changedKeys": result.changed_keys,
                        "backupPath": result.backup_path.map(|path| path.to_string_lossy().to_string()),
                        "snapshotPath": result.snapshot_path.map(|path| path.to_string_lossy().to_string()),
                    }),
                );
            }
        }
        Err(error) => {
            let _ = crate::diagnostic_log::append_diagnostic_log(
                "codex_app_state.sync_failed",
                json!({
                    "source": source,
                    "error": error.to_string(),
                }),
            );
        }
    }
}

pub fn set_local_full_access_mode(home: &Path) -> anyhow::Result<AppStateSyncResult> {
    let mut state = load_global_state(home)?.unwrap_or_default();
    let original = Value::Object(state.clone());
    let atom = state
        .entry("electron-persisted-atom-state".to_string())
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .context("electron-persisted-atom-state must be a JSON object")?;

    let agent_modes = atom
        .entry("agent-mode-by-host-id".to_string())
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .context("agent mode state must be a JSON object")?;
    agent_modes.insert("local".to_string(), json!("full-access"));

    atom.insert(
        "permission-selection-by-host-id:local".to_string(),
        json!({
            "kind": "agent-mode",
            "agentMode": "full-access",
        }),
    );
    atom.insert("skip-full-access-confirm".to_string(), json!(true));

    if let Some(thread_permissions) = atom
        .get_mut("heartbeat-thread-permissions-by-id")
        .and_then(Value::as_object_mut)
    {
        for permission in thread_permissions.values_mut() {
            let Some(permission) = permission.as_object_mut() else {
                continue;
            };
            permission.insert(
                "activePermissionProfile".to_string(),
                json!({ "id": ":danger-full-access", "extends": null }),
            );
            permission.insert("approvalPolicy".to_string(), json!("never"));
            permission.insert(
                "sandboxPolicy".to_string(),
                json!({ "type": "dangerFullAccess" }),
            );
        }
    }

    // Older Codex builds still consult this fallback when leaving full access.
    // Keep it local so a desktop repair never changes remote-host preferences.
    let preferences = atom
        .entry("preferred-non-full-access-agent-mode-by-host-id".to_string())
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .context("preferred agent mode state must be a JSON object")?;
    preferences.insert("local".to_string(), json!("auto"));

    let next = Value::Object(state);
    if next == original {
        return Ok(AppStateSyncResult {
            changed: false,
            changed_keys: Vec::new(),
            backup_path: None,
            snapshot_path: capture_app_state_snapshot(home)?,
        });
    }
    let original_text = serde_json::to_string_pretty(&original)?;
    let next_text = serde_json::to_string_pretty(&next)?;
    prune_backups(home)?;
    ensure_write_headroom(
        home,
        (original_text.len() as u64).saturating_add((next_text.len() as u64).saturating_mul(2)),
    )?;
    let backup_path = create_backup(home, &original_text)?;
    write_state_pair_with_rollback(home, next_text.as_bytes())?;
    let snapshot_path = match validate_local_full_access_state(home)
        .and_then(|_| capture_app_state_snapshot(home))
    {
        Ok(snapshot_path) => snapshot_path,
        Err(error) => {
            if let Err(restore_error) = restore_state_pair_from_backup(home, &backup_path) {
                return Err(error).context(format!(
                    "Codex full-access state verification failed and rollback was incomplete: {restore_error}"
                ));
            }
            return Err(error)
                .context("Codex full-access state verification failed; original files restored");
        }
    };
    Ok(AppStateSyncResult {
        changed: true,
        changed_keys: vec![
            "electron-persisted-atom-state.agent-mode-by-host-id.local".to_string(),
            "electron-persisted-atom-state.permission-selection-by-host-id:local".to_string(),
            "electron-persisted-atom-state.skip-full-access-confirm".to_string(),
            "electron-persisted-atom-state.heartbeat-thread-permissions-by-id".to_string(),
            "electron-persisted-atom-state.preferred-non-full-access-agent-mode-by-host-id"
                .to_string(),
        ],
        backup_path: Some(backup_path),
        snapshot_path,
    })
}

pub fn set_preferred_agent_mode_auto(home: &Path) -> anyhow::Result<AppStateSyncResult> {
    set_local_full_access_mode(home)
}

fn load_global_state(home: &Path) -> anyhow::Result<Option<Map<String, Value>>> {
    let path = state_path(home);
    if !path.exists() {
        return Ok(None);
    }
    let text =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let value: Value = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    value
        .as_object()
        .cloned()
        .map(Some)
        .ok_or_else(|| anyhow::anyhow!("{} must be a JSON object", path.display()))
}

fn load_snapshot(home: &Path) -> anyhow::Result<Option<Map<String, Value>>> {
    let path = snapshot_path(home);
    if !path.exists() {
        return Ok(None);
    }
    let text =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let value: Value = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    let state = value
        .get("state")
        .and_then(Value::as_object)
        .or_else(|| value.as_object())
        .cloned()
        .unwrap_or_default();
    Ok(Some(state))
}

fn safe_snapshot_from_state(state: &Map<String, Value>) -> Value {
    let mut safe = Map::new();
    for key in WORKSPACE_PATH_ARRAY_KEYS {
        if let Some(value) = state.get(*key) {
            safe.insert((*key).to_string(), json!(dedupe_paths(path_array(value))));
        }
    }
    if let Some(value) = state.get(ACTIVE_WORKSPACE_ROOTS_KEY) {
        safe.insert(
            ACTIVE_WORKSPACE_ROOTS_KEY.to_string(),
            normalize_active_workspace_roots(value),
        );
    }
    for key in WORKSPACE_PATH_MAP_KEYS {
        if let Some(value) = state.get(*key).and_then(Value::as_object) {
            safe.insert(
                (*key).to_string(),
                Value::Object(normalize_path_keyed_map(value)),
            );
        }
    }
    for key in THREAD_STATE_MAP_KEYS {
        if let Some(value) = state.get(*key).and_then(Value::as_object) {
            safe.insert(
                (*key).to_string(),
                Value::Object(normalize_string_keyed_map(value)),
            );
        }
    }
    for key in THREAD_ID_ARRAY_KEYS {
        if let Some(value) = state.get(*key) {
            safe.insert(
                (*key).to_string(),
                json!(dedupe_strings(string_array(value))),
            );
        }
    }
    for key in SAFE_TOP_LEVEL_KEYS {
        if let Some(value) = state.get(*key) {
            safe.insert((*key).to_string(), value.clone());
        }
    }
    if let Some(atom) = state
        .get("electron-persisted-atom-state")
        .and_then(Value::as_object)
    {
        let atom = safe_atom_state(atom);
        if !atom.is_empty() {
            safe.insert(
                "electron-persisted-atom-state".to_string(),
                Value::Object(atom),
            );
        }
    }
    json!({
        "version": SNAPSHOT_VERSION,
        "state": safe,
    })
}

fn normalize_current_state(state: &mut Map<String, Value>, changed: &mut BTreeSet<String>) {
    for key in WORKSPACE_PATH_ARRAY_KEYS {
        if let Some(value) = state.get(*key).cloned() {
            let next = json!(dedupe_paths(path_array(&value)));
            replace_if_changed(state, key, next, changed);
        }
    }
    if let Some(value) = state.get(ACTIVE_WORKSPACE_ROOTS_KEY).cloned() {
        replace_if_changed(
            state,
            ACTIVE_WORKSPACE_ROOTS_KEY,
            normalize_active_workspace_roots(&value),
            changed,
        );
    }
    for key in WORKSPACE_PATH_MAP_KEYS {
        if let Some(value) = state.get(*key).and_then(Value::as_object) {
            let next = Value::Object(normalize_path_keyed_map(value));
            replace_if_changed(state, key, next, changed);
        }
    }
    for key in THREAD_STATE_MAP_KEYS {
        if let Some(value) = state.get(*key).and_then(Value::as_object) {
            let next = Value::Object(normalize_string_keyed_map(value));
            replace_if_changed(state, key, next, changed);
        }
    }
    for key in THREAD_ID_ARRAY_KEYS {
        if let Some(value) = state.get(*key).cloned() {
            let next = json!(dedupe_strings(string_array(&value)));
            replace_if_changed(state, key, next, changed);
        }
    }
    if let Some(value) = state
        .get("electron-persisted-atom-state")
        .and_then(Value::as_object)
        .cloned()
    {
        let mut atom = value;
        normalize_atom_state(&mut atom);
        replace_if_changed(
            state,
            "electron-persisted-atom-state",
            Value::Object(atom),
            changed,
        );
    }
}

fn merge_safe_snapshot(
    target: &mut Map<String, Value>,
    snapshot: &Map<String, Value>,
    changed: &mut BTreeSet<String>,
) {
    for key in WORKSPACE_PATH_ARRAY_KEYS {
        let mut paths = target.get(*key).map(path_array).unwrap_or_default();
        paths.extend(snapshot.get(*key).map(path_array).unwrap_or_default());
        if !paths.is_empty() {
            replace_if_changed(target, key, json!(dedupe_paths(paths)), changed);
        }
    }
    let mut active_paths = target
        .get(ACTIVE_WORKSPACE_ROOTS_KEY)
        .map(path_array)
        .unwrap_or_default();
    active_paths.extend(
        snapshot
            .get(ACTIVE_WORKSPACE_ROOTS_KEY)
            .map(path_array)
            .unwrap_or_default(),
    );
    let active_paths = dedupe_paths(active_paths);
    if !active_paths.is_empty() {
        let target_is_array = target
            .get(ACTIVE_WORKSPACE_ROOTS_KEY)
            .is_some_and(Value::is_array);
        let snapshot_is_array = snapshot
            .get(ACTIVE_WORKSPACE_ROOTS_KEY)
            .is_some_and(Value::is_array);
        let next = if target_is_array || snapshot_is_array || active_paths.len() > 1 {
            json!(active_paths)
        } else {
            json!(active_paths[0])
        };
        replace_if_changed(target, ACTIVE_WORKSPACE_ROOTS_KEY, next, changed);
    }
    for key in WORKSPACE_PATH_MAP_KEYS {
        let snapshot_map = snapshot.get(*key).and_then(Value::as_object);
        let current_map = target.get(*key).and_then(Value::as_object);
        let merged = merge_path_keyed_maps(snapshot_map, current_map);
        if !merged.is_empty() {
            replace_if_changed(target, key, Value::Object(merged), changed);
        }
    }
    for key in THREAD_STATE_MAP_KEYS {
        let snapshot_map = snapshot.get(*key).and_then(Value::as_object);
        let current_map = target.get(*key).and_then(Value::as_object);
        let merged = merge_string_keyed_maps(snapshot_map, current_map);
        if !merged.is_empty() {
            replace_if_changed(target, key, Value::Object(merged), changed);
        }
    }
    for key in THREAD_ID_ARRAY_KEYS {
        let mut ids = target.get(*key).map(string_array).unwrap_or_default();
        ids.extend(snapshot.get(*key).map(string_array).unwrap_or_default());
        if !ids.is_empty() {
            replace_if_changed(target, key, json!(dedupe_strings(ids)), changed);
        }
    }
    for key in SAFE_TOP_LEVEL_KEYS {
        if let Some(value) = snapshot.get(*key) {
            replace_if_changed(target, key, value.clone(), changed);
        }
    }
    if let Some(snapshot_atom) = snapshot
        .get("electron-persisted-atom-state")
        .and_then(Value::as_object)
    {
        let mut atom = target
            .get("electron-persisted-atom-state")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for (key, value) in safe_atom_state(snapshot_atom) {
            atom.insert(key, value);
        }
        normalize_atom_state(&mut atom);
        replace_if_changed(
            target,
            "electron-persisted-atom-state",
            Value::Object(atom),
            changed,
        );
    }
}

fn replace_if_changed(
    target: &mut Map<String, Value>,
    key: &str,
    value: Value,
    changed: &mut BTreeSet<String>,
) {
    if target.get(key) != Some(&value) {
        target.insert(key.to_string(), value);
        changed.insert(key.to_string());
    }
}

fn safe_atom_state(atom: &Map<String, Value>) -> Map<String, Value> {
    atom.iter()
        .filter(|(key, _)| is_safe_atom_key(key))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn normalize_atom_state(atom: &mut Map<String, Value>) {
    if let Some(value) = atom.remove("service-tier-default") {
        atom.entry("default-service-tier".to_string())
            .or_insert(value);
    }
}

fn is_safe_atom_key(key: &str) -> bool {
    SAFE_ATOM_KEYS.contains(&key)
        || key.starts_with("app-shell:right-panel-width:")
        || key.starts_with("avatar-overlay-")
        || key.starts_with("electron:onboarding-")
        || key.starts_with("first-awake-pet-notification-")
        || key.starts_with("sidebar-project-expanded-")
        || key.starts_with("thread-summary-panel-section-expanded-")
}

fn merge_path_keyed_maps(
    snapshot: Option<&Map<String, Value>>,
    current: Option<&Map<String, Value>>,
) -> Map<String, Value> {
    let mut merged = Map::new();
    if let Some(snapshot) = snapshot {
        for (key, value) in normalize_path_keyed_map(snapshot) {
            merged.insert(key, value);
        }
    }
    if let Some(current) = current {
        for (key, value) in normalize_path_keyed_map(current) {
            merged.insert(key, value);
        }
    }
    merged
}

fn merge_string_keyed_maps(
    snapshot: Option<&Map<String, Value>>,
    current: Option<&Map<String, Value>>,
) -> Map<String, Value> {
    let mut merged = Map::new();
    if let Some(snapshot) = snapshot {
        for (key, value) in normalize_string_keyed_map(snapshot) {
            merged.insert(key, value);
        }
    }
    if let Some(current) = current {
        for (key, value) in normalize_string_keyed_map(current) {
            merged.insert(key, value);
        }
    }
    merged
}

fn normalize_path_keyed_map(map: &Map<String, Value>) -> Map<String, Value> {
    let mut next = Map::new();
    for (key, value) in map {
        if let Some(path) = normalize_desktop_path(key) {
            next.insert(path, value.clone());
        }
    }
    next
}

fn normalize_string_keyed_map(map: &Map<String, Value>) -> Map<String, Value> {
    let mut next = Map::new();
    for (key, value) in map {
        let key = key.trim();
        if !key.is_empty() {
            next.insert(key.to_string(), value.clone());
        }
    }
    next
}

fn path_array(value: &Value) -> Vec<String> {
    if let Some(items) = value.as_array() {
        items
            .iter()
            .filter_map(Value::as_str)
            .filter_map(normalize_desktop_path)
            .collect()
    } else if let Some(value) = value.as_str() {
        normalize_desktop_path(value).into_iter().collect()
    } else {
        Vec::new()
    }
}

fn normalize_active_workspace_roots(value: &Value) -> Value {
    let normalized = dedupe_paths(path_array(value));
    if value.is_array() {
        json!(normalized)
    } else if let Some(first) = normalized.first() {
        json!(first)
    } else {
        value.clone()
    }
}

fn dedupe_paths(paths: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for path in paths {
        let comparable = path
            .replace('/', r"\")
            .trim_end_matches('\\')
            .to_ascii_lowercase();
        if seen.insert(comparable) {
            result.push(path);
        }
    }
    result
}

fn string_array(value: &Value) -> Vec<String> {
    if let Some(items) = value.as_array() {
        items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(ToString::to_string)
            .collect()
    } else if let Some(value) = value.as_str() {
        let value = value.trim();
        if value.is_empty() {
            Vec::new()
        } else {
            vec![value.to_string()]
        }
    } else {
        Vec::new()
    }
}

fn dedupe_strings(items: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for item in items {
        if seen.insert(item.clone()) {
            result.push(item);
        }
    }
    result
}

fn normalize_desktop_path(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut path = trimmed.replace('/', r"\");
    while path.len() > 3 && path.ends_with('\\') {
        path.pop();
    }
    Some(path)
}

fn create_backup(home: &Path, original_text: &str) -> anyhow::Result<PathBuf> {
    let backup_root = home.join(BACKUP_ROOT);
    fs::create_dir_all(&backup_root)?;
    let timestamp = now_ms();
    let root = (0..1000)
        .map(|suffix| {
            if suffix == 0 {
                backup_root.join(timestamp.to_string())
            } else {
                backup_root.join(format!("{timestamp}-{suffix}"))
            }
        })
        .find(|candidate| !candidate.exists())
        .ok_or_else(|| anyhow::anyhow!("无法分配唯一的 App State 备份目录"))?;
    fs::create_dir(&root)?;
    let result = (|| -> anyhow::Result<()> {
        let original_state =
            fs::read(state_path(home)).unwrap_or_else(|_| original_text.as_bytes().to_vec());
        crate::settings::atomic_write(&root.join(GLOBAL_STATE_FILE), &original_state)?;
        let original_backup = fs::read(home.join(GLOBAL_STATE_BACKUP_FILE)).ok();
        if let Some(bytes) = original_backup.as_deref() {
            crate::settings::atomic_write(&root.join(GLOBAL_STATE_BACKUP_FILE), bytes)?;
        }
        let metadata = serde_json::to_string_pretty(&json!({
            "version": SNAPSHOT_VERSION,
            "managedBy": BACKUP_MANAGED_BY,
            "createdAtMs": timestamp,
            "stateFileExisted": state_path(home).is_file(),
            "backupFileExisted": original_backup.is_some(),
        }))?;
        crate::settings::atomic_write(&root.join("metadata.json"), metadata.as_bytes())?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&root);
        return Err(error);
    }
    Ok(root)
}

fn write_state_pair_with_rollback(home: &Path, next: &[u8]) -> anyhow::Result<()> {
    fs::create_dir_all(home)?;
    let state = state_path(home);
    let backup = home.join(GLOBAL_STATE_BACKUP_FILE);
    let original_state = fs::read(&state).ok();
    let original_backup = fs::read(&backup).ok();

    crate::settings::atomic_write(&state, next)?;
    if let Err(error) = crate::settings::atomic_write(&backup, next) {
        let state_restore = restore_optional_file(&state, original_state.as_deref());
        let backup_restore = restore_optional_file(&backup, original_backup.as_deref());
        let restore_error = state_restore.err().or_else(|| backup_restore.err());
        if let Some(restore_error) = restore_error {
            return Err(error).context(format!(
                "failed to update Codex App State backup and rollback was incomplete: {restore_error}"
            ));
        }
        return Err(error)
            .context("failed to update Codex App State backup; original files restored");
    }
    Ok(())
}

pub fn validate_local_full_access_state(home: &Path) -> anyhow::Result<()> {
    let state = load_global_state(home)?.context("Codex App State was not created")?;
    let atom = state
        .get("electron-persisted-atom-state")
        .and_then(Value::as_object)
        .context("Codex persisted atom state is missing")?;
    let local_mode = atom
        .get("agent-mode-by-host-id")
        .and_then(Value::as_object)
        .and_then(|modes| modes.get("local"))
        .and_then(Value::as_str);
    anyhow::ensure!(
        local_mode == Some("full-access"),
        "local agent mode was not persisted as full-access"
    );

    let selection = atom
        .get("permission-selection-by-host-id:local")
        .and_then(Value::as_object)
        .context("local permission selection is missing")?;
    anyhow::ensure!(
        selection.get("kind").and_then(Value::as_str) == Some("agent-mode")
            && selection.get("agentMode").and_then(Value::as_str) == Some("full-access"),
        "local permission selection was not persisted as full-access"
    );
    anyhow::ensure!(
        atom.get("skip-full-access-confirm")
            .and_then(Value::as_bool)
            == Some(true),
        "full-access confirmation preference was not persisted"
    );
    Ok(())
}

fn restore_state_pair_from_backup(home: &Path, backup_root: &Path) -> anyhow::Result<()> {
    let metadata: Value = serde_json::from_slice(&fs::read(backup_root.join("metadata.json"))?)?;
    let state_existed = metadata
        .get("stateFileExisted")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let backup_existed = metadata
        .get("backupFileExisted")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let original_state = state_existed
        .then(|| fs::read(backup_root.join(GLOBAL_STATE_FILE)))
        .transpose()?;
    let original_backup = backup_existed
        .then(|| fs::read(backup_root.join(GLOBAL_STATE_BACKUP_FILE)))
        .transpose()?;
    restore_optional_file(&state_path(home), original_state.as_deref())?;
    restore_optional_file(
        &home.join(GLOBAL_STATE_BACKUP_FILE),
        original_backup.as_deref(),
    )?;
    Ok(())
}

fn restore_optional_file(path: &Path, original: Option<&[u8]>) -> anyhow::Result<()> {
    match original {
        Some(bytes) => crate::settings::atomic_write(path, bytes),
        None if path.exists() => Ok(fs::remove_file(path)?),
        None => Ok(()),
    }
}

fn ensure_write_headroom(home: &Path, estimated_bytes: u64) -> anyhow::Result<()> {
    crate::mirror_access::ensure_storage_headroom(
        home,
        estimated_bytes,
        crate::mirror_access::MIN_SAFE_FREE_SPACE_BYTES,
    )
    .map(|_| ())
}

fn prune_backups(home: &Path) -> anyhow::Result<()> {
    let root = home.join(BACKUP_ROOT);
    if !root.exists() {
        return Ok(());
    }
    let mut managed = Vec::new();
    for entry in fs::read_dir(&root)? {
        let path = entry?.path();
        if !path.is_dir() || !path.starts_with(&root) || path == root {
            continue;
        }
        let Ok(text) = fs::read_to_string(path.join("metadata.json")) else {
            continue;
        };
        let Ok(metadata) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if metadata.get("managedBy").and_then(Value::as_str) == Some(BACKUP_MANAGED_BY) {
            managed.push(path);
        }
    }
    managed.sort_by(|left, right| right.file_name().cmp(&left.file_name()));
    let mut kept_count = 0usize;
    let mut kept_bytes = 0u64;
    for path in managed {
        let size = directory_size_bytes(&path).unwrap_or(BACKUP_TOTAL_BUDGET_BYTES);
        let keep_latest = kept_count == 0;
        let within_count = kept_count < BACKUP_KEEP_COUNT;
        let within_budget = kept_bytes.saturating_add(size) <= BACKUP_TOTAL_BUDGET_BYTES;
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
        if entry.file_type()?.is_dir() {
            total = total.saturating_add(directory_size_bytes(&entry.path())?);
        } else {
            total = total.saturating_add(entry.metadata()?.len());
        }
    }
    Ok(total)
}

fn state_path(home: &Path) -> PathBuf {
    home.join(GLOBAL_STATE_FILE)
}

fn snapshot_path(home: &Path) -> PathBuf {
    home.join(BACKUP_ROOT).join(SNAPSHOT_FILE)
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
