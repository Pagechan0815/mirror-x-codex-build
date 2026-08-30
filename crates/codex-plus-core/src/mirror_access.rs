use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use serde_json::json;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use toml_edit::{DocumentMut, Item, Table, value};

use crate::model_suffix::{ModelCatalogEntry, build_model_catalog_json_with_capabilities};
use crate::settings::{
    AggregateRelayMember, AggregateRelayProfile, AggregateRelayStrategy, BackendSettings,
    LaunchMode, RelayMode, RelayProfile, RelayProtocol, SettingsStore,
};

pub const MIRROR_BASE_URL: &str = "https://api.jingziai.club/v1";
/// Manager ownership/profile id and compatibility alias for older persisted threads.
pub const MIRROR_PROVIDER_ID: &str = "mirrorplus";
/// CodexPlusPlus's established transport provider id. The root selector and primary
/// table use this id; `mirrorplus` remains a second, equivalent compatibility table.
const MIRROR_CODEX_PROVIDER_ID: &str = "custom";
/// 接管操作需要为临时文件、baseline 和 Codex 后续启动保留的最低余量。
pub const MIN_SAFE_FREE_SPACE_BYTES: u64 = 64 * 1024 * 1024;
/// Codex Desktop、Electron 缓存和历史会话读取共同需要的启动余量。
pub const MIN_CODEX_RUNTIME_FREE_SPACE_BYTES: u64 = 512 * 1024 * 1024;
const RECOVERY_FREE_SPACE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_OPERATION_SNAPSHOTS: usize = 12;
const LEGACY_BASELINE_SCHEMA_VERSION: u32 = 1;
const BASELINE_SCHEMA_VERSION: u32 = 2;
const LEGACY_MANAGED_STATE_SCHEMA_VERSION: u32 = 1;
const MANAGED_STATE_SCHEMA_VERSION: u32 = 2;
const BASELINE_DIR: &str = "baseline-v1";
const MANAGED_STATE_FILE: &str = "managed-access.json";
const CATALOG_FILE: &str = "mirrorplus-model-catalog.json";

#[cfg(test)]
thread_local! {
    static FAIL_PROVIDER_REPAIR_AFTER_CATALOG_WRITE: std::cell::Cell<bool> = const {
        std::cell::Cell::new(false)
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MirrorAccessMode {
    MixedApi,
    PureApi,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestorableChatgptLogin {
    AuthFile,
    CredentialStore {
        credentials_store: String,
        forced_login_method: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MirrorModel {
    pub id: String,
    pub display_name: String,
    pub context_window: Option<u64>,
    pub context_source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MirrorModelDiscovery {
    pub models: Vec<MirrorModel>,
    pub default_model: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorAccessGroup {
    pub id: String,
    pub label: String,
    pub api_key: String,
    pub discovery: MirrorModelDiscovery,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MirrorAccessStatus {
    pub phase: String,
    pub active: bool,
    pub mode: Option<MirrorAccessMode>,
    pub model_count: usize,
    pub default_model: String,
    pub current_provider: String,
    pub original_provider: Option<String>,
    pub baseline_exists: bool,
    pub baseline_created_at_ms: Option<u64>,
    pub session_sync_status: String,
    pub mcp_server_count: usize,
    pub plugin_marketplace_status: String,
    pub last_message: String,
    pub last_operation_at_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MirrorEnableResult {
    pub status: MirrorAccessStatus,
    pub models: Vec<MirrorModel>,
}

#[derive(Debug, Clone)]
pub struct MirrorEnableTransaction {
    pub result: MirrorEnableResult,
    pub probe_profiles: Vec<MirrorProbeProfile>,
    snapshot_dir: std::path::PathBuf,
    initialized_plugin_marketplace: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorProbeProfile {
    pub label: String,
    pub model: String,
    pub profile: RelayProfile,
}

impl MirrorEnableTransaction {
    pub fn record_plugin_marketplace_initialization(&mut self, initialized: bool) {
        self.initialized_plugin_marketplace |= initialized;
    }

    pub fn rollback(
        self,
        home: &Path,
        state_dir: &Path,
        settings_path: &Path,
    ) -> anyhow::Result<MirrorAccessStatus> {
        restore_operation_snapshot(home, state_dir, settings_path, &self.snapshot_dir)?;
        if self.initialized_plugin_marketplace {
            crate::plugin_marketplace::rollback_openai_curated_remote_marketplace_initialization(
                home,
            )?;
        }
        try_access_status(home, state_dir)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MirrorRestoreResult {
    pub status: MirrorAccessStatus,
    pub original_provider: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BaselineFile {
    id: String,
    existed: bool,
    sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BaselineManifest {
    schema_version: u32,
    created_at_ms: u64,
    #[serde(default)]
    codex_home: Option<String>,
    original_provider: String,
    files: Vec<BaselineFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OperationSnapshotManifest {
    schema_version: u32,
    created_at_ms: u64,
    operation: String,
    files: Vec<BaselineFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManagedState {
    schema_version: u32,
    #[serde(default)]
    codex_home: Option<String>,
    active: bool,
    mode: Option<MirrorAccessMode>,
    model_count: usize,
    default_model: String,
    session_sync_status: String,
    last_message: String,
    last_operation_at_ms: u64,
}

impl Default for ManagedState {
    fn default() -> Self {
        Self {
            schema_version: MANAGED_STATE_SCHEMA_VERSION,
            codex_home: None,
            active: false,
            mode: None,
            model_count: 0,
            default_model: String::new(),
            session_sync_status: "not_run".to_string(),
            last_message: String::new(),
            last_operation_at_ms: now_ms(),
        }
    }
}

pub async fn discover_models(api_key: &str) -> anyhow::Result<MirrorModelDiscovery> {
    discover_models_at(api_key, MIRROR_BASE_URL).await
}

pub async fn discover_models_at(
    api_key: &str,
    base_url: &str,
) -> anyhow::Result<MirrorModelDiscovery> {
    let api_key = api_key.trim();
    if api_key.is_empty() {
        bail!("请输入镜子AI API Key。");
    }
    let endpoint = format!("{}/models", base_url.trim_end_matches('/'));
    let client = crate::http_client::proxied_client("mirrorplus/ModelDiscovery")?;
    let response = client
        .get(&endpoint)
        .bearer_auth(api_key)
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .with_context(|| "无法连接镜子AI模型服务")?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        bail!("API Key 无效或没有模型访问权限。");
    }
    if !status.is_success() {
        bail!("镜子AI模型服务返回 HTTP {}。", status.as_u16());
    }
    let payload: Value = response
        .json()
        .await
        .with_context(|| "镜子AI模型服务返回了无效 JSON")?;
    parse_model_discovery(&payload)
}

pub fn parse_model_discovery(payload: &Value) -> anyhow::Result<MirrorModelDiscovery> {
    let rows = payload
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("模型响应缺少 data 数组。"))?;
    let mut seen = HashSet::new();
    let mut models = Vec::new();
    for row in rows {
        let Some(id) = row.get("id").and_then(Value::as_str).map(str::trim) else {
            continue;
        };
        if id.is_empty() || !seen.insert(id.to_string()) {
            continue;
        }
        let display_name = row
            .get("display_name")
            .or_else(|| row.get("name"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(id)
            .to_string();
        let context_window = ["context_window", "max_context_window", "context_length"]
            .into_iter()
            .find_map(|key| row.get(key).and_then(json_u64));
        models.push(MirrorModel {
            id: id.to_string(),
            display_name,
            context_window,
            context_source: if context_window.is_some() {
                "service".to_string()
            } else {
                "fallback".to_string()
            },
        });
    }
    if models.is_empty() {
        bail!("当前 API Key 没有返回任何可用模型。");
    }
    let default_model = preferred_default_model(&models);
    Ok(MirrorModelDiscovery {
        models,
        default_model,
    })
}

pub fn select_models(
    discovery: MirrorModelDiscovery,
    selected_model_ids: &[String],
    default_model: &str,
) -> anyhow::Result<MirrorModelDiscovery> {
    let selected = selected_model_ids
        .iter()
        .map(|model| model.trim())
        .filter(|model| !model.is_empty())
        .collect::<HashSet<_>>();
    if selected.is_empty() {
        bail!("请至少勾选一个要插入 Codex 的模型。");
    }

    let available = discovery
        .models
        .iter()
        .map(|model| model.id.as_str())
        .collect::<HashSet<_>>();
    let mut unavailable = selected
        .iter()
        .filter(|model| !available.contains(**model))
        .map(|model| (*model).to_string())
        .collect::<Vec<_>>();
    unavailable.sort();
    if !unavailable.is_empty() {
        bail!(
            "所选模型已不可用，请重新验证 Key：{}",
            unavailable.join("、")
        );
    }

    let models = discovery
        .models
        .into_iter()
        .filter(|model| selected.contains(model.id.as_str()))
        .collect::<Vec<_>>();
    let default_model = default_model.trim();
    if default_model.is_empty() || !models.iter().any(|model| model.id == default_model) {
        bail!("默认模型必须是已勾选的模型。");
    }

    Ok(MirrorModelDiscovery {
        models,
        default_model: default_model.to_string(),
    })
}

pub fn try_access_status(home: &Path, state_dir: &Path) -> anyhow::Result<MirrorAccessStatus> {
    let state = load_managed_state_or_default(state_dir)?;
    let baseline = load_valid_baseline_if_present(state_dir)?;
    if let Some(baseline) = baseline.as_ref() {
        validate_access_home_binding(home, &state, baseline, state_requires_home_binding(&state))?;
    } else if state_requires_home_binding(&state) {
        validate_state_home_binding(home, &state)?;
    }
    let current_provider = read_provider(&home.join("config.toml"));
    // `custom` and `mirrorplus` can both be user-owned provider ids. Durable
    // state establishes current ownership; the baseline only identifies an old
    // branded-provider takeover that was interrupted before state was persisted.
    let legacy_managed_provider = current_provider == MIRROR_PROVIDER_ID
        && baseline
            .as_ref()
            .is_some_and(|value| value.original_provider != MIRROR_PROVIDER_ID);
    let active = (state.active
        && (current_provider == MIRROR_CODEX_PROVIDER_ID
            || current_provider == MIRROR_PROVIDER_ID))
        || legacy_managed_provider;
    if (active || state.active) && baseline.is_none() {
        bail!("检测到 mirrorplus 接管状态，但恢复 baseline 缺失；已停止所有配置和会话修改。");
    }
    let phase = if matches!(
        state.session_sync_status.as_str(),
        "pending_restore" | "restore_failed"
    ) || state.last_message.starts_with("恢复失败")
    {
        "restore_failed"
    } else if active && state.session_sync_status == "synced" {
        "active"
    } else if active {
        "active_degraded"
    } else {
        "unmanaged"
    };
    let marketplace = crate::plugin_marketplace::openai_curated_remote_marketplace_status(home);
    let plugin_marketplace_status = if marketplace.config_registered {
        "ready"
    } else if marketplace.marketplace_root.is_some() {
        "cached"
    } else {
        "missing"
    };
    Ok(MirrorAccessStatus {
        phase: phase.to_string(),
        active,
        mode: active.then_some(state.mode).flatten(),
        model_count: if active { state.model_count } else { 0 },
        default_model: if active {
            state.default_model
        } else {
            String::new()
        },
        current_provider,
        original_provider: baseline
            .as_ref()
            .map(|value| value.original_provider.clone()),
        baseline_exists: baseline.is_some(),
        baseline_created_at_ms: baseline.as_ref().map(|value| value.created_at_ms),
        session_sync_status: state.session_sync_status,
        mcp_server_count: mcp_server_count(&home.join("config.toml")),
        plugin_marketplace_status: plugin_marketplace_status.to_string(),
        last_message: state.last_message,
        last_operation_at_ms: Some(state.last_operation_at_ms),
    })
}

fn state_requires_home_binding(state: &ManagedState) -> bool {
    state.active || !matches!(state.session_sync_status.as_str(), "not_run" | "synced")
}

fn validate_state_home_binding(home: &Path, state: &ManagedState) -> anyhow::Result<String> {
    let current = crate::codex_home::codex_home_identity(home)?;
    if let Some(expected) = state.codex_home.as_deref()
        && expected != current
    {
        bail!(
            "当前 CODEX_HOME 与接管状态绑定目录不一致（绑定：{expected}；当前：{current}）。已停止所有配置和会话修改；请恢复原 CODEX_HOME 后重试。"
        );
    }
    Ok(current)
}

fn validate_access_home_binding(
    home: &Path,
    state: &ManagedState,
    baseline: &BaselineManifest,
    required: bool,
) -> anyhow::Result<String> {
    let current = crate::codex_home::codex_home_identity(home)?;
    if required {
        validate_state_home_binding(home, state)?;
        if let Some(expected) = baseline.codex_home.as_deref()
            && expected != current
        {
            bail!(
                "当前 CODEX_HOME 与恢复 baseline 绑定目录不一致（绑定：{expected}；当前：{current}）。已停止恢复，两个目录均未修改；请恢复原 CODEX_HOME 后重试。"
            );
        }
    }
    if let (Some(state_home), Some(baseline_home)) =
        (state.codex_home.as_deref(), baseline.codex_home.as_deref())
        && state_home != baseline_home
    {
        bail!("接管状态与恢复 baseline 绑定了不同的 CODEX_HOME，已停止所有修改并保留恢复数据。");
    }

    if required && state.codex_home.is_none() && baseline.codex_home.is_none() {
        let provider = read_provider(&home.join("config.toml"));
        let legacy_target_is_plausible = provider == MIRROR_CODEX_PROVIDER_ID
            || provider == MIRROR_PROVIDER_ID
            || provider == baseline.original_provider;
        if !legacy_target_is_plausible {
            bail!(
                "旧版恢复 baseline 未记录 CODEX_HOME，且当前目录无法与其接管状态对应。已停止恢复；请切回创建该 baseline 时使用的 CODEX_HOME。"
            );
        }
    }
    Ok(current)
}

/// 删除 Mirror X Codex 自有状态前，确认 Codex 配置和历史会话已经退出接管状态。
///
/// 此检查严格读取状态和 baseline；损坏或不可读时必须保留恢复数据。
pub fn ensure_restored_for_state_removal(home: &Path, state_dir: &Path) -> anyhow::Result<()> {
    validate_existing_config(home).context("无法确认 Codex 配置是否已恢复")?;
    let state =
        load_managed_state_or_default(state_dir).context("无法确认 Mirror X Codex 接管状态")?;
    let baseline = load_valid_baseline_if_present(state_dir).context("接管 baseline 校验失败")?;
    if let Some(baseline) = baseline.as_ref() {
        validate_access_home_binding(home, &state, baseline, state_requires_home_binding(&state))?;
    }
    let current_provider = read_provider(&home.join("config.toml"));
    let legacy_managed_provider = current_provider == MIRROR_PROVIDER_ID
        && baseline
            .as_ref()
            .is_some_and(|value| value.original_provider != MIRROR_PROVIDER_ID);
    if state.active || legacy_managed_provider {
        bail!("Codex 仍处于 Mirror X Codex 接管状态，请先执行“恢复接入前状态”。");
    }

    let sync_complete = state.session_sync_status == "synced";
    let never_managed = state.session_sync_status == "not_run";
    if !sync_complete && !never_managed {
        bail!(
            "历史会话恢复尚未完成（当前状态：{}），请先修复会话归属或重新执行恢复。",
            state.session_sync_status
        );
    }
    Ok(())
}

/// 状态页必须可渲染，但不能把无法读取的托管状态伪装成未接管。
/// 任何会修改 Codex 或会话的入口都必须使用 `try_access_status`。
pub fn access_status(home: &Path, state_dir: &Path) -> MirrorAccessStatus {
    match try_access_status(home, state_dir) {
        Ok(status) => status,
        Err(error) => unreadable_access_status(home, state_dir, &error),
    }
}

fn unreadable_access_status(
    home: &Path,
    state_dir: &Path,
    error: &anyhow::Error,
) -> MirrorAccessStatus {
    let baseline = load_baseline_manifest(state_dir).ok();
    let baseline_exists = fs::metadata(state_dir.join(BASELINE_DIR)).is_ok();
    let marketplace = crate::plugin_marketplace::openai_curated_remote_marketplace_status(home);
    let plugin_marketplace_status = if marketplace.config_registered {
        "ready"
    } else if marketplace.marketplace_root.is_some() {
        "cached"
    } else {
        "missing"
    };
    MirrorAccessStatus {
        phase: "state_unreadable".to_string(),
        active: false,
        mode: None,
        model_count: 0,
        default_model: String::new(),
        current_provider: read_provider(&home.join("config.toml")),
        original_provider: baseline
            .as_ref()
            .map(|value| value.original_provider.clone()),
        baseline_exists,
        baseline_created_at_ms: baseline.as_ref().map(|value| value.created_at_ms),
        session_sync_status: "state_unreadable".to_string(),
        mcp_server_count: mcp_server_count(&home.join("config.toml")),
        plugin_marketplace_status: plugin_marketplace_status.to_string(),
        last_message: format!("接管状态无法读取，已停止所有配置和会话修改：{error:#}"),
        last_operation_at_ms: None,
    }
}

pub fn validate_existing_config(home: &Path) -> anyhow::Result<()> {
    let config_path = home.join("config.toml");
    let text = match fs::read_to_string(&config_path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| "无法读取现有 config.toml"),
    };
    if text.trim().is_empty() {
        return Ok(());
    }
    text.parse::<DocumentMut>()
        .map(|_| ())
        .with_context(|| "现有 config.toml 无法解析，未执行接管")
}

/// 返回目标路径所在卷的可用空间。路径尚未创建时向上查找现有父目录。
pub fn available_space_bytes(path: &Path) -> anyhow::Result<u64> {
    let mut existing = path;
    while !existing.exists() {
        existing = existing
            .parent()
            .ok_or_else(|| anyhow::anyhow!("无法定位 {} 所在磁盘", path.display()))?;
    }
    fs2::available_space(existing)
        .with_context(|| format!("无法读取 {} 所在磁盘的可用空间", path.display()))
}

/// 在任何写入开始前检查空间，避免低磁盘时只写入部分 Codex 配置。
pub fn ensure_storage_headroom(
    path: &Path,
    planned_bytes: u64,
    reserve_bytes: u64,
) -> anyhow::Result<u64> {
    let available = available_space_bytes(path)?;
    let required = planned_bytes.saturating_add(reserve_bytes);
    if available < required {
        bail!(
            "{} 所在磁盘剩余空间不足：可用 {} MB，至少需要 {} MB；未修改 Codex 配置。",
            path.display(),
            available / (1024 * 1024),
            (required.saturating_add(1024 * 1024 - 1)) / (1024 * 1024)
        );
    }
    Ok(available)
}

/// Collect every volume Codex can write to while starting, then keep one
/// representative path per volume. This covers split CODEX_HOME/SQLite setups
/// as well as Electron caches and temporary files that remain on the system drive.
pub fn codex_runtime_storage_paths(
    home: &Path,
    state_dir: &Path,
    sqlite_home: Option<&Path>,
    app_dir: Option<&Path>,
) -> Vec<PathBuf> {
    let mut candidates = vec![home.to_path_buf(), state_dir.to_path_buf()];
    candidates.extend(sqlite_home.map(Path::to_path_buf));
    candidates.extend(app_dir.map(Path::to_path_buf));
    candidates.extend(
        ["LOCALAPPDATA", "APPDATA", "TEMP", "TMP"]
            .into_iter()
            .filter_map(std::env::var_os)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from),
    );
    candidates.push(std::env::temp_dir());
    candidates.extend(crate::app_paths::user_data_candidates());
    storage_paths_by_volume(candidates)
}

pub fn storage_paths_by_volume(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut volumes = BTreeMap::<String, PathBuf>::new();
    for path in paths {
        if path.as_os_str().is_empty() {
            continue;
        }
        volumes.entry(storage_volume_key(&path)).or_insert(path);
    }
    volumes.into_values().collect()
}

fn storage_volume_key(path: &Path) -> String {
    let mut existing = path;
    while !existing.exists() {
        let Some(parent) = existing.parent() else {
            break;
        };
        existing = parent;
    }
    let absolute = fs::canonicalize(existing).unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir().unwrap_or_default().join(path)
        }
    });
    if let Some(std::path::Component::Prefix(prefix)) = absolute.components().next() {
        return format!(
            "prefix:{}",
            prefix.as_os_str().to_string_lossy().to_ascii_lowercase()
        );
    }
    if absolute.is_absolute() {
        return "root:/".to_string();
    }
    format!("path:{}", absolute.to_string_lossy().to_ascii_lowercase())
}

pub fn enable_access(
    home: &Path,
    state_dir: &Path,
    settings_path: &Path,
    api_key: &str,
    mode: MirrorAccessMode,
    discovery: MirrorModelDiscovery,
) -> anyhow::Result<MirrorEnableResult> {
    let default_model = discovery.default_model.clone();
    enable_grouped_access(
        home,
        state_dir,
        settings_path,
        mode,
        vec![MirrorAccessGroup {
            id: "default".to_string(),
            label: "镜子AI".to_string(),
            api_key: api_key.to_string(),
            discovery,
        }],
        &default_model,
    )
}

/// Describes the non-secret authentication evidence an active Pure API cycle
/// can restore. Credential bytes never leave this module.
pub fn restorable_chatgpt_login(
    state_dir: &Path,
) -> anyhow::Result<Option<RestorableChatgptLogin>> {
    let managed_state = load_managed_state_or_default(state_dir)
        .with_context(|| "无法确认当前接管模式，未检查受保护的 ChatGPT 登录 baseline")?;
    if !managed_state.active || managed_state.mode != Some(MirrorAccessMode::PureApi) {
        return Ok(None);
    }
    let config = baseline_optional_contents(state_dir, "config")?;
    let config = parse_optional_toml(config.as_deref(), "baseline config.toml")?;
    let credentials_store = match config.get("cli_auth_credentials_store") {
        Some(item) => item
            .as_str()
            .ok_or_else(|| {
                anyhow::anyhow!("baseline config.toml 的 cli_auth_credentials_store 不是字符串")
            })?
            .trim()
            .to_ascii_lowercase(),
        None => "auto".to_string(),
    };
    if !matches!(credentials_store.as_str(), "auto" | "file" | "keyring") {
        bail!(
            "baseline config.toml 的 cli_auth_credentials_store 不受支持：{}",
            credentials_store
        );
    }
    let forced_login_method = match config.get("forced_login_method") {
        Some(item) => Some(
            item.as_str()
                .ok_or_else(|| {
                    anyhow::anyhow!("baseline config.toml 的 forced_login_method 不是字符串")
                })?
                .trim()
                .to_ascii_lowercase(),
        ),
        None => None,
    };
    if forced_login_method.as_deref() == Some("api") {
        return Ok(None);
    }
    if forced_login_method
        .as_deref()
        .is_some_and(|method| method != "chatgpt")
    {
        bail!(
            "baseline config.toml 的 forced_login_method 不受支持：{}",
            forced_login_method.as_deref().unwrap_or_default()
        );
    }
    let auth = baseline_optional_contents(state_dir, "auth")?;
    if credentials_store != "keyring"
        && auth
            .as_deref()
            .is_some_and(auth_contents_have_chatgpt_login)
    {
        return Ok(Some(RestorableChatgptLogin::AuthFile));
    }
    if credentials_store == "file" {
        return Ok(None);
    }
    Ok(Some(RestorableChatgptLogin::CredentialStore {
        credentials_store,
        forced_login_method,
    }))
}

fn auth_contents_have_chatgpt_login(contents: &[u8]) -> bool {
    let Ok(auth) = serde_json::from_slice::<Value>(contents) else {
        return false;
    };
    let is_chatgpt = auth
        .get("auth_mode")
        .and_then(Value::as_str)
        .is_some_and(|mode| mode.eq_ignore_ascii_case("chatgpt"));
    is_chatgpt
        && auth.get("tokens").is_some_and(|tokens| {
            ["access_token", "id_token", "refresh_token"]
                .iter()
                .any(|key| {
                    tokens
                        .get(*key)
                        .and_then(Value::as_str)
                        .is_some_and(|token| !token.trim().is_empty())
                })
        })
}

/// Repairs only an active Mirror-managed access configuration before launch.
///
/// A healthy configuration is strictly read-only. Repair is allowed only when the
/// durable managed state, recovery baseline, and active Manager profile all agree
/// that Mirror owns the live provider. The live config is used as the seed so
/// unrelated Codex, Windows, MCP, and plugin settings are preserved.
pub fn ensure_managed_provider_ready(
    home: &Path,
    state_dir: &Path,
    settings_path: &Path,
) -> anyhow::Result<bool> {
    let managed_state = load_managed_state_or_default(state_dir)
        .with_context(|| "启动前无法确认 Mirror X Codex 接管状态；未修改 Codex 配置")?;
    if !managed_state.active {
        return Ok(false);
    }
    let mode = managed_state
        .mode
        .ok_or_else(|| anyhow::anyhow!("Mirror X Codex 接管状态缺少登录模式；未修改 Codex 配置"))?;

    let settings = load_manager_settings_strict(settings_path)
        .with_context(|| "启动前无法读取 Manager 设置；未修改 Codex 配置")?;
    if !settings.relay_profiles_enabled || settings.active_relay_id != MIRROR_PROVIDER_ID {
        bail!(
            "Mirror X Codex 的接管状态仍有效，但 Manager 的接管开关或 active relay 已不一致；为避免用错误模式覆盖 Pure/Mixed 配置，已停止启动且未修改任何文件。请重新完成 API 接入或执行恢复。"
        );
    }
    load_valid_baseline_if_present(state_dir)?
        .ok_or_else(|| anyhow::anyhow!("Mirror X Codex 接管 baseline 缺失；未修改 Codex 配置"))?;

    let groups = existing_access_groups(settings_path)
        .with_context(|| "启动前无法从 Manager 设置恢复 Mirror 分组；未修改 Codex 配置")?;
    validate_access_groups(&groups, &managed_state.default_model)
        .with_context(|| "启动前 Mirror 分组校验失败；未修改 Codex 配置")?;
    let discovery = combined_discovery(&groups, &managed_state.default_model);
    if managed_state.model_count != discovery.models.len() {
        bail!(
            "Mirror X Codex 接管状态记录 {} 个模型，但 Manager 保存了 {} 个；未修改 Codex 配置",
            managed_state.model_count,
            discovery.models.len()
        );
    }

    let grouped = groups.len() > 1;
    let provider_key = if grouped {
        "codex-plus-aggregate"
    } else {
        groups[0].api_key.trim()
    };
    let provider_base_url = if grouped {
        crate::protocol_proxy::local_responses_proxy_base_url(
            crate::protocol_proxy::DEFAULT_PROTOCOL_PROXY_PORT,
        )
    } else {
        MIRROR_BASE_URL.to_string()
    };

    let config_path = home.join("config.toml");
    let auth_path = home.join("auth.json");
    let catalog_path = home.join(CATALOG_FILE);
    let current_config = read_optional(&config_path)
        .with_context(|| "启动前无法读取 config.toml；未修改 Codex 配置")?;
    let current_auth =
        read_optional(&auth_path).with_context(|| "启动前无法读取 auth.json；未修改 Codex 配置")?;
    let config_seed =
        managed_config_seed_for_mode(state_dir, &managed_state, mode, current_config.as_deref())?;
    let expected_auth = managed_auth_for_mode(
        state_dir,
        &managed_state,
        mode,
        current_auth.as_deref(),
        access_auth_key(&groups, &discovery.default_model),
    )?;

    let config_is_healthy = verify_managed_config(
        &config_path,
        &catalog_path,
        &provider_base_url,
        provider_key,
        mode,
        &discovery,
    )
    .is_ok();
    let auth_config_is_healthy = mode != MirrorAccessMode::MixedApi
        || managed_auth_config_keys_match(current_config.as_deref(), config_seed.as_deref())?;
    let auth_is_healthy =
        verify_optional_file_contents(&auth_path, expected_auth.as_deref(), "auth.json").is_ok();
    if config_is_healthy && auth_config_is_healthy && auth_is_healthy {
        return Ok(false);
    }

    let config = build_managed_config(
        config_seed.as_deref(),
        provider_key,
        &provider_base_url,
        mode,
        &discovery.default_model,
        &catalog_path,
    )?;
    let catalog = build_catalog(&discovery.models);
    let current_catalog = read_optional(&catalog_path)
        .with_context(|| "启动前无法读取模型目录；未修改 Codex 配置")?;
    let estimated_bytes = estimated_operation_bytes(home, state_dir, settings_path);
    ensure_storage_headroom(home, estimated_bytes, MIN_SAFE_FREE_SPACE_BYTES)?;
    ensure_storage_headroom(state_dir, estimated_bytes, MIN_SAFE_FREE_SPACE_BYTES)?;
    let snapshot_dir =
        create_operation_snapshot(home, state_dir, settings_path, "pre-launch-provider-repair")
            .with_context(|| "启动前无法创建完整恢复快照；未修改 Codex 配置")?;

    let repair_result = (|| -> anyhow::Result<()> {
        if current_catalog.as_deref() != Some(catalog.as_bytes()) {
            crate::settings::atomic_write(&catalog_path, catalog.as_bytes())?;
        }
        #[cfg(test)]
        maybe_fail_provider_repair_after_catalog_write()?;
        if current_auth.as_deref() != expected_auth.as_deref() {
            restore_optional(&auth_path, expected_auth.as_deref())?;
        }
        if current_config.as_deref() != Some(config.as_bytes()) {
            crate::settings::atomic_write(&config_path, config.as_bytes())?;
        }
        verify_managed_config(
            &config_path,
            &catalog_path,
            &provider_base_url,
            provider_key,
            mode,
            &discovery,
        )?;
        let repaired_config =
            read_optional(&config_path).with_context(|| "无法回读修复后的 config.toml")?;
        if mode == MirrorAccessMode::MixedApi
            && !managed_auth_config_keys_match(repaired_config.as_deref(), config_seed.as_deref())?
        {
            bail!("修复后的 Mixed API 登录配置与接管 baseline 不一致");
        }
        verify_optional_file_contents(&auth_path, expected_auth.as_deref(), "auth.json")
    })();

    if let Err(error) = repair_result {
        return match restore_operation_snapshot(home, state_dir, settings_path, &snapshot_dir) {
            Ok(()) => Err(error).context("启动前 Mirror provider 自愈失败，已恢复写入前状态"),
            Err(restore_error) => Err(error).context(format!(
                "启动前 Mirror provider 自愈失败，且自动恢复未完整成功：{restore_error:#}；完整快照保留在 {}",
                snapshot_dir.display()
            )),
        };
    }

    Ok(true)
}

#[cfg(test)]
fn maybe_fail_provider_repair_after_catalog_write() -> anyhow::Result<()> {
    if FAIL_PROVIDER_REPAIR_AFTER_CATALOG_WRITE.with(|flag| flag.replace(false)) {
        bail!("injected provider repair failure after catalog write");
    }
    Ok(())
}

pub fn enable_grouped_access(
    home: &Path,
    state_dir: &Path,
    settings_path: &Path,
    mode: MirrorAccessMode,
    groups: Vec<MirrorAccessGroup>,
    default_model: &str,
) -> anyhow::Result<MirrorEnableResult> {
    Ok(enable_grouped_access_transaction(
        home,
        state_dir,
        settings_path,
        mode,
        groups,
        default_model,
    )?
    .result)
}

pub fn enable_grouped_access_transaction(
    home: &Path,
    state_dir: &Path,
    settings_path: &Path,
    mode: MirrorAccessMode,
    groups: Vec<MirrorAccessGroup>,
    default_model: &str,
) -> anyhow::Result<MirrorEnableTransaction> {
    enable_grouped_access_transaction_with_policy(
        home,
        state_dir,
        settings_path,
        mode,
        groups,
        default_model,
        AccessGroupPolicy::MergeExisting,
    )
}

/// The quick setup page submits the complete desired group set. Keeping a group
/// that is absent from that request would retain a stale key the user cannot see
/// or remove, so this entry point replaces only Mirror-managed groups.
pub fn enable_grouped_access_transaction_replacing_groups(
    home: &Path,
    state_dir: &Path,
    settings_path: &Path,
    mode: MirrorAccessMode,
    groups: Vec<MirrorAccessGroup>,
    default_model: &str,
) -> anyhow::Result<MirrorEnableTransaction> {
    enable_grouped_access_transaction_with_policy(
        home,
        state_dir,
        settings_path,
        mode,
        groups,
        default_model,
        AccessGroupPolicy::ReplaceManaged,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccessGroupPolicy {
    MergeExisting,
    ReplaceManaged,
}

fn enable_grouped_access_transaction_with_policy(
    home: &Path,
    state_dir: &Path,
    settings_path: &Path,
    mode: MirrorAccessMode,
    groups: Vec<MirrorAccessGroup>,
    default_model: &str,
    group_policy: AccessGroupPolicy,
) -> anyhow::Result<MirrorEnableTransaction> {
    // 托管状态决定模式切换时如何处理 auth.json，必须在创建 baseline 或快照前确认可读。
    let state_path = state_dir.join(MANAGED_STATE_FILE);
    let old_state = read_optional(&state_path)
        .with_context(|| format!("无法读取接管状态 {}", state_path.display()))?;
    let managed_state = parse_optional_managed_state(&state_path, old_state.as_deref())?;
    validate_existing_manager_settings(settings_path)?;
    let groups = match group_policy {
        AccessGroupPolicy::MergeExisting => merge_existing_access_groups(settings_path, groups)?,
        AccessGroupPolicy::ReplaceManaged => groups,
    };
    validate_access_groups(&groups, default_model)?;
    let discovery = combined_discovery(&groups, default_model);
    let grouped = groups.len() > 1;
    let provider_key = if grouped {
        "codex-plus-aggregate"
    } else {
        groups[0].api_key.trim()
    };
    let provider_base_url = if grouped {
        crate::protocol_proxy::local_responses_proxy_base_url(
            crate::protocol_proxy::DEFAULT_PROTOCOL_PROXY_PORT,
        )
    } else {
        MIRROR_BASE_URL.to_string()
    };
    fs::create_dir_all(home)?;
    fs::create_dir_all(state_dir)?;
    let estimated_bytes = estimated_operation_bytes(home, state_dir, settings_path);
    ensure_storage_headroom(home, estimated_bytes, MIN_CODEX_RUNTIME_FREE_SPACE_BYTES)?;
    ensure_storage_headroom(
        state_dir,
        estimated_bytes,
        MIN_CODEX_RUNTIME_FREE_SPACE_BYTES,
    )?;
    let new_access_cycle = baseline_needs_refresh_for_new_cycle(&managed_state);
    if new_access_cycle {
        refresh_baseline(home, state_dir, settings_path)?;
    } else {
        ensure_baseline(home, state_dir, settings_path)?;
    }
    let baseline = load_baseline_manifest(state_dir)?;
    let binding_state = if new_access_cycle {
        ManagedState::default()
    } else {
        managed_state.clone()
    };
    let codex_home = validate_access_home_binding(home, &binding_state, &baseline, true)?;
    let snapshot_dir = create_operation_snapshot(home, state_dir, settings_path, "pre-enable")?;

    let config_path = home.join("config.toml");
    let auth_path = home.join("auth.json");
    let catalog_path = home.join(CATALOG_FILE);
    let old_config = read_optional(&config_path)?;
    let old_auth = read_optional(&auth_path)?;
    let old_settings = read_optional(settings_path)?;
    let old_catalog = read_optional(&catalog_path)?;

    let catalog = build_catalog(&discovery.models);
    let config_seed =
        managed_config_seed_for_mode(state_dir, &managed_state, mode, old_config.as_deref())?;
    let config = build_managed_config(
        config_seed.as_deref(),
        provider_key,
        &provider_base_url,
        mode,
        &discovery.default_model,
        &catalog_path,
    )?;
    let auth = managed_auth_for_mode(
        state_dir,
        &managed_state,
        mode,
        old_auth.as_deref(),
        access_auth_key(&groups, &discovery.default_model),
    )?;

    let write_result = (|| -> anyhow::Result<Vec<MirrorProbeProfile>> {
        crate::settings::atomic_write(&catalog_path, catalog.as_bytes())?;
        restore_optional(&auth_path, auth.as_deref())?;
        crate::settings::atomic_write(&config_path, config.as_bytes())?;
        save_managed_settings(
            settings_path,
            mode,
            &groups,
            &discovery,
            &config,
            auth.as_deref(),
        )?;
        verify_managed_config(
            &config_path,
            &catalog_path,
            &provider_base_url,
            provider_key,
            mode,
            &discovery,
        )?;
        verify_optional_file_contents(&auth_path, auth.as_deref(), "auth.json")?;
        verify_managed_settings(settings_path, mode, &groups, &discovery)
    })();

    let probe_profiles = match write_result {
        Ok(profiles) => profiles,
        Err(error) => {
            let rollback = [
                restore_optional(&config_path, old_config.as_deref()),
                restore_optional(&auth_path, old_auth.as_deref()),
                restore_optional(settings_path, old_settings.as_deref()),
                restore_optional(&catalog_path, old_catalog.as_deref()),
                restore_optional(&state_path, old_state.as_deref()),
            ];
            return match rollback.into_iter().find_map(Result::err) {
                Some(rollback_error) => Err(error).context(format!(
                    "接管失败，且自动回滚未完整完成：{rollback_error}；操作快照已保留在 {}",
                    snapshot_dir.display()
                )),
                None => Err(error).context("接管失败，已自动恢复本次操作前状态"),
            };
        }
    };

    let state = ManagedState {
        schema_version: MANAGED_STATE_SCHEMA_VERSION,
        codex_home: Some(codex_home),
        active: true,
        mode: Some(mode),
        model_count: discovery.models.len(),
        default_model: discovery.default_model.clone(),
        session_sync_status: "pending".to_string(),
        last_message: "mirror x codex 配置已写入，等待会话修复。".to_string(),
        last_operation_at_ms: now_ms(),
    };
    let state_save_result = save_managed_state(state_dir, &state).and_then(|_| {
        let persisted =
            load_managed_state(state_dir).with_context(|| "接管状态写入后无法重新读取")?;
        if persisted != state {
            bail!("接管状态写入后的内容校验失败。");
        }
        try_access_status(home, state_dir).with_context(|| "接管后的最终状态无法回读")
    });
    let status = match state_save_result {
        Ok(status) => status,
        Err(error) => {
            let rollback = [
                restore_optional(&config_path, old_config.as_deref()),
                restore_optional(&auth_path, old_auth.as_deref()),
                restore_optional(settings_path, old_settings.as_deref()),
                restore_optional(&catalog_path, old_catalog.as_deref()),
                restore_optional(&state_path, old_state.as_deref()),
            ];
            let rollback_error = rollback.into_iter().find_map(Result::err);
            return match rollback_error {
                Some(rollback_error) => Err(error).context(format!(
                    "接管状态保存或最终回读失败，且自动回滚也失败：{rollback_error}"
                )),
                None => Err(error).context("接管状态保存或最终回读失败，已自动恢复本次操作前状态"),
            };
        }
    };
    Ok(MirrorEnableTransaction {
        result: MirrorEnableResult {
            status,
            models: discovery.models,
        },
        probe_profiles,
        snapshot_dir,
        initialized_plugin_marketplace: false,
    })
}

fn validate_access_groups(groups: &[MirrorAccessGroup], default_model: &str) -> anyhow::Result<()> {
    if groups.is_empty() {
        bail!("请至少填写并验证一个分组 Key。");
    }
    let mut group_ids = HashSet::new();
    let mut model_ids = HashSet::new();
    for group in groups {
        if group.id.trim().is_empty()
            || !group
                .id
                .chars()
                .all(|value| value.is_ascii_alphanumeric() || matches!(value, '-' | '_'))
        {
            bail!("模型分组 ID 不合法。");
        }
        if !group_ids.insert(group.id.trim()) {
            bail!("模型分组 ID 重复：{}", group.id);
        }
        if group.api_key.trim().is_empty() || group.discovery.models.is_empty() {
            bail!("分组「{}」缺少有效 Key 或已选模型。", group.label);
        }
        for model in &group.discovery.models {
            if !model_ids.insert(model.id.as_str()) {
                bail!("模型 {} 被分配给多个 Key，请只保留一处。", model.id);
            }
        }
    }
    if !model_ids.contains(default_model.trim()) {
        bail!("默认模型必须是某个分组中已勾选的模型。");
    }
    Ok(())
}

fn combined_discovery(groups: &[MirrorAccessGroup], default_model: &str) -> MirrorModelDiscovery {
    MirrorModelDiscovery {
        models: groups
            .iter()
            .flat_map(|group| group.discovery.models.iter().cloned())
            .collect(),
        default_model: default_model.trim().to_string(),
    }
}

fn access_auth_key<'a>(groups: &'a [MirrorAccessGroup], default_model: &str) -> &'a str {
    groups
        .iter()
        .find(|group| {
            group
                .discovery
                .models
                .iter()
                .any(|model| model.id == default_model)
        })
        .unwrap_or(&groups[0])
        .api_key
        .trim()
}

pub fn restore_access(
    home: &Path,
    state_dir: &Path,
    settings_path: &Path,
) -> anyhow::Result<MirrorRestoreResult> {
    match restore_access_transaction(home, state_dir, settings_path, RestorePolicy::ManagedState) {
        Ok(result) => Ok(result),
        Err(error) => {
            let _ = record_restore_failure(state_dir, &error.to_string());
            Err(error)
        }
    }
}

/// Explicit recovery path for an unreadable managed state or a corrupted
/// managed config. The baseline is checksum-validated first, the current files
/// are snapshotted, and every write is rolled back if verification fails.
pub fn recover_access_from_baseline(
    home: &Path,
    state_dir: &Path,
    settings_path: &Path,
) -> anyhow::Result<MirrorRestoreResult> {
    restore_access_transaction(
        home,
        state_dir,
        settings_path,
        RestorePolicy::VerifiedBaseline,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RestorePolicy {
    ManagedState,
    VerifiedBaseline,
}

fn restore_access_transaction(
    home: &Path,
    state_dir: &Path,
    settings_path: &Path,
    policy: RestorePolicy,
) -> anyhow::Result<MirrorRestoreResult> {
    let state_path = state_dir.join(MANAGED_STATE_FILE);
    let old_state = read_optional(&state_path)
        .with_context(|| format!("无法读取接管状态 {}", state_path.display()))?;
    let managed_state = match policy {
        // Normal restore never guesses whether auth.json was managed by pure API.
        RestorePolicy::ManagedState => {
            parse_optional_managed_state(&state_path, old_state.as_deref())?
        }
        // The user explicitly selected baseline recovery. Managed keys are
        // restored from the verified baseline regardless of the damaged state.
        RestorePolicy::VerifiedBaseline => ManagedState::default(),
    };
    let baseline =
        load_baseline_manifest(state_dir).with_context(|| "没有找到可用的接管前 baseline")?;
    validate_baseline(state_dir, &baseline)?;
    let codex_home = validate_access_home_binding(home, &managed_state, &baseline, true)?;

    let config_path = home.join("config.toml");
    let auth_path = home.join("auth.json");
    let catalog_path = home.join(CATALOG_FILE);
    let old_config = read_optional(&config_path)?;
    let old_auth = read_optional(&auth_path)?;
    let old_settings = read_optional(settings_path)?;
    let old_catalog = read_optional(&catalog_path)?;
    let current_provider = read_provider(&config_path);
    let legacy_managed_provider =
        current_provider == MIRROR_PROVIDER_ID && baseline.original_provider != MIRROR_PROVIDER_ID;
    let restore_managed_files = policy == RestorePolicy::VerifiedBaseline
        || managed_state.active
        || legacy_managed_provider;
    let restored_config = if restore_managed_files {
        let baseline_config = baseline_optional_contents(state_dir, "config")?;
        match policy {
            RestorePolicy::ManagedState => {
                restore_managed_config_contents(old_config.as_deref(), baseline_config.as_deref())?
            }
            RestorePolicy::VerifiedBaseline => {
                recover_managed_config_contents(old_config.as_deref(), baseline_config.as_deref())
            }
        }
    } else {
        old_config.clone()
    };
    let restored_auth = if policy == RestorePolicy::VerifiedBaseline {
        let baseline_auth = baseline_optional_contents(state_dir, "auth")?;
        recover_managed_auth_contents(old_auth.as_deref(), baseline_auth.as_deref())
    } else if !restore_managed_files || managed_state.mode == Some(MirrorAccessMode::MixedApi) {
        old_auth.clone()
    } else {
        restore_managed_auth_contents(
            old_auth.as_deref(),
            baseline_optional_contents(state_dir, "auth")?.as_deref(),
        )?
    };
    let restored_settings = if restore_managed_files {
        let baseline_settings = baseline_optional_contents(state_dir, "manager-settings")?;
        match policy {
            RestorePolicy::ManagedState => restore_managed_settings_contents(
                old_settings.as_deref(),
                baseline_settings.as_deref(),
            )?,
            RestorePolicy::VerifiedBaseline => recover_managed_settings_contents(
                old_settings.as_deref(),
                baseline_settings.as_deref(),
            ),
        }
    } else {
        old_settings.clone()
    };
    let estimated_bytes = estimated_operation_bytes(home, state_dir, settings_path);
    ensure_storage_headroom(home, estimated_bytes, RECOVERY_FREE_SPACE_BYTES)?;
    ensure_storage_headroom(state_dir, estimated_bytes, RECOVERY_FREE_SPACE_BYTES)?;
    create_operation_snapshot(home, state_dir, settings_path, "pre-restore")?;

    let restore_result = (|| -> anyhow::Result<MirrorAccessStatus> {
        if restore_managed_files {
            restore_optional(&config_path, restored_config.as_deref())?;
            restore_optional(&auth_path, restored_auth.as_deref())?;
            restore_optional(settings_path, restored_settings.as_deref())?;
            restore_catalog_from_baseline(state_dir, &baseline, &catalog_path)?;
            verify_optional_file_contents(&config_path, restored_config.as_deref(), "config.toml")?;
            verify_optional_file_contents(&auth_path, restored_auth.as_deref(), "auth.json")?;
            verify_optional_file_contents(
                settings_path,
                restored_settings.as_deref(),
                "Manager settings.json",
            )?;

            let restored_provider = read_provider(&config_path);
            if restored_provider != baseline.original_provider {
                bail!(
                    "恢复校验失败：期望 provider {}，实际为 {}。",
                    baseline.original_provider,
                    restored_provider
                );
            }
        }

        let state = ManagedState {
            codex_home: Some(codex_home.clone()),
            active: false,
            mode: None,
            model_count: 0,
            default_model: String::new(),
            session_sync_status: "pending_restore".to_string(),
            last_message: "原始配置已恢复，等待会话归属恢复。".to_string(),
            ..ManagedState::default()
        };
        save_managed_state(state_dir, &state)?;
        let persisted =
            load_managed_state(state_dir).with_context(|| "恢复状态写入后无法重新读取")?;
        if persisted != state {
            bail!("恢复状态写入后的内容校验失败。");
        }
        try_access_status(home, state_dir).with_context(|| "恢复后的最终状态无法回读")
    })();

    let status = match restore_result {
        Ok(status) => status,
        Err(error) => {
            let rollback = [
                restore_optional(&config_path, old_config.as_deref()),
                restore_optional(&auth_path, old_auth.as_deref()),
                restore_optional(settings_path, old_settings.as_deref()),
                restore_optional(&catalog_path, old_catalog.as_deref()),
                restore_optional(&state_path, old_state.as_deref()),
            ];
            if let Some(rollback_error) = rollback.into_iter().find_map(Result::err) {
                return Err(error).context(format!(
                    "恢复未完成，且回滚本次恢复操作失败：{rollback_error}"
                ));
            }
            return Err(error).context("恢复未完成，已回滚到本次恢复前状态，可直接重试");
        }
    };

    Ok(MirrorRestoreResult {
        status,
        original_provider: baseline.original_provider,
    })
}

fn restore_managed_config_contents(
    current: Option<&[u8]>,
    baseline: Option<&[u8]>,
) -> anyhow::Result<Option<Vec<u8>>> {
    let mut current = parse_optional_toml(current, "当前 config.toml")?;
    let baseline = parse_optional_toml(baseline, "baseline config.toml")?;
    for key in [
        "model_provider",
        "model",
        "model_catalog_json",
        "cli_auth_credentials_store",
        "forced_login_method",
        "profile",
        "sandbox_mode",
        "approval_policy",
        "windows",
    ] {
        if let Some(item) = baseline.get(key) {
            current[key] = item.clone();
        } else {
            current.remove(key);
        }
    }

    let baseline_providers = match baseline.get("model_providers") {
        Some(item) => Some(
            item.as_table()
                .ok_or_else(|| anyhow::anyhow!("baseline config.toml 的 model_providers 不是表"))?,
        ),
        None => None,
    };
    let baseline_provider = baseline_providers
        .and_then(|providers| providers.get(MIRROR_PROVIDER_ID))
        .cloned();
    let baseline_compat_provider = baseline_providers
        .and_then(|providers| providers.get(MIRROR_CODEX_PROVIDER_ID))
        .cloned();
    if (baseline_provider.is_some() || baseline_compat_provider.is_some())
        && !current.contains_key("model_providers")
    {
        current["model_providers"] = Item::Table(Table::new());
    }
    if let Some(item) = current.get_mut("model_providers") {
        let providers = item
            .as_table_mut()
            .ok_or_else(|| anyhow::anyhow!("当前 config.toml 的 model_providers 不是表"))?;
        if let Some(provider) = baseline_provider {
            providers.insert(MIRROR_PROVIDER_ID, provider);
        } else {
            providers.remove(MIRROR_PROVIDER_ID);
        }
        if let Some(provider) = baseline_compat_provider {
            providers.insert(MIRROR_CODEX_PROVIDER_ID, provider);
        } else {
            providers.remove(MIRROR_CODEX_PROVIDER_ID);
        }
        if providers.is_empty() && baseline.get("model_providers").is_none() {
            current.remove("model_providers");
        }
    }

    if current.as_table().is_empty() {
        return Ok(None);
    }
    let mut output = current.to_string();
    if !output.ends_with('\n') {
        output.push('\n');
    }
    Ok(Some(output.into_bytes()))
}

fn recover_managed_config_contents(
    current: Option<&[u8]>,
    baseline: Option<&[u8]>,
) -> Option<Vec<u8>> {
    restore_managed_config_contents(current, baseline)
        .unwrap_or_else(|_| baseline.map(ToOwned::to_owned))
}

fn parse_optional_toml(contents: Option<&[u8]>, label: &str) -> anyhow::Result<DocumentMut> {
    let text = contents
        .map(String::from_utf8_lossy)
        .map(|value| value.into_owned())
        .unwrap_or_default();
    if text.trim().is_empty() {
        Ok(DocumentMut::new())
    } else {
        text.parse::<DocumentMut>()
            .with_context(|| format!("{label} 无法解析，未执行恢复"))
    }
}

fn restore_managed_auth_contents(
    current: Option<&[u8]>,
    baseline: Option<&[u8]>,
) -> anyhow::Result<Option<Vec<u8>>> {
    let current = parse_optional_json_object(current, "当前 auth.json")?;
    let mut restored = parse_optional_json_object(baseline, "baseline auth.json")?;
    for (key, value) in current {
        if key != "OPENAI_API_KEY" {
            restored.insert(key, value);
        }
    }
    if restored.is_empty() {
        Ok(None)
    } else {
        Ok(Some(serde_json::to_vec_pretty(&Value::Object(restored))?))
    }
}

fn recover_managed_auth_contents(
    current: Option<&[u8]>,
    baseline: Option<&[u8]>,
) -> Option<Vec<u8>> {
    restore_managed_auth_contents(current, baseline)
        .unwrap_or_else(|_| baseline.map(ToOwned::to_owned))
}

fn restore_managed_settings_contents(
    current: Option<&[u8]>,
    baseline: Option<&[u8]>,
) -> anyhow::Result<Option<Vec<u8>>> {
    let baseline_absent = baseline.is_none();
    let mut current = parse_optional_json_object(current, "当前 Manager settings.json")?;
    let baseline = parse_optional_json_object(baseline, "baseline Manager settings.json")?;
    for key in [
        "providerSyncEnabled",
        "relayProfilesEnabled",
        "enhancementsEnabled",
        "codexAppPluginMarketplaceUnlock",
        "codexAppModelWhitelistUnlock",
        "launchMode",
        "activeRelayId",
        "activeAggregateRelayId",
    ] {
        if let Some(value) = baseline.get(key) {
            current.insert(key.to_string(), value.clone());
        } else {
            current.remove(key);
        }
    }
    restore_managed_settings_array(&mut current, &baseline, "relayProfiles", |id| {
        id == MIRROR_PROVIDER_ID || id.starts_with("mirrorplus-")
    })?;
    restore_managed_settings_array(&mut current, &baseline, "aggregateRelayProfiles", |id| {
        id == MIRROR_PROVIDER_ID
    })?;

    if baseline_absent {
        let mut defaults = serde_json::to_value(BackendSettings::default())?
            .as_object()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Manager 默认设置无法序列化为 JSON object"))?;
        for key in [
            "providerSyncEnabled",
            "relayProfilesEnabled",
            "enhancementsEnabled",
            "codexAppPluginMarketplaceUnlock",
            "codexAppModelWhitelistUnlock",
            "launchMode",
            "activeRelayId",
            "activeAggregateRelayId",
        ] {
            defaults.remove(key);
        }
        restore_managed_settings_array(&mut defaults, &Map::new(), "relayProfiles", |id| {
            id == MIRROR_PROVIDER_ID || id.starts_with("mirrorplus-")
        })?;
        restore_managed_settings_array(
            &mut defaults,
            &Map::new(),
            "aggregateRelayProfiles",
            |id| id == MIRROR_PROVIDER_ID,
        )?;
        if current == defaults {
            return Ok(None);
        }
    }

    if current.is_empty() {
        Ok(None)
    } else {
        Ok(Some(serde_json::to_vec_pretty(&Value::Object(current))?))
    }
}

fn recover_managed_settings_contents(
    current: Option<&[u8]>,
    baseline: Option<&[u8]>,
) -> Option<Vec<u8>> {
    restore_managed_settings_contents(current, baseline)
        .unwrap_or_else(|_| baseline.map(ToOwned::to_owned))
}

fn restore_managed_settings_array(
    current: &mut Map<String, Value>,
    baseline: &Map<String, Value>,
    key: &str,
    is_managed: impl Fn(&str) -> bool,
) -> anyhow::Result<()> {
    let current_had_key = current.contains_key(key);
    let current_items = match current.remove(key) {
        Some(Value::Array(items)) => items,
        Some(_) => bail!("当前 Manager settings.json 的 {key} 不是数组"),
        None => Vec::new(),
    };
    let baseline_items = match baseline.get(key) {
        Some(Value::Array(items)) => items.as_slice(),
        Some(_) => bail!("baseline Manager settings.json 的 {key} 不是数组"),
        None => &[],
    };
    let mut restored = current_items
        .into_iter()
        .filter(|item| !settings_profile_is_managed(item, &is_managed))
        .collect::<Vec<_>>();
    restored.extend(
        baseline_items
            .iter()
            .filter(|item| settings_profile_is_managed(item, &is_managed))
            .cloned(),
    );
    if !restored.is_empty() || current_had_key || baseline.contains_key(key) {
        current.insert(key.to_string(), Value::Array(restored));
    }
    Ok(())
}

fn settings_profile_is_managed(item: &Value, is_managed: &impl Fn(&str) -> bool) -> bool {
    item.get("id")
        .and_then(Value::as_str)
        .is_some_and(is_managed)
}

fn parse_optional_json_object(
    contents: Option<&[u8]>,
    label: &str,
) -> anyhow::Result<Map<String, Value>> {
    let Some(contents) = contents else {
        return Ok(Map::new());
    };
    let value: Value = serde_json::from_slice(contents)
        .with_context(|| format!("{label} 无法解析，未执行恢复"))?;
    match value {
        Value::Object(object) => Ok(object),
        _ => bail!("{label} 的根节点不是 JSON object，未执行恢复"),
    }
}

fn record_restore_failure(state_dir: &Path, message: &str) -> anyhow::Result<()> {
    let mut state = load_managed_state_or_default(state_dir)?;
    state.session_sync_status = "restore_failed".to_string();
    state.last_message = if message.starts_with("恢复失败") {
        message.to_string()
    } else {
        format!("恢复失败：{message}")
    };
    state.last_operation_at_ms = now_ms();
    save_managed_state(state_dir, &state)
}

pub fn record_session_sync(
    home: &Path,
    state_dir: &Path,
    synced: bool,
    message: &str,
) -> anyhow::Result<MirrorAccessStatus> {
    let mut state = load_managed_state_or_default(state_dir)?;
    let baseline = load_valid_baseline_if_present(state_dir)?;
    let codex_home = match baseline.as_ref() {
        Some(baseline) => validate_access_home_binding(home, &state, baseline, true)?,
        None => validate_state_home_binding(home, &state)?,
    };
    state.codex_home = Some(codex_home);
    let restoring = !state.active
        && matches!(
            state.session_sync_status.as_str(),
            "pending_restore" | "restore_failed"
        );
    state.session_sync_status = if synced {
        "synced"
    } else if restoring {
        "restore_failed"
    } else {
        "degraded"
    }
    .to_string();
    state.last_message = if !synced && restoring && !message.starts_with("恢复失败") {
        format!("恢复失败：{message}")
    } else {
        message.to_string()
    };
    state.last_operation_at_ms = now_ms();
    save_managed_state(state_dir, &state)?;
    try_access_status(home, state_dir)
}

fn build_managed_config(
    existing: Option<&[u8]>,
    api_key: &str,
    base_url: &str,
    mode: MirrorAccessMode,
    default_model: &str,
    catalog_path: &Path,
) -> anyhow::Result<String> {
    let text = existing
        .map(String::from_utf8_lossy)
        .map(|value| value.into_owned())
        .unwrap_or_default();
    let mut doc = if text.trim().is_empty() {
        DocumentMut::new()
    } else {
        text.parse::<DocumentMut>()
            .with_context(|| "现有 config.toml 无法解析，未执行接管")?
    };
    doc["model_provider"] = value(MIRROR_CODEX_PROVIDER_ID);
    doc["model"] = value(default_model);
    doc["model_catalog_json"] = value(catalog_path.to_string_lossy().to_string());
    // Use Codex's native top-level access controls. The Windows sandbox table
    // triggers the desktop setup workflow and must not be part of normal Mirror
    // startup; restore puts the user's original values back from the baseline.
    doc["sandbox_mode"] = value("danger-full-access");
    doc["approval_policy"] = value("never");
    let remove_empty_windows = if let Some(windows) = doc.get_mut("windows") {
        let windows = windows
            .as_table_mut()
            .ok_or_else(|| anyhow::anyhow!("config.toml 的 windows 不是合法表。"))?;
        windows.remove("sandbox");
        windows.is_empty()
    } else {
        false
    };
    if remove_empty_windows {
        doc.as_table_mut().remove("windows");
    }
    // A previously selected Codex profile can override the root provider and model.
    // Keep the saved [profiles.*] definitions, but deactivate the selection while
    // Mirror manages the active runtime; restore puts the original selector back.
    doc.as_table_mut().remove("profile");
    if mode == MirrorAccessMode::PureApi {
        // Make auth.json authoritative even when Codex would otherwise select
        // the Windows credential store. Both keys are restored from baseline
        // when leaving pure API mode.
        doc["cli_auth_credentials_store"] = value("file");
        doc["forced_login_method"] = value("api");
    }
    if !doc.contains_key("model_providers") {
        doc["model_providers"] = Item::Table(Table::new());
    }
    let providers = doc["model_providers"]
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("config.toml 的 model_providers 不是合法表。"))?;
    let mut provider = Table::new();
    provider.insert("name", value("mirror x codex"));
    provider.insert("base_url", value(base_url));
    provider.insert("wire_api", value("responses"));
    provider.insert("experimental_bearer_token", value(api_key));
    if mode == MirrorAccessMode::MixedApi {
        provider.insert("requires_openai_auth", value(true));
    }
    // New CodexPlusPlus requests use `custom`, while older Mirror builds and
    // persisted threads can still carry `mirrorplus`. Keep both ids resolvable
    // without relying on renderer injection during startup or history resume.
    providers.insert(MIRROR_PROVIDER_ID, Item::Table(provider.clone()));
    providers.insert(MIRROR_CODEX_PROVIDER_ID, Item::Table(provider));
    let mut output = doc.to_string();
    if !output.ends_with('\n') {
        output.push('\n');
    }
    Ok(output)
}

fn managed_config_seed_for_mode(
    state_dir: &Path,
    _managed_state: &ManagedState,
    mode: MirrorAccessMode,
    current_config: Option<&[u8]>,
) -> anyhow::Result<Option<Vec<u8>>> {
    if mode != MirrorAccessMode::MixedApi {
        return Ok(current_config.map(ToOwned::to_owned));
    }
    let baseline_config = baseline_optional_contents(state_dir, "config")?;
    restore_managed_auth_config_keys(current_config, baseline_config.as_deref())
}

fn managed_auth_config_keys_match(
    current_config: Option<&[u8]>,
    expected_config: Option<&[u8]>,
) -> anyhow::Result<bool> {
    let current = parse_optional_toml(current_config, "当前 config.toml")?;
    let expected = parse_optional_toml(expected_config, "预期 config.toml")?;
    for key in ["cli_auth_credentials_store", "forced_login_method"] {
        let Some(expected_item) = expected.get(key) else {
            if current.get(key).is_some() {
                return Ok(false);
            }
            continue;
        };
        let expected_value = expected_item
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("baseline config.toml 的 {key} 不是字符串"))?;
        if current.get(key).and_then(Item::as_str) != Some(expected_value) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn restore_managed_auth_config_keys(
    current_config: Option<&[u8]>,
    baseline_config: Option<&[u8]>,
) -> anyhow::Result<Option<Vec<u8>>> {
    let current = current_config
        .map(String::from_utf8_lossy)
        .map(|value| value.into_owned())
        .unwrap_or_default();
    let mut current_doc = if current.trim().is_empty() {
        DocumentMut::new()
    } else {
        current
            .parse::<DocumentMut>()
            .with_context(|| "当前 config.toml 无法恢复登录存储设置")?
    };
    let baseline = baseline_config
        .map(String::from_utf8_lossy)
        .map(|value| value.into_owned())
        .unwrap_or_default();
    let baseline_doc = if baseline.trim().is_empty() {
        DocumentMut::new()
    } else {
        baseline
            .parse::<DocumentMut>()
            .with_context(|| "baseline config.toml 无法恢复登录存储设置")?
    };
    for key in ["cli_auth_credentials_store", "forced_login_method"] {
        if let Some(item) = baseline_doc.get(key) {
            current_doc[key] = item.clone();
        } else {
            current_doc.remove(key);
        }
    }
    let mut output = current_doc.to_string();
    if !output.is_empty() && !output.ends_with('\n') {
        output.push('\n');
    }
    Ok((!output.is_empty()).then(|| output.into_bytes()))
}

fn build_pure_api_auth(existing: Option<&[u8]>, api_key: &str) -> anyhow::Result<Vec<u8>> {
    let _ = existing;
    let mut object = Map::new();
    object.insert(
        "OPENAI_API_KEY".to_string(),
        Value::String(api_key.to_string()),
    );
    Ok(serde_json::to_vec_pretty(&Value::Object(object))?)
}

fn managed_auth_for_mode(
    state_dir: &Path,
    managed_state: &ManagedState,
    mode: MirrorAccessMode,
    current_auth: Option<&[u8]>,
    api_key: &str,
) -> anyhow::Result<Option<Vec<u8>>> {
    match mode {
        MirrorAccessMode::PureApi => Ok(Some(build_pure_api_auth(current_auth, api_key)?)),
        MirrorAccessMode::MixedApi => {
            let switching_from_pure =
                managed_state.active && managed_state.mode == Some(MirrorAccessMode::PureApi);
            if switching_from_pure {
                baseline_optional_contents(state_dir, "auth")
            } else {
                Ok(current_auth.map(ToOwned::to_owned))
            }
        }
    }
}

fn baseline_optional_contents(state_dir: &Path, id: &str) -> anyhow::Result<Option<Vec<u8>>> {
    let manifest = load_baseline_manifest(state_dir)?;
    validate_baseline(state_dir, &manifest)?;
    let file = manifest
        .files
        .iter()
        .find(|file| file.id == id)
        .ok_or_else(|| anyhow::anyhow!("baseline manifest 缺少 {id}"))?;
    if file.existed {
        Ok(Some(fs::read(
            state_dir.join(BASELINE_DIR).join(backup_name(id)?),
        )?))
    } else {
        Ok(None)
    }
}

fn build_catalog(models: &[MirrorModel]) -> String {
    let entries = models
        .iter()
        .map(|model| ModelCatalogEntry {
            slug: model.id.clone(),
            display_name: model.display_name.clone(),
            suffix_window: model.context_window,
        })
        .collect::<Vec<_>>();
    build_model_catalog_json_with_capabilities(&entries, None, None, Some(false))
}

fn save_managed_settings(
    settings_path: &Path,
    mode: MirrorAccessMode,
    groups: &[MirrorAccessGroup],
    discovery: &MirrorModelDiscovery,
    config: &str,
    auth: Option<&[u8]>,
) -> anyhow::Result<()> {
    let store = SettingsStore::new(settings_path.to_path_buf());
    let mut settings = load_manager_settings_strict(settings_path)?;
    let auth_contents = auth
        .map(String::from_utf8_lossy)
        .map(|value| value.into_owned())
        .unwrap_or_default();
    let grouped = groups.len() > 1;
    let provider_key = if grouped {
        "codex-plus-aggregate"
    } else {
        groups[0].api_key.trim()
    };
    let profile = RelayProfile {
        id: MIRROR_PROVIDER_ID.to_string(),
        name: "mirror x codex".to_string(),
        model: discovery.default_model.clone(),
        base_url: if grouped {
            crate::protocol_proxy::local_responses_proxy_base_url(
                crate::protocol_proxy::DEFAULT_PROTOCOL_PROXY_PORT,
            )
        } else {
            MIRROR_BASE_URL.to_string()
        },
        upstream_base_url: MIRROR_BASE_URL.to_string(),
        api_key: provider_key.to_string(),
        protocol: RelayProtocol::Responses,
        relay_mode: if grouped {
            RelayMode::Aggregate
        } else {
            match mode {
                MirrorAccessMode::MixedApi => RelayMode::MixedApi,
                MirrorAccessMode::PureApi => RelayMode::PureApi,
            }
        },
        official_mix_api_key: mode == MirrorAccessMode::MixedApi,
        test_model: discovery.default_model.clone(),
        config_contents: config.to_string(),
        auth_contents,
        model_list: discovery
            .models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        model_windows: serde_json::to_string(
            &discovery
                .models
                .iter()
                .filter_map(|model| Some((model.id.clone(), model.context_window?.to_string())))
                .collect::<std::collections::HashMap<_, _>>(),
        )?,
        ..RelayProfile::default()
    };
    let member_profiles = groups
        .iter()
        .map(build_member_relay_profile)
        .collect::<anyhow::Result<Vec<_>>>()?;
    settings.relay_profiles_enabled = true;
    settings.enhancements_enabled = true;
    // Session repair is run explicitly by the access workflow. Keeping the legacy
    // launch-time switch enabled would rescan and back up every rollout on each start,
    // which is especially harmful for large histories or low-disk machines.
    settings.provider_sync_enabled = false;
    settings.codex_app_plugin_marketplace_unlock = true;
    settings.codex_app_model_whitelist_unlock = true;
    settings
        .relay_profiles
        .retain(|existing| !is_managed_relay_profile(existing));
    settings
        .relay_profiles
        .extend(member_profiles.iter().cloned());
    settings.relay_profiles.push(profile);
    settings.active_relay_id = MIRROR_PROVIDER_ID.to_string();
    settings
        .aggregate_relay_profiles
        .retain(|existing| existing.id != MIRROR_PROVIDER_ID);
    if grouped {
        settings
            .aggregate_relay_profiles
            .push(AggregateRelayProfile {
                id: MIRROR_PROVIDER_ID.to_string(),
                name: "mirror x codex 分组路由".to_string(),
                strategy: AggregateRelayStrategy::Failover,
                members: member_profiles
                    .iter()
                    .map(|profile| AggregateRelayMember {
                        relay_id: profile.id.clone(),
                        weight: 1,
                    })
                    .collect(),
            });
    }
    settings.active_aggregate_relay_id = if grouped {
        MIRROR_PROVIDER_ID.to_string()
    } else {
        String::new()
    };
    settings.launch_mode = if mode == MirrorAccessMode::PureApi {
        LaunchMode::Patch
    } else {
        LaunchMode::Relay
    };
    store.update(serde_json::to_value(&settings)?).map(|_| ())
}

pub fn probe_profile_for_group(group: &MirrorAccessGroup) -> anyhow::Result<MirrorProbeProfile> {
    Ok(MirrorProbeProfile {
        label: group.label.clone(),
        model: group.discovery.default_model.clone(),
        profile: build_member_relay_profile(group)?,
    })
}

fn build_member_relay_profile(group: &MirrorAccessGroup) -> anyhow::Result<RelayProfile> {
    Ok(RelayProfile {
        id: format!("mirrorplus-{}", group.id.trim()),
        name: group.label.clone(),
        model: group.discovery.default_model.clone(),
        base_url: MIRROR_BASE_URL.to_string(),
        upstream_base_url: MIRROR_BASE_URL.to_string(),
        api_key: group.api_key.trim().to_string(),
        protocol: RelayProtocol::Responses,
        relay_mode: RelayMode::MixedApi,
        test_model: group.discovery.default_model.clone(),
        config_contents: build_group_profile_config(
            group.id.trim(),
            group.api_key.trim(),
            &group.discovery.default_model,
        )?,
        model_list: group
            .discovery
            .models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        model_windows: serde_json::to_string(
            &group
                .discovery
                .models
                .iter()
                .filter_map(|model| Some((model.id.clone(), model.context_window?.to_string())))
                .collect::<std::collections::HashMap<_, _>>(),
        )?,
        ..RelayProfile::default()
    })
}

fn load_manager_settings_strict(settings_path: &Path) -> anyhow::Result<BackendSettings> {
    let bytes = match fs::read(settings_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(BackendSettings::default()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("无法读取 Manager 设置 {}", settings_path.display()));
        }
    };
    serde_json::from_slice(&bytes)
        .with_context(|| format!("Manager 设置 {} 不是有效 JSON", settings_path.display()))
}

fn verify_optional_file_contents(
    path: &Path,
    expected: Option<&[u8]>,
    label: &str,
) -> anyhow::Result<()> {
    let actual = read_optional(path).with_context(|| format!("接管后的 {label} 无法回读"))?;
    if actual.as_deref() != expected {
        bail!("接管后的 {label} 内容校验失败。");
    }
    Ok(())
}

fn verify_managed_settings(
    settings_path: &Path,
    mode: MirrorAccessMode,
    groups: &[MirrorAccessGroup],
    discovery: &MirrorModelDiscovery,
) -> anyhow::Result<Vec<MirrorProbeProfile>> {
    let settings = load_manager_settings_strict(settings_path)
        .with_context(|| "接管后的 Manager 设置无法回读")?;
    if !settings.relay_profiles_enabled
        || !settings.enhancements_enabled
        || settings.active_relay_id != MIRROR_PROVIDER_ID
    {
        bail!("接管后的 Manager 总开关或 active relay 校验失败。");
    }

    let grouped = groups.len() > 1;
    let main = settings
        .relay_profiles
        .iter()
        .find(|profile| profile.id == MIRROR_PROVIDER_ID)
        .ok_or_else(|| anyhow::anyhow!("接管后的 mirrorplus 主 profile 缺失。"))?;
    let expected_main_mode = if grouped {
        RelayMode::Aggregate
    } else {
        match mode {
            MirrorAccessMode::MixedApi => RelayMode::MixedApi,
            MirrorAccessMode::PureApi => RelayMode::PureApi,
        }
    };
    let expected_main_key = if grouped {
        "codex-plus-aggregate"
    } else {
        groups[0].api_key.trim()
    };
    let expected_main_base_url = if grouped {
        crate::protocol_proxy::local_responses_proxy_base_url(
            crate::protocol_proxy::DEFAULT_PROTOCOL_PROXY_PORT,
        )
    } else {
        MIRROR_BASE_URL.to_string()
    };
    if main.protocol != RelayProtocol::Responses
        || main.relay_mode != expected_main_mode
        || crate::relay_config::relay_profile_model(main) != discovery.default_model
        || crate::relay_config::relay_profile_base_url(main) != expected_main_base_url
        || crate::relay_config::relay_profile_api_key(main) != expected_main_key
    {
        bail!("接管后的 mirrorplus 主 profile 字段校验失败。");
    }

    let expected_model_ids = discovery
        .models
        .iter()
        .map(|model| model.id.as_str())
        .collect::<HashSet<_>>();
    let stored_model_ids = main
        .model_list
        .lines()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .collect::<HashSet<_>>();
    if stored_model_ids != expected_model_ids {
        bail!("接管后的 mirrorplus 主 profile 模型列表校验失败。");
    }

    if grouped {
        let aggregate = settings
            .aggregate_relay_profiles
            .iter()
            .find(|profile| profile.id == MIRROR_PROVIDER_ID)
            .filter(|_| settings.active_aggregate_relay_id == MIRROR_PROVIDER_ID)
            .ok_or_else(|| anyhow::anyhow!("接管后的分组路由未成为 active aggregate。"))?;
        let expected_members = groups
            .iter()
            .map(|group| format!("mirrorplus-{}", group.id.trim()))
            .collect::<HashSet<_>>();
        let actual_members = aggregate
            .members
            .iter()
            .map(|member| member.relay_id.clone())
            .collect::<HashSet<_>>();
        if actual_members != expected_members {
            bail!("接管后的分组路由成员校验失败。");
        }
    } else if !settings.active_aggregate_relay_id.trim().is_empty() {
        bail!("单 Key 接管后仍残留 active aggregate。");
    }

    let mut probes = Vec::with_capacity(groups.len());
    for group in groups {
        let profile_id = format!("mirrorplus-{}", group.id.trim());
        let profile = settings
            .relay_profiles
            .iter()
            .find(|profile| profile.id == profile_id)
            .ok_or_else(|| anyhow::anyhow!("接管后的分组 profile {} 缺失。", profile_id))?;
        if profile.protocol != RelayProtocol::Responses
            || crate::relay_config::relay_profile_base_url(profile) != MIRROR_BASE_URL
            || crate::relay_config::relay_profile_api_key(profile) != group.api_key.trim()
            || crate::relay_config::relay_profile_model(profile) != group.discovery.default_model
        {
            bail!("接管后的分组「{}」profile 字段校验失败。", group.label);
        }
        let expected = group
            .discovery
            .models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<HashSet<_>>();
        let actual = profile
            .model_list
            .lines()
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .collect::<HashSet<_>>();
        if actual != expected {
            bail!("接管后的分组「{}」模型列表校验失败。", group.label);
        }
        probes.push(MirrorProbeProfile {
            label: group.label.clone(),
            model: group.discovery.default_model.clone(),
            profile: profile.clone(),
        });
    }
    Ok(probes)
}

fn merge_existing_access_groups(
    settings_path: &Path,
    groups: Vec<MirrorAccessGroup>,
) -> anyhow::Result<Vec<MirrorAccessGroup>> {
    let incoming_ids = groups
        .iter()
        .map(|group| group.id.trim().to_string())
        .collect::<HashSet<_>>();
    let mut merged = existing_access_groups(settings_path)?
        .into_iter()
        .filter(|group| group.id.trim() != "claude")
        .filter(|group| !incoming_ids.contains(group.id.trim()))
        .collect::<Vec<_>>();
    merged.extend(groups);
    Ok(merged)
}

fn validate_existing_manager_settings(settings_path: &Path) -> anyhow::Result<()> {
    load_manager_settings_strict(settings_path)
        .map(|_| ())
        .with_context(|| "现有 Manager 设置不是有效 JSON 或无法读取，未执行接管")
}

fn existing_access_groups(settings_path: &Path) -> anyhow::Result<Vec<MirrorAccessGroup>> {
    let settings = SettingsStore::new(settings_path.to_path_buf()).load()?;
    let mut groups = settings
        .relay_profiles
        .iter()
        .filter_map(|profile| {
            let group_id = profile.id.strip_prefix("mirrorplus-")?;
            profile_to_access_group(profile, group_id)
        })
        .collect::<Vec<_>>();

    if groups.is_empty() {
        if let Some(profile) = settings.relay_profiles.iter().find(|profile| {
            profile.id == MIRROR_PROVIDER_ID && profile.relay_mode != RelayMode::Aggregate
        }) {
            let group_id = infer_legacy_group_id(profile);
            if let Some(group) = profile_to_access_group(profile, group_id) {
                groups.push(group);
            }
        }
    }
    Ok(groups)
}

fn profile_to_access_group(profile: &RelayProfile, group_id: &str) -> Option<MirrorAccessGroup> {
    let api_key = crate::relay_config::relay_profile_api_key(profile);
    if api_key.trim().is_empty() || api_key == "codex-plus-aggregate" {
        return None;
    }
    let windows =
        serde_json::from_str::<HashMap<String, String>>(&profile.model_windows).unwrap_or_default();
    let mut seen = HashSet::new();
    let models = profile
        .model_list
        .split(['\r', '\n', ','])
        .map(str::trim)
        .filter(|model| !model.is_empty() && seen.insert((*model).to_string()))
        .map(|model| MirrorModel {
            id: model.to_string(),
            display_name: model.to_string(),
            context_window: windows
                .get(model)
                .and_then(|value| value.parse::<u64>().ok()),
            context_source: "stored".to_string(),
        })
        .collect::<Vec<_>>();
    if models.is_empty() {
        return None;
    }
    let configured_default = crate::relay_config::relay_profile_model(profile);
    let default_model = if models.iter().any(|model| model.id == configured_default) {
        configured_default
    } else {
        models[0].id.clone()
    };
    Some(MirrorAccessGroup {
        id: group_id.to_string(),
        label: profile.name.clone(),
        api_key,
        discovery: MirrorModelDiscovery {
            models,
            default_model,
        },
    })
}

fn infer_legacy_group_id(profile: &RelayProfile) -> &'static str {
    if profile
        .model_list
        .lines()
        .any(|model| model.trim().to_ascii_lowercase().contains("claude"))
    {
        "claude"
    } else {
        "codexpro"
    }
}

fn is_managed_relay_profile(profile: &RelayProfile) -> bool {
    profile.id == MIRROR_PROVIDER_ID || profile.id.starts_with("mirrorplus-")
}

fn build_group_profile_config(
    group_id: &str,
    api_key: &str,
    default_model: &str,
) -> anyhow::Result<String> {
    let provider_id = format!("mirrorplus-{group_id}");
    let mut doc = DocumentMut::new();
    doc["model_provider"] = value(provider_id.as_str());
    doc["model"] = value(default_model);
    doc["model_providers"] = Item::Table(Table::new());
    let mut provider = Table::new();
    provider.insert("name", value(provider_id.as_str()));
    provider.insert("base_url", value(MIRROR_BASE_URL));
    provider.insert("wire_api", value("responses"));
    provider.insert("requires_openai_auth", value(true));
    provider.insert("experimental_bearer_token", value(api_key));
    doc["model_providers"]
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("无法创建分组 provider 配置"))?
        .insert(provider_id.as_str(), Item::Table(provider));
    Ok(doc.to_string())
}

fn verify_managed_config(
    path: &Path,
    catalog_path: &Path,
    provider_base_url: &str,
    provider_key: &str,
    mode: MirrorAccessMode,
    discovery: &MirrorModelDiscovery,
) -> anyhow::Result<()> {
    let text = fs::read_to_string(path)?;
    let doc = text.parse::<DocumentMut>()?;
    let provider = doc
        .get("model_provider")
        .and_then(Item::as_str)
        .unwrap_or_default();
    let model = doc.get("model").and_then(Item::as_str).unwrap_or_default();
    if provider != MIRROR_CODEX_PROVIDER_ID
        || model != discovery.default_model
        || doc.get("profile").is_some()
    {
        bail!("接管后的 config.toml 校验失败。");
    }
    let expected_catalog_path = catalog_path.to_string_lossy();
    if doc.get("model_catalog_json").and_then(Item::as_str) != Some(expected_catalog_path.as_ref())
    {
        bail!("接管后的模型目录指针校验失败。");
    }
    let providers = doc
        .get("model_providers")
        .and_then(Item::as_table)
        .ok_or_else(|| anyhow::anyhow!("接管后的 model_providers 表缺失。"))?;
    let login_storage_valid = mode != MirrorAccessMode::PureApi
        || (doc.get("cli_auth_credentials_store").and_then(Item::as_str) == Some("file")
            && doc.get("forced_login_method").and_then(Item::as_str) == Some("api"));
    for provider_id in [MIRROR_CODEX_PROVIDER_ID, MIRROR_PROVIDER_ID] {
        let table = providers
            .get(provider_id)
            .and_then(Item::as_table)
            .ok_or_else(|| anyhow::anyhow!("接管后的 {provider_id} provider 缺失。"))?;
        let auth_fields_valid = match mode {
            MirrorAccessMode::MixedApi => {
                table.get("requires_openai_auth").and_then(Item::as_bool) == Some(true)
                    && table
                        .get("experimental_bearer_token")
                        .and_then(Item::as_str)
                        == Some(provider_key)
            }
            MirrorAccessMode::PureApi => {
                !table.contains_key("requires_openai_auth")
                    && table
                        .get("experimental_bearer_token")
                        .and_then(Item::as_str)
                        == Some(provider_key)
            }
        };
        if table.get("base_url").and_then(Item::as_str) != Some(provider_base_url)
            || table.get("wire_api").and_then(Item::as_str) != Some("responses")
            || !auth_fields_valid
        {
            bail!("接管后的 {provider_id} provider 配置不完整。");
        }
    }
    if !login_storage_valid {
        bail!("接管后的 Pure API 登录存储配置不完整。");
    }

    let catalog: Value = serde_json::from_slice(&fs::read(catalog_path)?)
        .with_context(|| "接管后的模型目录 JSON 无法解析")?;
    let actual_models = catalog
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("接管后的模型目录缺少 models 数组。"))?
        .iter()
        .filter_map(|model| model.get("slug").and_then(Value::as_str))
        .collect::<Vec<_>>();
    let expected_models = discovery
        .models
        .iter()
        .map(|model| model.id.as_str())
        .collect::<Vec<_>>();
    if actual_models != expected_models {
        bail!("接管后的模型目录与勾选模型不一致。");
    }
    Ok(())
}

fn ensure_baseline(home: &Path, state_dir: &Path, settings_path: &Path) -> anyhow::Result<()> {
    let baseline_dir = state_dir.join(BASELINE_DIR);
    if baseline_dir.join("manifest.json").exists() {
        return validate_baseline(state_dir, &load_baseline_manifest(state_dir)?);
    }
    if baseline_dir.exists() {
        bail!("baseline 目录存在但不完整，请先人工检查后再接管。");
    }
    let staging = state_dir.join(format!("{BASELINE_DIR}.staging-{}", uuid::Uuid::new_v4()));
    let manifest = match create_baseline_staging(home, settings_path, &staging) {
        Ok(manifest) => manifest,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
    };
    fs::rename(&staging, &baseline_dir).with_context(|| "无法提交 baseline")?;
    validate_baseline(state_dir, &manifest)
}

fn baseline_needs_refresh_for_new_cycle(state: &ManagedState) -> bool {
    !state.active && matches!(state.session_sync_status.as_str(), "not_run" | "synced")
}

fn refresh_baseline(home: &Path, state_dir: &Path, settings_path: &Path) -> anyhow::Result<()> {
    let baseline_dir = state_dir.join(BASELINE_DIR);
    if !baseline_dir.exists() {
        return ensure_baseline(home, state_dir, settings_path);
    }
    validate_baseline(state_dir, &load_baseline_manifest(state_dir)?)?;

    let transaction_id = uuid::Uuid::new_v4();
    let staging = state_dir.join(format!("{BASELINE_DIR}.staging-{transaction_id}"));
    let previous = state_dir.join(format!("{BASELINE_DIR}.previous-{transaction_id}"));
    let manifest = match create_baseline_staging(home, settings_path, &staging) {
        Ok(manifest) => manifest,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(error).context("无法为新一轮接入创建 baseline");
        }
    };

    fs::rename(&baseline_dir, &previous).with_context(|| "无法暂存上一轮 baseline")?;
    if let Err(error) = fs::rename(&staging, &baseline_dir) {
        let restore = fs::rename(&previous, &baseline_dir);
        let _ = fs::remove_dir_all(&staging);
        return match restore {
            Ok(()) => Err(error).context("无法提交新一轮 baseline，已保留上一轮 baseline"),
            Err(restore_error) => Err(error).context(format!(
                "无法提交新一轮 baseline，且上一轮 baseline 无法自动放回：{restore_error}"
            )),
        };
    }

    if let Err(error) = validate_baseline(state_dir, &manifest) {
        let failed = state_dir.join(format!("{BASELINE_DIR}.failed-{transaction_id}"));
        let move_failed = fs::rename(&baseline_dir, &failed);
        let restore = fs::rename(&previous, &baseline_dir);
        return match (move_failed, restore) {
            (Ok(()), Ok(())) => Err(error).context(
                "新一轮 baseline 校验失败，已恢复上一轮 baseline；失败副本已保留",
            ),
            (move_result, restore_result) => Err(error).context(format!(
                "新一轮 baseline 校验失败且自动恢复不完整：隔离失败副本={move_result:?}；恢复上一轮={restore_result:?}"
            )),
        };
    }

    if let Err(error) = fs::remove_dir_all(&previous) {
        let _ = crate::diagnostic_log::append_diagnostic_log(
            "mirror_access.previous_baseline_cleanup_failed",
            serde_json::json!({
                "path": previous,
                "error": error.to_string(),
            }),
        );
    }
    Ok(())
}

fn create_baseline_staging(
    home: &Path,
    settings_path: &Path,
    staging: &Path,
) -> anyhow::Result<BaselineManifest> {
    fs::create_dir_all(staging)?;
    let config_path = home.join("config.toml");
    let auth_path = home.join("auth.json");
    let files = [
        snapshot_file("config", &config_path, &staging.join("config.toml"))?,
        snapshot_file("auth", &auth_path, &staging.join("auth.json"))?,
        snapshot_file(
            "manager-settings",
            settings_path,
            &staging.join("manager-settings.json"),
        )?,
        snapshot_file(
            "catalog",
            &home.join(CATALOG_FILE),
            &staging.join(CATALOG_FILE),
        )?,
    ];
    let manifest = BaselineManifest {
        schema_version: BASELINE_SCHEMA_VERSION,
        created_at_ms: now_ms(),
        codex_home: Some(crate::codex_home::codex_home_identity(home)?),
        original_provider: read_provider(&config_path),
        files: files.to_vec(),
    };
    crate::settings::atomic_write(
        &staging.join("manifest.json"),
        &serde_json::to_vec_pretty(&manifest)?,
    )?;
    validate_baseline_dir(staging, &manifest)?;
    Ok(manifest)
}

fn create_operation_snapshot(
    home: &Path,
    state_dir: &Path,
    settings_path: &Path,
    operation: &str,
) -> anyhow::Result<std::path::PathBuf> {
    let operations_root = state_dir.join("operations");
    let dir = operations_root.join(format!(
        "{}-{}-{}",
        now_ms(),
        operation,
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&dir)?;
    let snapshot_result = (|| -> anyhow::Result<()> {
        let files = [
            snapshot_file(
                "config",
                &home.join("config.toml"),
                &dir.join("config.toml"),
            )?,
            snapshot_file("auth", &home.join("auth.json"), &dir.join("auth.json"))?,
            snapshot_file(
                "manager-settings",
                settings_path,
                &dir.join("manager-settings.json"),
            )?,
            snapshot_file("catalog", &home.join(CATALOG_FILE), &dir.join(CATALOG_FILE))?,
            snapshot_file(
                "managed-state",
                &state_dir.join(MANAGED_STATE_FILE),
                &dir.join(MANAGED_STATE_FILE),
            )?,
        ];
        let manifest = OperationSnapshotManifest {
            schema_version: 1,
            created_at_ms: now_ms(),
            operation: operation.to_string(),
            files: files.to_vec(),
        };
        crate::settings::atomic_write(
            &dir.join("manifest.json"),
            &serde_json::to_vec_pretty(&manifest)?,
        )?;
        validate_snapshot_files(&dir, &manifest.files)
    })();
    if let Err(error) = snapshot_result {
        let _ = fs::remove_dir_all(&dir);
        return Err(error).context("无法创建完整操作快照，已清理未完成目录");
    }
    if let Err(error) = prune_operation_snapshots(&operations_root, &dir) {
        let _ = crate::diagnostic_log::append_diagnostic_log(
            "mirror_access.operation_snapshot_prune_failed",
            serde_json::json!({
                "operations_root": operations_root,
                "error": error.to_string(),
            }),
        );
    }
    Ok(dir)
}

fn prune_operation_snapshots(operations_root: &Path, keep_dir: &Path) -> anyhow::Result<usize> {
    if !operations_root.exists() {
        return Ok(0);
    }
    let canonical_root = fs::canonicalize(operations_root)?;
    let canonical_keep = fs::canonicalize(keep_dir)?;
    let mut snapshots = Vec::new();
    for entry in fs::read_dir(operations_root)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        let Ok(canonical_path) = fs::canonicalize(&path) else {
            continue;
        };
        if canonical_path.parent() != Some(canonical_root.as_path()) {
            continue;
        }
        let Ok(bytes) = fs::read(path.join("manifest.json")) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_slice::<OperationSnapshotManifest>(&bytes) else {
            continue;
        };
        if manifest.schema_version != 1 || validate_snapshot_files(&path, &manifest.files).is_err()
        {
            continue;
        }
        snapshots.push((manifest.created_at_ms, canonical_path));
    }
    snapshots.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    let remove_count = snapshots.len().saturating_sub(MAX_OPERATION_SNAPSHOTS);
    let mut removed = 0;
    for (_, path) in snapshots
        .into_iter()
        .filter(|(_, path)| path != &canonical_keep)
        .take(remove_count)
    {
        fs::remove_dir_all(&path)
            .with_context(|| format!("无法清理旧操作快照 {}", path.display()))?;
        removed += 1;
    }
    Ok(removed)
}

fn restore_operation_snapshot(
    home: &Path,
    state_dir: &Path,
    settings_path: &Path,
    snapshot_dir: &Path,
) -> anyhow::Result<()> {
    let operations_root = state_dir.join("operations");
    if !snapshot_dir.starts_with(&operations_root) || snapshot_dir == operations_root {
        bail!("操作快照路径不属于当前 Mirror X 状态目录。");
    }
    let manifest: OperationSnapshotManifest =
        serde_json::from_slice(&fs::read(snapshot_dir.join("manifest.json"))?)
            .with_context(|| "操作快照 manifest 无法解析")?;
    if manifest.schema_version != 1 {
        bail!("操作快照版本不受支持。");
    }
    validate_snapshot_files(snapshot_dir, &manifest.files)?;

    let targets = operation_snapshot_targets(home, state_dir, settings_path);
    let current = targets
        .iter()
        .map(|(id, path)| Ok((*id, path.clone(), read_optional(path)?)))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let restore_result = (|| -> anyhow::Result<()> {
        for file in &manifest.files {
            let target = targets
                .iter()
                .find_map(|(id, path)| (*id == file.id).then_some(path))
                .ok_or_else(|| anyhow::anyhow!("操作快照包含未知文件：{}", file.id))?;
            restore_snapshot_file(snapshot_dir, file, target)?;
        }
        validate_restored_snapshot(&manifest.files, &targets)?;
        Ok(())
    })();
    if let Err(error) = restore_result {
        let rollback_errors = current
            .iter()
            .filter_map(|(id, path, contents)| {
                restore_optional(path, contents.as_deref())
                    .err()
                    .map(|rollback_error| format!("{id}: {rollback_error}"))
            })
            .collect::<Vec<_>>();
        return if rollback_errors.is_empty() {
            Err(error).context("操作快照恢复失败，已回到恢复动作开始前状态")
        } else {
            Err(error).context(format!(
                "操作快照恢复失败，且恢复前状态有文件无法重新写回：{}",
                rollback_errors.join("；")
            ))
        };
    }
    Ok(())
}

fn operation_snapshot_targets<'a>(
    home: &Path,
    state_dir: &Path,
    settings_path: &'a Path,
) -> [(&'static str, std::path::PathBuf); 5] {
    [
        ("config", home.join("config.toml")),
        ("auth", home.join("auth.json")),
        ("manager-settings", settings_path.to_path_buf()),
        ("catalog", home.join(CATALOG_FILE)),
        ("managed-state", state_dir.join(MANAGED_STATE_FILE)),
    ]
}

fn validate_snapshot_files(snapshot_dir: &Path, files: &[BaselineFile]) -> anyhow::Result<()> {
    let mut ids = HashSet::new();
    for file in files {
        if !ids.insert(file.id.as_str()) {
            bail!("操作快照包含重复文件：{}", file.id);
        }
        if !file.existed {
            continue;
        }
        let bytes = fs::read(snapshot_dir.join(backup_name(&file.id)?))?;
        if file.sha256.as_deref() != Some(sha256(&bytes).as_str()) {
            bail!("操作快照文件 {} 校验失败。", file.id);
        }
    }
    let expected = [
        "config",
        "auth",
        "manager-settings",
        "catalog",
        "managed-state",
    ]
    .into_iter()
    .collect::<HashSet<_>>();
    if ids != expected {
        bail!("操作快照文件清单不完整，拒绝执行回滚。");
    }
    Ok(())
}

fn restore_snapshot_file(
    snapshot_dir: &Path,
    file: &BaselineFile,
    target: &Path,
) -> anyhow::Result<()> {
    if file.existed {
        let bytes = fs::read(snapshot_dir.join(backup_name(&file.id)?))?;
        crate::settings::atomic_write(target, &bytes)
    } else {
        restore_optional(target, None)
    }
}

fn validate_restored_snapshot(
    files: &[BaselineFile],
    targets: &[(&'static str, std::path::PathBuf)],
) -> anyhow::Result<()> {
    for file in files {
        let target = targets
            .iter()
            .find_map(|(id, path)| (*id == file.id).then_some(path))
            .ok_or_else(|| anyhow::anyhow!("操作快照包含未知文件：{}", file.id))?;
        if file.existed {
            let bytes = fs::read(target)?;
            if file.sha256.as_deref() != Some(sha256(&bytes).as_str()) {
                bail!("操作快照恢复后校验失败：{}", target.display());
            }
        } else if target.exists() {
            bail!("操作快照未能恢复文件不存在状态：{}", target.display());
        }
    }
    Ok(())
}

fn estimated_operation_bytes(home: &Path, state_dir: &Path, settings_path: &Path) -> u64 {
    let paths = [
        home.join("config.toml"),
        home.join("auth.json"),
        home.join(CATALOG_FILE),
        settings_path.to_path_buf(),
        state_dir.join(MANAGED_STATE_FILE),
    ];
    let existing = paths
        .iter()
        .filter_map(|path| fs::metadata(path).ok().map(|metadata| metadata.len()))
        .sum::<u64>();
    // Temporary copies and JSON/TOML rewrites can briefly coexist with originals.
    existing.saturating_mul(2).saturating_add(1024 * 1024)
}

fn snapshot_file(id: &str, source: &Path, target: &Path) -> anyhow::Result<BaselineFile> {
    match fs::read(source) {
        Ok(bytes) => {
            fs::write(target, &bytes)?;
            Ok(BaselineFile {
                id: id.to_string(),
                existed: true,
                sha256: Some(sha256(&bytes)),
            })
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(BaselineFile {
            id: id.to_string(),
            existed: false,
            sha256: None,
        }),
        Err(error) => Err(error.into()),
    }
}

fn validate_baseline(state_dir: &Path, manifest: &BaselineManifest) -> anyhow::Result<()> {
    validate_baseline_dir(&state_dir.join(BASELINE_DIR), manifest)
}

fn validate_baseline_dir(dir: &Path, manifest: &BaselineManifest) -> anyhow::Result<()> {
    if !matches!(
        manifest.schema_version,
        LEGACY_BASELINE_SCHEMA_VERSION | BASELINE_SCHEMA_VERSION
    ) {
        bail!("不支持的 baseline schema version。")
    }
    if manifest.schema_version == BASELINE_SCHEMA_VERSION && manifest.codex_home.is_none() {
        bail!("baseline 缺少 CODEX_HOME 绑定，已停止恢复。")
    }
    let required = ["config", "auth", "manager-settings"]
        .into_iter()
        .collect::<HashSet<_>>();
    let allowed = ["config", "auth", "manager-settings", "catalog"]
        .into_iter()
        .collect::<HashSet<_>>();
    let mut ids = HashSet::new();
    for file in &manifest.files {
        if !allowed.contains(file.id.as_str()) {
            bail!("baseline 包含未知文件：{}", file.id);
        }
        if !ids.insert(file.id.as_str()) {
            bail!("baseline 包含重复文件：{}", file.id);
        }
        if !file.existed {
            if file.sha256.is_some() {
                bail!("baseline 文件 {} 的存在状态与校验值不一致。", file.id);
            }
            continue;
        }
        if file.sha256.is_none() {
            bail!("baseline 文件 {} 缺少校验值。", file.id);
        }
        let path = dir.join(backup_name(&file.id)?);
        let bytes = fs::read(&path).with_context(|| format!("baseline 缺少 {}", path.display()))?;
        if file.sha256.as_deref() != Some(sha256(&bytes).as_str()) {
            bail!("baseline 文件 {} 校验失败。", file.id);
        }
    }
    if !required.is_subset(&ids) {
        bail!("baseline 文件清单不完整，必须包含 config、auth 和 manager-settings。")
    }
    Ok(())
}

fn load_valid_baseline_if_present(state_dir: &Path) -> anyhow::Result<Option<BaselineManifest>> {
    let baseline_dir = state_dir.join(BASELINE_DIR);
    match fs::metadata(&baseline_dir) {
        Ok(metadata) if metadata.is_dir() => {
            let baseline =
                load_baseline_manifest(state_dir).context("接管 baseline 无法读取或解析")?;
            validate_baseline(state_dir, &baseline)?;
            Ok(Some(baseline))
        }
        Ok(_) => bail!(
            "接管 baseline 路径不是目录，已保留恢复数据：{}",
            baseline_dir.display()
        ),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => {
            Err(error).with_context(|| format!("无法读取接管 baseline {}", baseline_dir.display()))
        }
    }
}

fn restore_catalog_from_baseline(
    state_dir: &Path,
    manifest: &BaselineManifest,
    target: &Path,
) -> anyhow::Result<()> {
    if manifest.files.iter().any(|file| file.id == "catalog") {
        restore_baseline_file(state_dir, manifest, "catalog", target)
    } else {
        // Baselines created by older releases did not record this managed file.
        restore_optional(target, None)
    }
}

fn restore_baseline_file(
    state_dir: &Path,
    manifest: &BaselineManifest,
    id: &str,
    target: &Path,
) -> anyhow::Result<()> {
    let file = manifest
        .files
        .iter()
        .find(|file| file.id == id)
        .ok_or_else(|| anyhow::anyhow!("baseline manifest 缺少 {id}"))?;
    if file.existed {
        let bytes = fs::read(state_dir.join(BASELINE_DIR).join(backup_name(id)?))?;
        crate::settings::atomic_write(target, &bytes)
    } else if target.exists() {
        fs::remove_file(target)
            .with_context(|| format!("无法恢复文件不存在状态：{}", target.display()))
    } else {
        Ok(())
    }
}

fn backup_name(id: &str) -> anyhow::Result<&'static str> {
    match id {
        "config" => Ok("config.toml"),
        "auth" => Ok("auth.json"),
        "manager-settings" => Ok("manager-settings.json"),
        "catalog" => Ok(CATALOG_FILE),
        "managed-state" => Ok(MANAGED_STATE_FILE),
        _ => bail!("未知 baseline 文件 id：{id}"),
    }
}

fn load_baseline_manifest(state_dir: &Path) -> anyhow::Result<BaselineManifest> {
    Ok(serde_json::from_slice(&fs::read(
        state_dir.join(BASELINE_DIR).join("manifest.json"),
    )?)?)
}

fn load_managed_state(state_dir: &Path) -> anyhow::Result<ManagedState> {
    let path = state_dir.join(MANAGED_STATE_FILE);
    let bytes = fs::read(&path).with_context(|| format!("无法读取接管状态 {}", path.display()))?;
    parse_managed_state(&path, &bytes)
}

fn load_managed_state_or_default(state_dir: &Path) -> anyhow::Result<ManagedState> {
    let path = state_dir.join(MANAGED_STATE_FILE);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(ManagedState::default()),
        Err(error) => {
            return Err(error).with_context(|| format!("无法读取接管状态 {}", path.display()));
        }
    };
    parse_managed_state(&path, &bytes)
}

fn parse_optional_managed_state(path: &Path, bytes: Option<&[u8]>) -> anyhow::Result<ManagedState> {
    match bytes {
        Some(bytes) => parse_managed_state(path, bytes),
        None => Ok(ManagedState::default()),
    }
}

fn parse_managed_state(path: &Path, bytes: &[u8]) -> anyhow::Result<ManagedState> {
    let state: ManagedState = serde_json::from_slice(bytes)
        .with_context(|| format!("接管状态 {} 不是有效 JSON", path.display()))?;
    if !matches!(
        state.schema_version,
        LEGACY_MANAGED_STATE_SCHEMA_VERSION | MANAGED_STATE_SCHEMA_VERSION
    ) {
        bail!("接管状态 {} 的 schema version 不受支持", path.display());
    }
    if state.schema_version == MANAGED_STATE_SCHEMA_VERSION
        && state_requires_home_binding(&state)
        && state.codex_home.is_none()
    {
        bail!("接管状态 {} 缺少 CODEX_HOME 绑定", path.display());
    }
    Ok(state)
}

fn save_managed_state(state_dir: &Path, state: &ManagedState) -> anyhow::Result<()> {
    crate::settings::atomic_write(
        &state_dir.join(MANAGED_STATE_FILE),
        &serde_json::to_vec_pretty(state)?,
    )
}

fn read_provider(config_path: &Path) -> String {
    fs::read_to_string(config_path)
        .ok()
        .and_then(|text| text.parse::<DocumentMut>().ok())
        .and_then(|doc| {
            doc.get("model_provider")
                .and_then(Item::as_str)
                .map(ToString::to_string)
        })
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "openai".to_string())
}

fn mcp_server_count(config_path: &Path) -> usize {
    fs::read_to_string(config_path)
        .ok()
        .and_then(|text| text.parse::<DocumentMut>().ok())
        .and_then(|doc| {
            doc.get("mcp_servers")
                .and_then(Item::as_table)
                .map(Table::len)
        })
        .unwrap_or(0)
}

fn preferred_default_model(models: &[MirrorModel]) -> String {
    for preferred in ["gpt-5.5", "gpt-5.4"] {
        if models.iter().any(|model| model.id == preferred) {
            return preferred.to_string();
        }
    }
    models
        .first()
        .map(|model| model.id.clone())
        .unwrap_or_default()
}

fn json_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
        .filter(|value| *value > 0)
}

fn read_optional(path: &Path) -> anyhow::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn restore_optional(path: &Path, contents: Option<&[u8]>) -> anyhow::Result<()> {
    match contents {
        Some(bytes) => crate::settings::atomic_write(path, bytes),
        None if path.exists() => {
            fs::remove_file(path)?;
            Ok(())
        }
        None => Ok(()),
    }
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_state_removal_allows_a_never_managed_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join(".mirrorplus");

        ensure_restored_for_state_removal(&home, &state).unwrap();
    }

    #[test]
    fn owned_state_removal_requires_config_and_session_restore() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join(".mirrorplus");
        let settings = state.join("settings.json");
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join("config.toml"), "model_provider = \"openai\"\n").unwrap();
        let discovery = parse_model_discovery(&json!({"data": [{"id": "gpt-5.4"}]})).unwrap();

        enable_access(
            &home,
            &state,
            &settings,
            "sk-test",
            MirrorAccessMode::PureApi,
            discovery,
        )
        .unwrap();
        assert!(
            ensure_restored_for_state_removal(&home, &state)
                .unwrap_err()
                .to_string()
                .contains("仍处于")
        );

        restore_access(&home, &state, &settings).unwrap();
        assert!(
            ensure_restored_for_state_removal(&home, &state)
                .unwrap_err()
                .to_string()
                .contains("历史会话恢复尚未完成")
        );

        record_session_sync(&home, &state, true, "synced").unwrap();
        ensure_restored_for_state_removal(&home, &state).unwrap();
    }

    #[test]
    fn owned_state_removal_refuses_corrupt_state_or_baseline() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join(".mirrorplus");
        fs::create_dir_all(&state).unwrap();
        fs::write(state.join(MANAGED_STATE_FILE), b"{broken").unwrap();

        assert!(ensure_restored_for_state_removal(&home, &state).is_err());

        fs::remove_file(state.join(MANAGED_STATE_FILE)).unwrap();
        let settings = state.join("settings.json");
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join("config.toml"), "model_provider = \"openai\"\n").unwrap();
        ensure_baseline(&home, &state, &settings).unwrap();
        fs::write(state.join(BASELINE_DIR).join("config.toml"), b"tampered").unwrap();

        assert!(
            ensure_restored_for_state_removal(&home, &state)
                .unwrap_err()
                .to_string()
                .contains("baseline")
        );
    }

    #[test]
    fn incomplete_baseline_is_reported_and_never_treated_as_unmanaged() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join(".mirrorplus");
        let settings = state.join("settings.json");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&state).unwrap();
        fs::write(home.join("config.toml"), "model_provider = \"openai\"\n").unwrap();
        ensure_baseline(&home, &state, &settings).unwrap();

        let manifest_path = state.join(BASELINE_DIR).join("manifest.json");
        let mut manifest: BaselineManifest =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest.files.retain(|file| file.id != "auth");
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();

        assert!(
            try_access_status(&home, &state)
                .unwrap_err()
                .to_string()
                .contains("清单不完整")
        );
        let rendered = access_status(&home, &state);
        assert_eq!(rendered.phase, "state_unreadable");
        assert!(rendered.baseline_exists);
        assert!(ensure_restored_for_state_removal(&home, &state).is_err());
    }

    #[test]
    fn baseline_preserves_a_preexisting_model_catalog() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join(".mirrorplus");
        let settings = state.join("settings.json");
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join("config.toml"), "model_provider = \"openai\"\n").unwrap();
        let original_catalog = b"{\"models\":[{\"slug\":\"user-model\"}]}";
        fs::write(home.join(CATALOG_FILE), original_catalog).unwrap();
        let discovery = parse_model_discovery(&json!({
            "data": [{"id": "gpt-5.4"}]
        }))
        .unwrap();

        enable_access(
            &home,
            &state,
            &settings,
            "sk-test",
            MirrorAccessMode::MixedApi,
            discovery,
        )
        .unwrap();
        assert_ne!(fs::read(home.join(CATALOG_FILE)).unwrap(), original_catalog);

        restore_access(&home, &state, &settings).unwrap();
        assert_eq!(fs::read(home.join(CATALOG_FILE)).unwrap(), original_catalog);
    }

    #[test]
    fn parses_models_and_prefers_gpt_55() {
        let discovery = parse_model_discovery(&json!({
            "data": [
                {"id": "other"},
                {"id": "gpt-5.5", "context_window": 400000},
                {"id": "gpt-5.5"}
            ]
        }))
        .unwrap();
        assert_eq!(discovery.default_model, "gpt-5.5");
        assert_eq!(discovery.models.len(), 2);
        assert_eq!(discovery.models[1].context_window, Some(400000));
    }

    #[test]
    fn rejects_empty_or_malformed_model_discovery() {
        assert!(parse_model_discovery(&json!({})).is_err());
        assert!(parse_model_discovery(&json!({"data": []})).is_err());
        assert!(parse_model_discovery(&json!({"data": [{"name": "missing-id"}]})).is_err());
    }

    #[test]
    fn selects_model_subset_in_service_order() {
        let discovery = parse_model_discovery(&json!({
            "data": [
                {"id": "model-a"},
                {"id": "model-b"},
                {"id": "model-c"}
            ]
        }))
        .unwrap();
        let selected = select_models(
            discovery,
            &["model-c".to_string(), "model-a".to_string()],
            "model-c",
        )
        .unwrap();

        assert_eq!(
            selected
                .models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec!["model-a", "model-c"]
        );
        assert_eq!(selected.default_model, "model-c");
    }

    #[test]
    fn rejects_invalid_model_selection() {
        let discovery = parse_model_discovery(&json!({
            "data": [{"id": "model-a"}, {"id": "model-b"}]
        }))
        .unwrap();
        assert!(select_models(discovery.clone(), &[], "model-a").is_err());
        assert!(
            select_models(discovery.clone(), &["missing".to_string()], "missing")
                .unwrap_err()
                .to_string()
                .contains("已不可用")
        );
        assert!(
            select_models(discovery, &["model-a".to_string()], "model-b")
                .unwrap_err()
                .to_string()
                .contains("默认模型")
        );
    }

    #[test]
    fn preflight_probe_uses_the_same_group_route_and_key_as_persisted_settings() {
        let discovery = parse_model_discovery(&json!({
            "data": [{"id": "gpt-5.4", "context_window": 400000}]
        }))
        .unwrap();
        let probe = probe_profile_for_group(&MirrorAccessGroup {
            id: "codexpro".to_string(),
            label: "CodexPro".to_string(),
            api_key: "sk-codexpro".to_string(),
            discovery,
        })
        .unwrap();

        assert_eq!(probe.label, "CodexPro");
        assert_eq!(probe.model, "gpt-5.4");
        assert_eq!(probe.profile.protocol, RelayProtocol::Responses);
        assert_eq!(
            crate::relay_config::relay_profile_base_url(&probe.profile),
            MIRROR_BASE_URL
        );
        assert_eq!(
            crate::relay_config::relay_profile_api_key(&probe.profile),
            "sk-codexpro"
        );
        assert_eq!(
            crate::relay_config::relay_profile_model(&probe.profile),
            "gpt-5.4"
        );
    }

    #[test]
    fn managed_catalog_contains_only_selected_models() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join(".mirrorplus");
        let settings = state.join("settings.json");
        fs::create_dir_all(&home).unwrap();
        let discovery = parse_model_discovery(&json!({
            "data": [
                {"id": "model-a"},
                {"id": "model-b"},
                {"id": "model-c"}
            ]
        }))
        .unwrap();
        let discovery = select_models(discovery, &["model-b".to_string()], "model-b").unwrap();

        enable_access(
            &home,
            &state,
            &settings,
            "sk-test",
            MirrorAccessMode::PureApi,
            discovery,
        )
        .unwrap();

        let catalog: Value =
            serde_json::from_slice(&fs::read(home.join(CATALOG_FILE)).unwrap()).unwrap();
        let model_ids = catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|model| model["slug"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(model_ids, vec!["model-b"]);
        assert_eq!(catalog["models"][0]["use_responses_lite"], false);
    }

    #[test]
    fn grouped_access_writes_local_proxy_and_isolates_group_keys() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join(".mirrorplus");
        let settings_path = state.join("settings.json");
        fs::create_dir_all(&home).unwrap();
        let codexpro = parse_model_discovery(&json!({
            "data": [{"id": "gpt-5.4"}, {"id": "grok-4.1"}]
        }))
        .unwrap();
        let claude = parse_model_discovery(&json!({
            "data": [{"id": "claude-opus-4-6"}]
        }))
        .unwrap();

        let transaction = enable_grouped_access_transaction(
            &home,
            &state,
            &settings_path,
            MirrorAccessMode::MixedApi,
            vec![
                MirrorAccessGroup {
                    id: "codexpro".to_string(),
                    label: "CodexPro".to_string(),
                    api_key: "sk-codexpro".to_string(),
                    discovery: codexpro,
                },
                MirrorAccessGroup {
                    id: "claude".to_string(),
                    label: "Claude".to_string(),
                    api_key: "sk-claude".to_string(),
                    discovery: claude,
                },
            ],
            "gpt-5.4",
        )
        .unwrap();

        assert_eq!(transaction.probe_profiles.len(), 2);
        assert_eq!(transaction.probe_profiles[0].label, "CodexPro");
        assert_eq!(transaction.probe_profiles[0].model, "gpt-5.4");
        assert_eq!(
            crate::relay_config::relay_profile_api_key(&transaction.probe_profiles[0].profile),
            "sk-codexpro"
        );
        assert_eq!(transaction.probe_profiles[1].label, "Claude");
        assert_eq!(transaction.probe_profiles[1].model, "claude-opus-4-6");

        let config = fs::read_to_string(home.join("config.toml")).unwrap();
        assert!(config.contains("http://127.0.0.1:57321/v1"));
        assert!(!config.contains("sk-codexpro"));
        assert!(!config.contains("sk-claude"));
        let settings = SettingsStore::new(settings_path).load().unwrap();
        assert_eq!(settings.active_relay_id, MIRROR_PROVIDER_ID);
        assert_eq!(settings.active_aggregate_relay_id, MIRROR_PROVIDER_ID);
        assert!(!settings.provider_sync_enabled);
        assert_eq!(settings.aggregate_relay_profiles.len(), 1);
        let codexpro = settings
            .relay_profiles
            .iter()
            .find(|profile| profile.id == "mirrorplus-codexpro")
            .unwrap();
        let claude = settings
            .relay_profiles
            .iter()
            .find(|profile| profile.id == "mirrorplus-claude")
            .unwrap();
        assert_eq!(codexpro.api_key, "sk-codexpro");
        assert_eq!(codexpro.model_list, "gpt-5.4\ngrok-4.1");
        assert_eq!(claude.api_key, "sk-claude");
        assert_eq!(claude.model_list, "claude-opus-4-6");
    }

    #[test]
    fn mixed_mode_preserves_auth_file_and_uses_responses_provider() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join(".mirrorplus");
        let settings = state.join("settings.json");
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join("config.toml"), "model_provider = \"openai\"\n").unwrap();
        fs::write(
            home.join("auth.json"),
            br#"{"tokens":{"access_token":"official"}}"#,
        )
        .unwrap();
        let discovery = parse_model_discovery(&json!({"data": [{"id": "gpt-5.4"}]})).unwrap();
        enable_access(
            &home,
            &state,
            &settings,
            "sk-secret",
            MirrorAccessMode::MixedApi,
            discovery,
        )
        .unwrap();
        assert_eq!(
            fs::read(home.join("auth.json")).unwrap(),
            br#"{"tokens":{"access_token":"official"}}"#
        );
        let config = fs::read_to_string(home.join("config.toml")).unwrap();
        assert!(config.contains("wire_api = \"responses\""));
        assert!(config.contains("experimental_bearer_token = \"sk-secret\""));
        let managed = SettingsStore::new(settings).load().unwrap();
        assert!(managed.enhancements_enabled);
        assert!(managed.codex_app_model_whitelist_unlock);
        assert!(managed.codex_app_plugin_marketplace_unlock);
        assert!(managed.codex_app_plugin_auto_expand);
    }

    #[test]
    fn enable_transaction_rolls_back_to_the_immediately_previous_working_access() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join(".mirrorplus");
        let settings = state.join("settings.json");
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join("config.toml"), "model_provider = \"openai\"\n").unwrap();
        fs::write(
            home.join("auth.json"),
            br#"{"tokens":{"access_token":"official"}}"#,
        )
        .unwrap();
        let first = parse_model_discovery(&json!({"data": [{"id": "gpt-first"}]})).unwrap();
        enable_access(
            &home,
            &state,
            &settings,
            "sk-first",
            MirrorAccessMode::MixedApi,
            first,
        )
        .unwrap();
        let paths = [
            home.join("config.toml"),
            home.join("auth.json"),
            settings.clone(),
            home.join(CATALOG_FILE),
            state.join(MANAGED_STATE_FILE),
        ];
        let before = paths
            .iter()
            .map(|path| fs::read(path).unwrap())
            .collect::<Vec<_>>();
        let second = parse_model_discovery(&json!({"data": [{"id": "gpt-second"}]})).unwrap();

        let transaction = enable_grouped_access_transaction(
            &home,
            &state,
            &settings,
            MirrorAccessMode::MixedApi,
            vec![MirrorAccessGroup {
                id: "default".to_string(),
                label: "镜子AI".to_string(),
                api_key: "sk-second".to_string(),
                discovery: second,
            }],
            "gpt-second",
        )
        .unwrap();
        assert!(
            fs::read_to_string(home.join("config.toml"))
                .unwrap()
                .contains("sk-second")
        );

        let status = transaction.rollback(&home, &state, &settings).unwrap();

        assert!(status.active);
        assert_eq!(status.default_model, "gpt-first");
        for (path, expected) in paths.iter().zip(before) {
            assert_eq!(fs::read(path).unwrap(), expected, "{}", path.display());
        }
    }

    #[test]
    fn pure_api_replaces_official_auth_and_uses_explicit_provider_key() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join(".mirrorplus");
        let settings = state.join("settings.json");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("auth.json"),
            br#"{"auth_mode":"chatgpt","tokens":{"access_token":"official"}}"#,
        )
        .unwrap();
        let discovery = parse_model_discovery(&json!({"data": [{"id": "gpt-5.4"}]})).unwrap();

        enable_access(
            &home,
            &state,
            &settings,
            "sk-pure",
            MirrorAccessMode::PureApi,
            discovery,
        )
        .unwrap();

        let auth: Value =
            serde_json::from_slice(&fs::read(home.join("auth.json")).unwrap()).unwrap();
        assert_eq!(auth, json!({"OPENAI_API_KEY": "sk-pure"}));
        let config = fs::read_to_string(home.join("config.toml")).unwrap();
        assert!(!config.contains("requires_openai_auth"));
        assert!(config.contains("experimental_bearer_token = \"sk-pure\""));
        assert!(config.contains("cli_auth_credentials_store = \"file\""));
        assert!(config.contains("forced_login_method = \"api\""));
    }

    #[test]
    fn pure_api_deactivates_stale_profile_that_selects_missing_custom_provider() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join(".mirrorplus");
        let settings = state.join("settings.json");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("config.toml"),
            concat!(
                "model_provider = \"openai\"\n",
                "profile = \"legacy\"\n\n",
                "[profiles.legacy]\n",
                "model_provider = \"custom\"\n",
                "model = \"old-model\"\n",
            ),
        )
        .unwrap();
        let discovery = parse_model_discovery(&json!({"data": [{"id": "gpt-5.4"}]})).unwrap();

        enable_access(
            &home,
            &state,
            &settings,
            "sk-pure",
            MirrorAccessMode::PureApi,
            discovery,
        )
        .unwrap();

        let config = fs::read_to_string(home.join("config.toml")).unwrap();
        let doc = config.parse::<DocumentMut>().unwrap();
        assert_eq!(
            doc.get("model_provider").and_then(Item::as_str),
            Some(MIRROR_CODEX_PROVIDER_ID)
        );
        assert!(doc.get("profile").is_none());
        assert_eq!(
            doc.get("profiles")
                .and_then(Item::as_table)
                .and_then(|profiles| profiles.get("legacy"))
                .and_then(Item::as_table)
                .and_then(|profile| profile.get("model_provider"))
                .and_then(Item::as_str),
            Some("custom")
        );
        assert!(config.contains("[model_providers.mirrorplus]"));
        assert!(config.contains("[model_providers.custom]"));

        restore_access(&home, &state, &settings).unwrap();
        let restored = fs::read_to_string(home.join("config.toml"))
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        assert_eq!(
            restored.get("profile").and_then(Item::as_str),
            Some("legacy")
        );
        assert_eq!(
            restored
                .get("profiles")
                .and_then(Item::as_table)
                .and_then(|profiles| profiles.get("legacy"))
                .and_then(Item::as_table)
                .and_then(|profile| profile.get("model_provider"))
                .and_then(Item::as_str),
            Some("custom")
        );
        assert!(
            restored
                .get("model_providers")
                .and_then(Item::as_table)
                .and_then(|providers| providers.get(MIRROR_CODEX_PROVIDER_ID))
                .is_none()
        );
    }

    #[test]
    fn mixed_api_deactivates_stale_profile_that_selects_missing_custom_provider() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join(".mirrorplus");
        let settings = state.join("settings.json");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("config.toml"),
            concat!(
                "model_provider = \"openai\"\n",
                "profile = \"legacy\"\n\n",
                "[profiles.legacy]\n",
                "model_provider = \"custom\"\n",
            ),
        )
        .unwrap();
        fs::write(
            home.join("auth.json"),
            br#"{"auth_mode":"chatgpt","tokens":{"access_token":"official"}}"#,
        )
        .unwrap();
        let discovery = parse_model_discovery(&json!({"data": [{"id": "gpt-5.4"}]})).unwrap();

        enable_access(
            &home,
            &state,
            &settings,
            "sk-mixed",
            MirrorAccessMode::MixedApi,
            discovery,
        )
        .unwrap();

        let config = fs::read_to_string(home.join("config.toml")).unwrap();
        let doc = config.parse::<DocumentMut>().unwrap();
        assert_eq!(
            doc.get("model_provider").and_then(Item::as_str),
            Some(MIRROR_CODEX_PROVIDER_ID)
        );
        assert!(doc.get("profile").is_none());
        assert_eq!(
            doc.get("profiles")
                .and_then(Item::as_table)
                .and_then(|profiles| profiles.get("legacy"))
                .and_then(Item::as_table)
                .and_then(|profile| profile.get("model_provider"))
                .and_then(Item::as_str),
            Some("custom")
        );
        assert!(config.contains("[model_providers.mirrorplus]"));
        assert!(config.contains("[model_providers.custom]"));
        assert!(config.contains("requires_openai_auth = true"));
    }

    #[test]
    fn prelaunch_repair_restores_pure_provider_without_touching_healthy_files_twice() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join(".mirrorplus");
        let settings = state.join("settings.json");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("config.toml"),
            "model_provider = \"openai\"\n\n[mcp_servers.keep]\ncommand = \"keep\"\n",
        )
        .unwrap();
        let discovery = parse_model_discovery(&json!({"data": [{"id": "gpt-5.4"}]})).unwrap();
        enable_access(
            &home,
            &state,
            &settings,
            "sk-pure",
            MirrorAccessMode::PureApi,
            discovery,
        )
        .unwrap();
        let expected_auth = fs::read(home.join("auth.json")).unwrap();

        let config_path = home.join("config.toml");
        let mut damaged = fs::read_to_string(&config_path)
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        damaged["profile"] = value("legacy");
        damaged["profiles"] = Item::Table(Table::new());
        let mut legacy = Table::new();
        legacy.insert("model_provider", value("custom"));
        damaged["profiles"]
            .as_table_mut()
            .unwrap()
            .insert("legacy", Item::Table(legacy));
        // Reproduce the delivered failure exactly: the root selects `custom`,
        // but only the older `mirrorplus` provider table remains.
        damaged["model_providers"]
            .as_table_mut()
            .unwrap()
            .remove(MIRROR_CODEX_PROVIDER_ID);
        fs::write(&config_path, damaged.to_string()).unwrap();

        assert!(ensure_managed_provider_ready(&home, &state, &settings).unwrap());
        let repaired_bytes = fs::read(&config_path).unwrap();
        let repaired = String::from_utf8(repaired_bytes.clone())
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        assert!(repaired.get("profile").is_none());
        assert!(
            repaired
                .get("model_providers")
                .and_then(Item::as_table)
                .and_then(|providers| providers.get(MIRROR_PROVIDER_ID))
                .is_some()
        );
        assert!(
            repaired
                .get("model_providers")
                .and_then(Item::as_table)
                .and_then(|providers| providers.get(MIRROR_CODEX_PROVIDER_ID))
                .is_some()
        );
        assert_eq!(
            repaired
                .get("profiles")
                .and_then(Item::as_table)
                .and_then(|profiles| profiles.get("legacy"))
                .and_then(Item::as_table)
                .and_then(|profile| profile.get("model_provider"))
                .and_then(Item::as_str),
            Some("custom")
        );
        assert!(repaired.get("mcp_servers").is_some());
        assert_eq!(fs::read(home.join("auth.json")).unwrap(), expected_auth);

        let catalog_path = home.join(CATALOG_FILE);
        let expected_catalog = fs::read(&catalog_path).unwrap();
        let expected_settings = fs::read(&settings).unwrap();
        let operation_count = fs::read_dir(state.join("operations")).unwrap().count();
        let config_modified = fs::metadata(&config_path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(!ensure_managed_provider_ready(&home, &state, &settings).unwrap());
        assert_eq!(fs::read(&config_path).unwrap(), repaired_bytes);
        assert_eq!(
            fs::metadata(&config_path).unwrap().modified().unwrap(),
            config_modified
        );
        assert_eq!(fs::read(&catalog_path).unwrap(), expected_catalog);
        assert_eq!(fs::read(&settings).unwrap(), expected_settings);
        assert_eq!(
            fs::read_dir(state.join("operations")).unwrap().count(),
            operation_count
        );
    }

    #[test]
    fn prelaunch_repair_keeps_grouped_pure_mode_and_default_group_auth() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join(".mirrorplus");
        let settings_path = state.join("settings.json");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("config.toml"),
            "sandbox_mode = \"danger-full-access\"\n\n[mcp_servers.keep]\ncommand = \"keep\"\n",
        )
        .unwrap();
        let codexpro = parse_model_discovery(&json!({"data": [{"id": "gpt-5.4"}]})).unwrap();
        let enterprise =
            parse_model_discovery(&json!({"data": [{"id": "claude-opus-4-6"}]})).unwrap();
        enable_grouped_access(
            &home,
            &state,
            &settings_path,
            MirrorAccessMode::PureApi,
            vec![
                MirrorAccessGroup {
                    id: "codexpro".to_string(),
                    label: "CodexPro".to_string(),
                    api_key: "sk-codexpro".to_string(),
                    discovery: codexpro,
                },
                MirrorAccessGroup {
                    id: "enterprise".to_string(),
                    label: "企业专线".to_string(),
                    api_key: "sk-enterprise".to_string(),
                    discovery: enterprise,
                },
            ],
            "claude-opus-4-6",
        )
        .unwrap();
        let expected_settings = fs::read(&settings_path).unwrap();

        let config_path = home.join("config.toml");
        let mut damaged = fs::read_to_string(&config_path)
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        damaged["profile"] = value("legacy");
        let provider = damaged["model_providers"]
            .as_table_mut()
            .unwrap()
            .get_mut(MIRROR_CODEX_PROVIDER_ID)
            .and_then(Item::as_table_mut)
            .unwrap();
        provider.insert("requires_openai_auth", value(true));
        provider.insert("experimental_bearer_token", value("wrong-mode"));
        fs::write(&config_path, damaged.to_string()).unwrap();
        fs::write(
            home.join("auth.json"),
            serde_json::to_vec_pretty(&json!({"OPENAI_API_KEY": "wrong-key"})).unwrap(),
        )
        .unwrap();

        assert!(ensure_managed_provider_ready(&home, &state, &settings_path).unwrap());
        let repaired = fs::read_to_string(&config_path)
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        let provider = repaired["model_providers"]
            .as_table()
            .unwrap()
            .get(MIRROR_CODEX_PROVIDER_ID)
            .and_then(Item::as_table)
            .unwrap();
        let expected_proxy_base_url = crate::protocol_proxy::local_responses_proxy_base_url(
            crate::protocol_proxy::DEFAULT_PROTOCOL_PROXY_PORT,
        );
        assert!(!provider.contains_key("requires_openai_auth"));
        assert_eq!(
            provider
                .get("experimental_bearer_token")
                .and_then(Item::as_str),
            Some("codex-plus-aggregate")
        );
        assert_eq!(
            provider.get("base_url").and_then(Item::as_str),
            Some(expected_proxy_base_url.as_str())
        );
        assert_eq!(
            repaired.get("sandbox_mode").and_then(Item::as_str),
            Some("danger-full-access")
        );
        assert!(repaired.get("mcp_servers").is_some());
        let auth: Value =
            serde_json::from_slice(&fs::read(home.join("auth.json")).unwrap()).unwrap();
        assert_eq!(auth["OPENAI_API_KEY"], "sk-enterprise");
        assert_eq!(fs::read(&settings_path).unwrap(), expected_settings);
    }

    #[test]
    fn prelaunch_repair_keeps_mixed_auth_and_rebuilds_provider_table() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join(".mirrorplus");
        let settings = state.join("settings.json");
        fs::create_dir_all(&home).unwrap();
        let official_auth = br#"{"auth_mode":"chatgpt","tokens":{"access_token":"official"}}"#;
        fs::write(home.join("auth.json"), official_auth).unwrap();
        let discovery = parse_model_discovery(&json!({"data": [{"id": "gpt-5.4"}]})).unwrap();
        enable_access(
            &home,
            &state,
            &settings,
            "sk-mixed",
            MirrorAccessMode::MixedApi,
            discovery,
        )
        .unwrap();

        let config_path = home.join("config.toml");
        let mut damaged = fs::read_to_string(&config_path)
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        damaged["profile"] = value("legacy");
        damaged["model_providers"]
            .as_table_mut()
            .unwrap()
            .remove(MIRROR_PROVIDER_ID);
        damaged["model_providers"]
            .as_table_mut()
            .unwrap()
            .remove(MIRROR_CODEX_PROVIDER_ID);
        damaged["cli_auth_credentials_store"] = value("file");
        damaged["forced_login_method"] = value("api");
        fs::write(&config_path, damaged.to_string()).unwrap();

        assert!(ensure_managed_provider_ready(&home, &state, &settings).unwrap());
        assert_eq!(fs::read(home.join("auth.json")).unwrap(), official_auth);
        let repaired = fs::read_to_string(&config_path)
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        let provider = repaired["model_providers"]
            .as_table()
            .unwrap()
            .get(MIRROR_CODEX_PROVIDER_ID)
            .and_then(Item::as_table)
            .unwrap();
        assert_eq!(
            provider.get("requires_openai_auth").and_then(Item::as_bool),
            Some(true)
        );
        assert_eq!(
            provider
                .get("experimental_bearer_token")
                .and_then(Item::as_str),
            Some("sk-mixed")
        );
        let compatibility_provider = repaired["model_providers"]
            .as_table()
            .unwrap()
            .get(MIRROR_PROVIDER_ID)
            .and_then(Item::as_table)
            .unwrap();
        assert_eq!(
            compatibility_provider
                .get("experimental_bearer_token")
                .and_then(Item::as_str),
            Some("sk-mixed")
        );
        assert_eq!(
            compatibility_provider
                .get("requires_openai_auth")
                .and_then(Item::as_bool),
            Some(true)
        );
        assert!(repaired.get("cli_auth_credentials_store").is_none());
        assert!(repaired.get("forced_login_method").is_none());
    }

    #[test]
    fn prelaunch_repair_fails_closed_when_live_mirror_and_manager_disagree() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join(".mirrorplus");
        let settings_path = state.join("settings.json");
        fs::create_dir_all(&home).unwrap();
        let discovery = parse_model_discovery(&json!({"data": [{"id": "gpt-5.4"}]})).unwrap();
        enable_access(
            &home,
            &state,
            &settings_path,
            "sk-pure",
            MirrorAccessMode::PureApi,
            discovery,
        )
        .unwrap();
        let mut settings: Value =
            serde_json::from_slice(&fs::read(&settings_path).unwrap()).unwrap();
        settings["activeRelayId"] = Value::String("other".to_string());
        fs::write(
            &settings_path,
            serde_json::to_vec_pretty(&settings).unwrap(),
        )
        .unwrap();
        let config_before = fs::read(home.join("config.toml")).unwrap();
        let auth_before = fs::read(home.join("auth.json")).unwrap();
        let catalog_before = fs::read(home.join(CATALOG_FILE)).unwrap();
        let operation_count = fs::read_dir(state.join("operations")).unwrap().count();

        let error = ensure_managed_provider_ready(&home, &state, &settings_path).unwrap_err();

        assert!(error.to_string().contains("已不一致"));
        assert_eq!(fs::read(home.join("config.toml")).unwrap(), config_before);
        assert_eq!(fs::read(home.join("auth.json")).unwrap(), auth_before);
        assert_eq!(fs::read(home.join(CATALOG_FILE)).unwrap(), catalog_before);
        assert_eq!(
            fs::read_dir(state.join("operations")).unwrap().count(),
            operation_count
        );
    }

    #[test]
    fn prelaunch_provider_check_is_noop_without_active_managed_state() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join(".mirrorplus");
        let settings = state.join("settings.json");
        fs::create_dir_all(&home).unwrap();
        let config = b"model_provider = \"third-party\"\ncustom_setting = true\n";
        fs::write(home.join("config.toml"), config).unwrap();

        assert!(!ensure_managed_provider_ready(&home, &state, &settings).unwrap());
        assert_eq!(fs::read(home.join("config.toml")).unwrap(), config);
        assert!(!state.exists());
    }

    #[test]
    fn prelaunch_provider_repair_restores_snapshot_when_a_write_fails() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join(".mirrorplus");
        let settings = state.join("settings.json");
        fs::create_dir_all(&home).unwrap();
        let discovery = parse_model_discovery(&json!({"data": [{"id": "gpt-5.4"}]})).unwrap();
        enable_access(
            &home,
            &state,
            &settings,
            "sk-pure",
            MirrorAccessMode::PureApi,
            discovery,
        )
        .unwrap();

        let config_path = home.join("config.toml");
        let mut damaged = fs::read_to_string(&config_path)
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        damaged["profile"] = value("legacy");
        fs::write(&config_path, damaged.to_string()).unwrap();
        fs::write(home.join(CATALOG_FILE), b"{broken-catalog").unwrap();
        let protected_paths = [
            config_path.clone(),
            home.join("auth.json"),
            home.join(CATALOG_FILE),
            settings.clone(),
            state.join(MANAGED_STATE_FILE),
        ];
        let before = protected_paths
            .iter()
            .map(|path| fs::read(path).unwrap())
            .collect::<Vec<_>>();
        FAIL_PROVIDER_REPAIR_AFTER_CATALOG_WRITE.with(|flag| flag.set(true));

        let error = ensure_managed_provider_ready(&home, &state, &settings).unwrap_err();

        assert!(error.to_string().contains("已恢复写入前状态"), "{error:#}");
        for (path, expected) in protected_paths.iter().zip(&before) {
            assert_eq!(&fs::read(path).unwrap(), expected, "{}", path.display());
        }
        assert!(ensure_managed_provider_ready(&home, &state, &settings).unwrap());
    }

    #[test]
    fn switching_from_pure_back_to_mixed_restores_baseline_login() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join(".mirrorplus");
        let settings = state.join("settings.json");
        fs::create_dir_all(&home).unwrap();
        let official_auth = br#"{"auth_mode":"chatgpt","tokens":{"access_token":"official"}}"#;
        fs::write(home.join("auth.json"), official_auth).unwrap();
        let discovery = parse_model_discovery(&json!({"data": [{"id": "gpt-5.4"}]})).unwrap();
        assert_eq!(restorable_chatgpt_login(&state).unwrap(), None);

        enable_access(
            &home,
            &state,
            &settings,
            "sk-pure",
            MirrorAccessMode::PureApi,
            discovery.clone(),
        )
        .unwrap();
        assert_eq!(
            restorable_chatgpt_login(&state).unwrap(),
            Some(RestorableChatgptLogin::AuthFile)
        );
        enable_access(
            &home,
            &state,
            &settings,
            "sk-mixed",
            MirrorAccessMode::MixedApi,
            discovery,
        )
        .unwrap();
        assert_eq!(restorable_chatgpt_login(&state).unwrap(), None);

        assert_eq!(fs::read(home.join("auth.json")).unwrap(), official_auth);
        let config = fs::read_to_string(home.join("config.toml")).unwrap();
        assert!(config.contains("requires_openai_auth = true"));
        assert!(config.contains("experimental_bearer_token = \"sk-mixed\""));
        assert!(!config.contains("cli_auth_credentials_store"));
        assert!(!config.contains("forced_login_method"));
    }

    #[test]
    fn restorable_login_rejects_api_key_or_incomplete_chatgpt_baselines() {
        for auth in [
            br#"{"auth_mode":"apikey","tokens":{"access_token":"api"}}"#.as_slice(),
            br#"{"auth_mode":"chatgpt","tokens":{}}"#.as_slice(),
            br#"{"tokens":{"access_token":"missing-mode"}}"#.as_slice(),
        ] {
            assert!(!auth_contents_have_chatgpt_login(auth));
        }
        assert!(auth_contents_have_chatgpt_login(
            br#"{"auth_mode":"chatgpt","tokens":{"refresh_token":"official"}}"#
        ));
    }

    #[test]
    fn pure_to_mixed_restores_preexisting_keyring_and_login_policy() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join(".mirrorplus");
        let settings = state.join("settings.json");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("config.toml"),
            "cli_auth_credentials_store = \"keyring\"\nforced_login_method = \"chatgpt\"\n",
        )
        .unwrap();
        let discovery = parse_model_discovery(&json!({"data": [{"id": "gpt-5.4"}]})).unwrap();

        enable_access(
            &home,
            &state,
            &settings,
            "sk-pure",
            MirrorAccessMode::PureApi,
            discovery.clone(),
        )
        .unwrap();
        let pure = fs::read_to_string(home.join("config.toml")).unwrap();
        assert!(pure.contains("cli_auth_credentials_store = \"file\""));
        assert!(pure.contains("forced_login_method = \"api\""));
        assert_eq!(
            restorable_chatgpt_login(&state).unwrap(),
            Some(RestorableChatgptLogin::CredentialStore {
                credentials_store: "keyring".to_string(),
                forced_login_method: Some("chatgpt".to_string()),
            })
        );

        enable_access(
            &home,
            &state,
            &settings,
            "sk-mixed",
            MirrorAccessMode::MixedApi,
            discovery,
        )
        .unwrap();
        let mixed = fs::read_to_string(home.join("config.toml")).unwrap();
        assert!(mixed.contains("cli_auth_credentials_store = \"keyring\""));
        assert!(mixed.contains("forced_login_method = \"chatgpt\""));
    }

    #[test]
    fn pure_baseline_forced_to_api_is_not_treated_as_restorable_chatgpt() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join(".mirrorplus");
        let settings = state.join("settings.json");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("config.toml"),
            "cli_auth_credentials_store = \"keyring\"\nforced_login_method = \"api\"\n",
        )
        .unwrap();
        let discovery = parse_model_discovery(&json!({"data": [{"id": "gpt-5.4"}]})).unwrap();

        enable_access(
            &home,
            &state,
            &settings,
            "sk-pure",
            MirrorAccessMode::PureApi,
            discovery,
        )
        .unwrap();

        assert_eq!(restorable_chatgpt_login(&state).unwrap(), None);
    }

    #[test]
    fn grouped_pure_api_auth_uses_real_default_group_key() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join(".mirrorplus");
        let settings = state.join("settings.json");
        fs::create_dir_all(&home).unwrap();
        let codexpro = parse_model_discovery(&json!({"data": [{"id": "gpt-5.4"}]})).unwrap();
        let claude = parse_model_discovery(&json!({"data": [{"id": "claude-opus-4-6"}]})).unwrap();

        enable_grouped_access(
            &home,
            &state,
            &settings,
            MirrorAccessMode::PureApi,
            vec![
                MirrorAccessGroup {
                    id: "codexpro".to_string(),
                    label: "CodexPro".to_string(),
                    api_key: "sk-codexpro".to_string(),
                    discovery: codexpro,
                },
                MirrorAccessGroup {
                    id: "claude".to_string(),
                    label: "Claude".to_string(),
                    api_key: "sk-claude".to_string(),
                    discovery: claude,
                },
            ],
            "claude-opus-4-6",
        )
        .unwrap();

        let auth: Value =
            serde_json::from_slice(&fs::read(home.join("auth.json")).unwrap()).unwrap();
        assert_eq!(auth["OPENAI_API_KEY"], "sk-claude");
        assert_ne!(auth["OPENAI_API_KEY"], "codex-plus-aggregate");
    }

    #[test]
    fn refuses_to_overwrite_invalid_existing_config() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join(".mirrorplus");
        let settings = state.join("settings.json");
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join("config.toml"), "not = [valid").unwrap();
        let discovery = parse_model_discovery(&json!({"data": [{"id": "gpt-5.5"}]})).unwrap();
        let error = enable_access(
            &home,
            &state,
            &settings,
            "sk-test",
            MirrorAccessMode::PureApi,
            discovery,
        )
        .unwrap_err();
        assert!(error.to_string().contains("无法解析"));
        assert_eq!(
            fs::read_to_string(home.join("config.toml")).unwrap(),
            "not = [valid"
        );
    }

    #[test]
    fn preflight_accepts_missing_or_valid_config_and_rejects_invalid_config() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        fs::create_dir_all(&home).unwrap();
        assert!(validate_existing_config(&home).is_ok());

        fs::write(home.join("config.toml"), "model_provider = \"openai\"\n").unwrap();
        assert!(validate_existing_config(&home).is_ok());

        fs::write(home.join("config.toml"), "not = [valid").unwrap();
        assert!(validate_existing_config(&home).is_err());
    }

    #[test]
    fn storage_headroom_check_fails_before_writes_when_reserve_is_unavailable() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("nested").join("config.toml");
        let error = ensure_storage_headroom(&target, 1, u64::MAX).unwrap_err();
        assert!(error.to_string().contains("剩余空间不足"));
        assert!(!target.exists());
    }

    #[test]
    fn runtime_storage_paths_keep_one_probe_per_volume() {
        #[cfg(windows)]
        let paths = vec![
            PathBuf::from(r"C:\Users\test\.codex"),
            PathBuf::from(r"c:\Users\test\AppData\Local"),
            PathBuf::from(r"D:\Portable\Codex"),
        ];
        #[cfg(not(windows))]
        let paths = vec![PathBuf::from("/tmp/codex"), PathBuf::from("/var/tmp/codex")];

        let paths = storage_paths_by_volume(paths);

        #[cfg(windows)]
        assert_eq!(paths.len(), 2);
        #[cfg(not(windows))]
        assert_eq!(paths.len(), 1);
    }

    #[test]
    fn operation_snapshots_keep_a_bounded_set_and_preserve_the_latest() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join("state");
        let settings = temp.path().join("settings.json");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&state).unwrap();
        fs::write(home.join("config.toml"), "model_provider = \"openai\"\n").unwrap();

        let mut latest = None;
        for _ in 0..(MAX_OPERATION_SNAPSHOTS + 4) {
            latest = Some(create_operation_snapshot(&home, &state, &settings, "test").unwrap());
        }

        let snapshots = fs::read_dir(state.join("operations"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .collect::<Vec<_>>();
        assert_eq!(snapshots.len(), MAX_OPERATION_SNAPSHOTS);
        assert!(latest.unwrap().exists());
    }

    #[test]
    fn access_status_reports_preserved_mcp_servers() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join(".mirrorplus");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("config.toml"),
            "[mcp_servers.one]\ncommand = \"one\"\n\n[mcp_servers.two]\ncommand = \"two\"\n",
        )
        .unwrap();

        let status = access_status(&home, &state);

        assert_eq!(status.mcp_server_count, 2);
        assert_eq!(status.plugin_marketplace_status, "missing");
    }

    #[test]
    fn refuses_restore_when_baseline_is_tampered() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join(".mirrorplus");
        let settings = state.join("settings.json");
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join("config.toml"), "model_provider = \"openai\"\n").unwrap();
        let discovery = parse_model_discovery(&json!({"data": [{"id": "gpt-5.5"}]})).unwrap();
        enable_access(
            &home,
            &state,
            &settings,
            "sk-test",
            MirrorAccessMode::PureApi,
            discovery,
        )
        .unwrap();
        fs::write(state.join(BASELINE_DIR).join("config.toml"), "tampered").unwrap();
        assert!(restore_access(&home, &state, &settings).is_err());
        assert_eq!(
            read_provider(&home.join("config.toml")),
            MIRROR_CODEX_PROVIDER_ID
        );
        let status = access_status(&home, &state);
        assert_eq!(status.phase, "state_unreadable");
        assert!(status.baseline_exists);
        assert!(status.last_message.contains("baseline"));
    }

    #[test]
    fn enable_and_restore_preserve_unknown_config_and_file_existence() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join(".mirrorplus");
        let settings = state.join("settings.json");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("config.toml"),
            concat!(
                "model_provider = \"openai\"\n",
                "sandbox_mode = \"workspace-write\"\n",
                "approval_policy = \"on-request\"\n\n",
                "[windows]\n",
                "sandbox = \"unelevated\"\n\n",
                "[model_providers.custom]\n",
                "name = \"user custom\"\n",
                "base_url = \"https://user.example/v1\"\n",
                "wire_api = \"responses\"\n\n",
                "[mcp_servers.keep]\n",
                "command = \"keep\"\n",
            ),
        )
        .unwrap();
        let discovery = parse_model_discovery(&json!({
            "data": [{"id": "gpt-5.5", "context_window": 400000}]
        }))
        .unwrap();
        enable_access(
            &home,
            &state,
            &settings,
            "sk-test",
            MirrorAccessMode::PureApi,
            discovery,
        )
        .unwrap();
        let active = fs::read_to_string(home.join("config.toml")).unwrap();
        assert!(active.contains("model_provider = \"custom\""));
        assert!(active.contains("sandbox_mode = \"danger-full-access\""));
        assert!(active.contains("approval_policy = \"never\""));
        assert!(!active.contains("[windows]"));
        assert!(!active.contains("sandbox = \"unelevated\""));
        assert!(active.contains("[model_providers.custom]"));
        assert!(active.contains("base_url = \"https://api.jingziai.club/v1\""));
        assert!(!active.contains("base_url = \"https://user.example/v1\""));
        assert!(active.contains("[mcp_servers.keep]"));
        assert!(home.join("auth.json").exists());
        let managed = SettingsStore::new(settings.clone()).load().unwrap();
        assert!(managed.enhancements_enabled);
        assert!(managed.codex_app_plugin_marketplace_unlock);
        assert!(managed.codex_app_plugin_auto_expand);
        // 接入只负责 mirror 配置，不应重置用户已存在的 Codex 能力开关。
        assert!(managed.codex_app_session_delete);
        assert!(managed.codex_app_markdown_export);
        assert!(managed.codex_app_native_menu_placement);

        let restored = restore_access(&home, &state, &settings).unwrap();
        assert_eq!(restored.original_provider, "openai");
        assert_eq!(restored.status.phase, "restore_failed");
        assert_eq!(restored.status.session_sync_status, "pending_restore");
        let original = fs::read_to_string(home.join("config.toml")).unwrap();
        assert!(original.contains("sandbox_mode = \"workspace-write\""));
        assert!(original.contains("approval_policy = \"on-request\""));
        assert!(original.contains("[windows]"));
        assert!(original.contains("sandbox = \"unelevated\""));
        assert!(!original.contains("mirrorplus"));
        assert!(original.contains("[model_providers.custom]"));
        assert!(original.contains("name = \"user custom\""));
        assert!(original.contains("base_url = \"https://user.example/v1\""));
        assert!(!home.join("auth.json").exists());
        assert!(!settings.exists());
    }

    #[test]
    fn restore_preserves_preexisting_custom_and_mirrorplus_providers() {
        for original_provider in [MIRROR_CODEX_PROVIDER_ID, MIRROR_PROVIDER_ID] {
            let temp = tempfile::tempdir().unwrap();
            let home = temp.path().join(".codex");
            let state = temp.path().join(".mirrorplus");
            let settings = state.join("settings.json");
            fs::create_dir_all(&home).unwrap();
            fs::write(
                home.join("config.toml"),
                format!(
                    concat!(
                        "model_provider = \"{}\"\n\n",
                        "[model_providers.custom]\n",
                        "name = \"original custom\"\n",
                        "base_url = \"https://custom.example/v1\"\n",
                        "wire_api = \"responses\"\n\n",
                        "[model_providers.mirrorplus]\n",
                        "name = \"original mirrorplus\"\n",
                        "base_url = \"https://mirrorplus.example/v1\"\n",
                        "wire_api = \"responses\"\n",
                    ),
                    original_provider
                ),
            )
            .unwrap();
            let discovery = parse_model_discovery(&json!({"data": [{"id": "gpt-5.5"}]})).unwrap();

            enable_access(
                &home,
                &state,
                &settings,
                "sk-managed",
                MirrorAccessMode::PureApi,
                discovery,
            )
            .unwrap();

            let active = fs::read_to_string(home.join("config.toml"))
                .unwrap()
                .parse::<DocumentMut>()
                .unwrap();
            assert_eq!(
                active.get("model_provider").and_then(Item::as_str),
                Some(MIRROR_CODEX_PROVIDER_ID)
            );
            let active_providers = active["model_providers"].as_table().unwrap();
            assert!(active_providers.contains_key(MIRROR_CODEX_PROVIDER_ID));
            assert!(active_providers.contains_key(MIRROR_PROVIDER_ID));

            let restored = restore_access(&home, &state, &settings).unwrap();
            assert!(!restored.status.active);
            let restored_config = fs::read_to_string(home.join("config.toml"))
                .unwrap()
                .parse::<DocumentMut>()
                .unwrap();
            assert_eq!(
                restored_config.get("model_provider").and_then(Item::as_str),
                Some(original_provider)
            );
            let restored_providers = restored_config["model_providers"].as_table().unwrap();
            assert_eq!(
                restored_providers[MIRROR_CODEX_PROVIDER_ID]
                    .as_table()
                    .and_then(|provider| provider.get("base_url"))
                    .and_then(Item::as_str),
                Some("https://custom.example/v1")
            );
            assert_eq!(
                restored_providers[MIRROR_PROVIDER_ID]
                    .as_table()
                    .and_then(|provider| provider.get("base_url"))
                    .and_then(Item::as_str),
                Some("https://mirrorplus.example/v1")
            );
        }
    }

    #[test]
    fn restore_preserves_unmanaged_user_changes_and_restores_managed_baseline_keys() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join(".mirrorplus");
        let settings_path = state.join("settings.json");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&state).unwrap();
        fs::write(
            home.join("config.toml"),
            concat!(
                "model_provider = \"openai\"\n",
                "model = \"gpt-original\"\n",
                "approval_policy = \"on-request\"\n\n",
                "[model_providers.openai]\n",
                "name = \"OpenAI\"\n",
                "wire_api = \"responses\"\n\n",
                "[mcp_servers.before]\n",
                "command = \"before\"\n",
            ),
        )
        .unwrap();
        fs::write(
            home.join("auth.json"),
            serde_json::to_vec_pretty(&json!({
                "OPENAI_API_KEY": "sk-original",
                "tokens": { "access": "old" }
            }))
            .unwrap(),
        )
        .unwrap();
        let mut baseline_settings = serde_json::to_value(BackendSettings::default()).unwrap();
        baseline_settings["futureBaseline"] = json!({ "keep": true });
        fs::write(
            &settings_path,
            serde_json::to_vec_pretty(&baseline_settings).unwrap(),
        )
        .unwrap();

        let discovery = parse_model_discovery(&json!({"data": [{"id": "gpt-5.5"}]})).unwrap();
        enable_access(
            &home,
            &state,
            &settings_path,
            "sk-managed",
            MirrorAccessMode::PureApi,
            discovery,
        )
        .unwrap();

        let mut current_config = fs::read_to_string(home.join("config.toml")).unwrap();
        current_config = current_config.replace(
            "approval_policy = \"on-request\"",
            "approval_policy = \"never\"",
        );
        current_config.push_str(concat!(
            "\n[mcp_servers.after]\n",
            "command = \"after\"\n\n",
            "[model_providers.user]\n",
            "name = \"User provider\"\n",
            "wire_api = \"responses\"\n\n",
            "[marketplaces.user]\n",
            "source_type = \"local\"\n",
            "source = \"C:/user/plugins\"\n",
        ));
        fs::write(home.join("config.toml"), current_config).unwrap();

        fs::write(
            home.join("auth.json"),
            serde_json::to_vec_pretty(&json!({
                "OPENAI_API_KEY": "sk-managed",
                "tokens": { "access": "new" },
                "futureAuth": true
            }))
            .unwrap(),
        )
        .unwrap();

        let mut current_settings: Value =
            serde_json::from_slice(&fs::read(&settings_path).unwrap()).unwrap();
        current_settings["futureAfter"] = json!(["keep"]);
        current_settings["codexAppSessionDelete"] = Value::Bool(false);
        current_settings["relayProfiles"]
            .as_array_mut()
            .unwrap()
            .push(
                serde_json::to_value(RelayProfile {
                    id: "user-after".to_string(),
                    name: "User after".to_string(),
                    model: "user-model".to_string(),
                    model_list: "user-model".to_string(),
                    ..RelayProfile::default()
                })
                .unwrap(),
            );
        fs::write(
            &settings_path,
            serde_json::to_vec_pretty(&current_settings).unwrap(),
        )
        .unwrap();

        restore_access(&home, &state, &settings_path).unwrap();

        let config = fs::read_to_string(home.join("config.toml")).unwrap();
        assert!(config.contains("model_provider = \"openai\""));
        assert!(config.contains("model = \"gpt-original\""));
        assert!(config.contains("approval_policy = \"on-request\""));
        assert!(!config.contains("approval_policy = \"never\""));
        assert!(config.contains("[mcp_servers.before]"));
        assert!(config.contains("[mcp_servers.after]"));
        assert!(config.contains("[model_providers.user]"));
        assert!(config.contains("[marketplaces.user]"));
        assert!(!config.contains("[model_providers.mirrorplus]"));
        assert!(!config.contains("model_catalog_json"));
        assert!(!config.contains("forced_login_method"));

        let auth: Value =
            serde_json::from_slice(&fs::read(home.join("auth.json")).unwrap()).unwrap();
        assert_eq!(auth["OPENAI_API_KEY"], "sk-original");
        assert_eq!(auth["tokens"]["access"], "new");
        assert_eq!(auth["futureAuth"], true);

        let settings: Value = serde_json::from_slice(&fs::read(&settings_path).unwrap()).unwrap();
        assert_eq!(settings["futureBaseline"], json!({ "keep": true }));
        assert_eq!(settings["futureAfter"], json!(["keep"]));
        assert_eq!(settings["codexAppSessionDelete"], false);
        assert_eq!(settings["activeRelayId"], "default");
        let relay_ids = settings["relayProfiles"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|profile| profile.get("id").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert!(relay_ids.contains(&"user-after"));
        assert!(!relay_ids.iter().any(|id| *id == MIRROR_PROVIDER_ID));
        assert!(!relay_ids.iter().any(|id| id.starts_with("mirrorplus-")));
    }

    #[test]
    fn baseline_is_immutable_across_mode_switches() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join(".mirrorplus");
        let settings = state.join("settings.json");
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join("config.toml"), "model_provider = \"openai\"\n").unwrap();
        let discovery = parse_model_discovery(&json!({"data": [{"id": "gpt-5.5"}]})).unwrap();
        enable_access(
            &home,
            &state,
            &settings,
            "sk-first",
            MirrorAccessMode::MixedApi,
            discovery.clone(),
        )
        .unwrap();
        enable_access(
            &home,
            &state,
            &settings,
            "sk-second",
            MirrorAccessMode::PureApi,
            discovery,
        )
        .unwrap();
        let manifest = load_baseline_manifest(&state).unwrap();
        assert_eq!(manifest.original_provider, "openai");
        restore_access(&home, &state, &settings).unwrap();
        assert_eq!(read_provider(&home.join("config.toml")), "openai");
    }

    #[test]
    fn completed_restore_refreshes_baseline_before_the_next_access_cycle() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join(".mirrorplus");
        let settings = state.join("settings.json");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("config.toml"),
            "model_provider = \"openai\"\nold_setting = true\n",
        )
        .unwrap();
        fs::write(home.join("auth.json"), br#"{"OPENAI_API_KEY":"sk-old"}"#).unwrap();
        let discovery = parse_model_discovery(&json!({"data": [{"id": "gpt-5.5"}]})).unwrap();

        enable_access(
            &home,
            &state,
            &settings,
            "sk-first",
            MirrorAccessMode::PureApi,
            discovery.clone(),
        )
        .unwrap();
        restore_access(&home, &state, &settings).unwrap();
        record_session_sync(&home, &state, true, "restore complete").unwrap();

        let next_config = "model_provider = \"user-next\"\nnext_setting = true\n";
        let next_auth = br#"{"auth_mode":"chatgpt","tokens":{"access_token":"next"}}"#;
        fs::write(home.join("config.toml"), next_config).unwrap();
        fs::write(home.join("auth.json"), next_auth).unwrap();

        enable_access(
            &home,
            &state,
            &settings,
            "sk-second",
            MirrorAccessMode::MixedApi,
            discovery,
        )
        .unwrap();
        let refreshed = load_baseline_manifest(&state).unwrap();
        assert_eq!(refreshed.original_provider, "user-next");

        restore_access(&home, &state, &settings).unwrap();
        assert_eq!(
            fs::read_to_string(home.join("config.toml")).unwrap(),
            next_config
        );
        assert_eq!(fs::read(home.join("auth.json")).unwrap(), next_auth);
    }

    #[test]
    fn completed_restore_allows_a_new_access_cycle_in_another_codex_home() {
        let temp = tempfile::tempdir().unwrap();
        let home_a = temp.path().join("codex-a");
        let home_b = temp.path().join("codex-b");
        let state = temp.path().join(".mirrorplus");
        let settings = state.join("settings.json");
        fs::create_dir_all(&home_a).unwrap();
        fs::create_dir_all(&home_b).unwrap();
        fs::write(home_a.join("config.toml"), "model_provider = \"openai\"\n").unwrap();
        let discovery = parse_model_discovery(&json!({"data": [{"id": "gpt-5.5"}]})).unwrap();

        enable_access(
            &home_a,
            &state,
            &settings,
            "sk-first",
            MirrorAccessMode::PureApi,
            discovery.clone(),
        )
        .unwrap();
        restore_access(&home_a, &state, &settings).unwrap();
        record_session_sync(&home_a, &state, true, "restore complete").unwrap();

        let home_b_config = b"model_provider = \"user-b\"\n";
        fs::write(home_b.join("config.toml"), home_b_config).unwrap();
        assert_eq!(
            try_access_status(&home_b, &state).unwrap().phase,
            "unmanaged"
        );

        enable_access(
            &home_b,
            &state,
            &settings,
            "sk-second",
            MirrorAccessMode::PureApi,
            discovery,
        )
        .unwrap();
        let refreshed = load_baseline_manifest(&state).unwrap();
        assert_eq!(
            refreshed.codex_home.as_deref(),
            Some(
                crate::codex_home::codex_home_identity(&home_b)
                    .unwrap()
                    .as_str()
            )
        );

        restore_access(&home_b, &state, &settings).unwrap();
        assert_eq!(fs::read(home_b.join("config.toml")).unwrap(), home_b_config);
    }

    #[test]
    fn updating_one_group_preserves_other_groups_and_user_feature_flags() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join(".mirrorplus");
        let settings_path = state.join("settings.json");
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join("config.toml"), "model_provider = \"openai\"\n").unwrap();

        let custom = SettingsStore::new(settings_path.clone());
        let mut settings = custom.load().unwrap();
        settings.codex_app_session_delete = false;
        settings.codex_app_native_menu_placement = false;
        settings.relay_profiles.push(RelayProfile {
            id: "user-profile".to_string(),
            name: "用户自定义".to_string(),
            api_key: "sk-user".to_string(),
            model: "user-model".to_string(),
            model_list: "user-model".to_string(),
            config_contents: "model_provider = \"user-provider\"\n[model_providers.user-provider]\nexperimental_bearer_token = \"sk-user\"\n".to_string(),
            ..RelayProfile::default()
        });
        custom.save(&settings).unwrap();

        let codexpro = parse_model_discovery(&json!({
            "data": [{"id": "gpt-5.4"}, {"id": "grok-4.1"}]
        }))
        .unwrap();
        let claude = parse_model_discovery(&json!({"data": [{"id": "claude-opus-4-6"}]})).unwrap();
        enable_grouped_access(
            &home,
            &state,
            &settings_path,
            MirrorAccessMode::MixedApi,
            vec![
                MirrorAccessGroup {
                    id: "codexpro".to_string(),
                    label: "CodexPro".to_string(),
                    api_key: "sk-codexpro".to_string(),
                    discovery: codexpro,
                },
                MirrorAccessGroup {
                    id: "claude".to_string(),
                    label: "Claude".to_string(),
                    api_key: "sk-claude".to_string(),
                    discovery: claude.clone(),
                },
            ],
            "gpt-5.4",
        )
        .unwrap();

        // 只提交 Claude 的更新，CodexPro 与用户自定义 profile 都必须继续存在。
        let updated_access = enable_grouped_access(
            &home,
            &state,
            &settings_path,
            MirrorAccessMode::MixedApi,
            vec![MirrorAccessGroup {
                id: "claude".to_string(),
                label: "Claude".to_string(),
                api_key: "sk-claude-new".to_string(),
                discovery: claude,
            }],
            // 默认模型仍可保留在本次未提交、但已存在的 CodexPro 分组。
            "gpt-5.4",
        )
        .unwrap();
        assert_eq!(updated_access.status.default_model, "gpt-5.4");

        let updated = SettingsStore::new(settings_path.clone()).load().unwrap();
        assert!(!updated.codex_app_session_delete);
        assert!(!updated.codex_app_native_menu_placement);
        assert!(
            updated
                .relay_profiles
                .iter()
                .any(|profile| profile.id == "user-profile")
        );
        assert!(
            updated
                .relay_profiles
                .iter()
                .any(|profile| profile.id == "mirrorplus-codexpro"
                    && profile.api_key == "sk-codexpro")
        );
        assert!(
            updated
                .relay_profiles
                .iter()
                .any(|profile| profile.id == "mirrorplus-claude"
                    && profile.api_key == "sk-claude-new")
        );

        let enterprise = parse_model_discovery(&json!({"data": [{"id": "gpt-5.5"}]})).unwrap();
        enable_grouped_access(
            &home,
            &state,
            &settings_path,
            MirrorAccessMode::MixedApi,
            vec![MirrorAccessGroup {
                id: "enterprise".to_string(),
                label: "企业GPT专线（极稳）".to_string(),
                api_key: "sk-enterprise".to_string(),
                discovery: enterprise,
            }],
            "gpt-5.5",
        )
        .unwrap();
        let migrated = SettingsStore::new(settings_path).load().unwrap();
        assert!(
            migrated
                .relay_profiles
                .iter()
                .any(|profile| profile.id == "mirrorplus-enterprise"
                    && profile.api_key == "sk-enterprise")
        );
        assert!(
            !migrated
                .relay_profiles
                .iter()
                .any(|profile| profile.id == "mirrorplus-claude")
        );
    }

    #[test]
    fn quick_setup_replaces_omitted_managed_groups_but_keeps_user_profiles() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join(".mirrorplus");
        let settings_path = state.join("settings.json");
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join("config.toml"), "model_provider = \"openai\"\n").unwrap();

        let store = SettingsStore::new(settings_path.clone());
        let mut initial = store.load().unwrap();
        initial.relay_profiles.push(RelayProfile {
            id: "user-profile".to_string(),
            name: "User profile".to_string(),
            model: "user-model".to_string(),
            model_list: "user-model".to_string(),
            ..RelayProfile::default()
        });
        store.save(&initial).unwrap();

        let codexpro = parse_model_discovery(&json!({"data": [{"id": "gpt-5.4"}]})).unwrap();
        let enterprise = parse_model_discovery(&json!({"data": [{"id": "gpt-5.5"}]})).unwrap();
        enable_grouped_access(
            &home,
            &state,
            &settings_path,
            MirrorAccessMode::PureApi,
            vec![
                MirrorAccessGroup {
                    id: "codexpro".to_string(),
                    label: "CodexPro".to_string(),
                    api_key: "sk-codexpro".to_string(),
                    discovery: codexpro,
                },
                MirrorAccessGroup {
                    id: "enterprise".to_string(),
                    label: "Enterprise".to_string(),
                    api_key: "sk-enterprise".to_string(),
                    discovery: enterprise.clone(),
                },
            ],
            "gpt-5.4",
        )
        .unwrap();

        enable_grouped_access_transaction_replacing_groups(
            &home,
            &state,
            &settings_path,
            MirrorAccessMode::PureApi,
            vec![MirrorAccessGroup {
                id: "enterprise".to_string(),
                label: "Enterprise".to_string(),
                api_key: "sk-enterprise-new".to_string(),
                discovery: enterprise,
            }],
            "gpt-5.5",
        )
        .unwrap();

        let updated = SettingsStore::new(settings_path).load().unwrap();
        assert!(
            updated
                .relay_profiles
                .iter()
                .any(|profile| profile.id == "user-profile")
        );
        assert!(
            !updated
                .relay_profiles
                .iter()
                .any(|profile| profile.id == "mirrorplus-codexpro")
        );
        assert!(updated.relay_profiles.iter().any(|profile| {
            profile.id == "mirrorplus-enterprise" && profile.api_key == "sk-enterprise-new"
        }));
    }

    #[test]
    fn invalid_manager_settings_are_never_replaced_with_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join(".mirrorplus");
        let settings_path = state.join("settings.json");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&state).unwrap();
        fs::write(home.join("config.toml"), "model_provider = \"openai\"\n").unwrap();
        fs::write(&settings_path, b"{broken-settings").unwrap();
        let discovery = parse_model_discovery(&json!({"data": [{"id": "gpt-5.4"}]})).unwrap();

        let error = enable_access(
            &home,
            &state,
            &settings_path,
            "sk-test",
            MirrorAccessMode::MixedApi,
            discovery,
        )
        .unwrap_err();

        assert!(error.to_string().contains("不是有效 JSON"));
        assert_eq!(fs::read(&settings_path).unwrap(), b"{broken-settings");
        assert_eq!(read_provider(&home.join("config.toml")), "openai");
    }

    #[test]
    fn invalid_managed_state_is_reported_without_becoming_unmanaged() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join(".mirrorplus");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&state).unwrap();
        fs::write(
            home.join("config.toml"),
            "model_provider = \"mirrorplus\"\n",
        )
        .unwrap();
        let state_path = state.join(MANAGED_STATE_FILE);
        let invalid = b"{broken-managed-state";
        fs::write(&state_path, invalid).unwrap();

        let error = try_access_status(&home, &state).unwrap_err();
        assert!(error.to_string().contains("不是有效 JSON"));
        let status = access_status(&home, &state);
        assert_eq!(status.phase, "state_unreadable");
        assert_eq!(status.session_sync_status, "state_unreadable");
        assert!(!status.active);
        assert_eq!(status.mode, None);
        assert_eq!(status.current_provider, MIRROR_PROVIDER_ID);
        assert!(status.last_message.contains("已停止所有配置和会话修改"));
        assert_eq!(fs::read(&state_path).unwrap(), invalid);
    }

    #[test]
    fn managed_state_directory_is_not_treated_as_missing() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join(".mirrorplus");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(state.join(MANAGED_STATE_FILE)).unwrap();

        assert!(try_access_status(&home, &state).is_err());
        assert_eq!(access_status(&home, &state).phase, "state_unreadable");
        assert!(state.join(MANAGED_STATE_FILE).is_dir());
    }

    #[test]
    fn invalid_managed_state_blocks_pure_and_mixed_enable_before_writes() {
        for mode in [MirrorAccessMode::PureApi, MirrorAccessMode::MixedApi] {
            let temp = tempfile::tempdir().unwrap();
            let home = temp.path().join(".codex");
            let state = temp.path().join(".mirrorplus");
            let settings_path = state.join("settings.json");
            fs::create_dir_all(&home).unwrap();
            fs::create_dir_all(&state).unwrap();
            let config = b"model_provider = \"openai\"\n";
            let auth = br#"{"auth_mode":"chatgpt","tokens":{"access_token":"official"}}"#;
            fs::write(home.join("config.toml"), config).unwrap();
            fs::write(home.join("auth.json"), auth).unwrap();
            let state_path = state.join(MANAGED_STATE_FILE);
            let invalid = b"{broken-managed-state";
            fs::write(&state_path, invalid).unwrap();
            let discovery = parse_model_discovery(&json!({"data": [{"id": "gpt-5.4"}]})).unwrap();

            let error = enable_access(&home, &state, &settings_path, "sk-test", mode, discovery)
                .unwrap_err();

            assert!(error.to_string().contains("不是有效 JSON"), "{error:#}");
            assert_eq!(fs::read(home.join("config.toml")).unwrap(), config);
            assert_eq!(fs::read(home.join("auth.json")).unwrap(), auth);
            assert_eq!(fs::read(&state_path).unwrap(), invalid);
            assert!(!settings_path.exists());
            assert!(!home.join(CATALOG_FILE).exists());
            assert!(!state.join(BASELINE_DIR).exists());
        }
    }

    #[test]
    fn invalid_managed_state_blocks_restore_and_session_recording_without_overwrite() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join(".mirrorplus");
        let settings_path = state.join("settings.json");
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join("config.toml"), "model_provider = \"openai\"\n").unwrap();
        fs::write(
            home.join("auth.json"),
            br#"{"auth_mode":"chatgpt","tokens":{"access_token":"official"}}"#,
        )
        .unwrap();
        let discovery = parse_model_discovery(&json!({"data": [{"id": "gpt-5.4"}]})).unwrap();
        enable_access(
            &home,
            &state,
            &settings_path,
            "sk-pure",
            MirrorAccessMode::PureApi,
            discovery,
        )
        .unwrap();

        let protected_paths = [
            home.join("config.toml"),
            home.join("auth.json"),
            settings_path.clone(),
            home.join(CATALOG_FILE),
        ];
        let protected_bytes = protected_paths
            .iter()
            .map(|path| fs::read(path).unwrap())
            .collect::<Vec<_>>();
        let state_path = state.join(MANAGED_STATE_FILE);
        let invalid = b"{broken-managed-state";
        fs::write(&state_path, invalid).unwrap();

        let restore_error = restore_access(&home, &state, &settings_path).unwrap_err();
        assert!(
            restore_error.to_string().contains("不是有效 JSON"),
            "{restore_error:#}"
        );
        let sync_error = record_session_sync(&home, &state, true, "synced").unwrap_err();
        assert!(
            sync_error.to_string().contains("不是有效 JSON"),
            "{sync_error:#}"
        );
        assert_eq!(fs::read(&state_path).unwrap(), invalid);
        for (path, expected) in protected_paths.iter().zip(protected_bytes) {
            assert_eq!(fs::read(path).unwrap(), expected, "{}", path.display());
        }
    }

    #[test]
    fn explicit_baseline_recovery_handles_corrupt_state_config_auth_and_settings() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".codex");
        let state = temp.path().join(".mirrorplus");
        let settings_path = state.join("settings.json");
        fs::create_dir_all(&home).unwrap();
        let original_config = b"model_provider = \"openai\"\napproval_policy = \"never\"\n";
        let original_auth = br#"{"auth_mode":"chatgpt","tokens":{"access_token":"official"}}"#;
        fs::write(home.join("config.toml"), original_config).unwrap();
        fs::write(home.join("auth.json"), original_auth).unwrap();
        let discovery = parse_model_discovery(&json!({"data": [{"id": "gpt-5.4"}]})).unwrap();
        enable_access(
            &home,
            &state,
            &settings_path,
            "sk-pure",
            MirrorAccessMode::PureApi,
            discovery,
        )
        .unwrap();

        fs::write(state.join(MANAGED_STATE_FILE), b"{broken-state").unwrap();
        fs::write(home.join("config.toml"), b"not = [valid").unwrap();
        fs::write(home.join("auth.json"), b"{broken-auth").unwrap();
        fs::write(&settings_path, b"{broken-settings").unwrap();

        assert!(restore_access(&home, &state, &settings_path).is_err());
        let recovered = recover_access_from_baseline(&home, &state, &settings_path).unwrap();

        assert_eq!(recovered.original_provider, "openai");
        assert_eq!(fs::read(home.join("config.toml")).unwrap(), original_config);
        assert_eq!(fs::read(home.join("auth.json")).unwrap(), original_auth);
        assert!(!settings_path.exists());
        assert_eq!(
            load_managed_state(&state).unwrap().session_sync_status,
            "pending_restore"
        );
    }

    #[test]
    fn baseline_restore_refuses_a_different_codex_home_without_writes() {
        let temp = tempfile::tempdir().unwrap();
        let home_a = temp.path().join("codex-a");
        let home_b = temp.path().join("codex-b");
        let state = temp.path().join(".mirrorplus");
        let settings_path = state.join("settings.json");
        fs::create_dir_all(&home_a).unwrap();
        fs::create_dir_all(&home_b).unwrap();
        fs::create_dir_all(&state).unwrap();
        fs::write(home_a.join("config.toml"), "model_provider = \"openai\"\n").unwrap();
        let home_b_config = b"model_provider = \"user-b\"\n";
        let home_b_auth = br#"{"OPENAI_API_KEY":"keep-b"}"#;
        fs::write(home_b.join("config.toml"), home_b_config).unwrap();
        fs::write(home_b.join("auth.json"), home_b_auth).unwrap();
        ensure_baseline(&home_a, &state, &settings_path).unwrap();

        let error = recover_access_from_baseline(&home_b, &state, &settings_path).unwrap_err();

        assert!(error.to_string().contains("CODEX_HOME"), "{error:#}");
        assert_eq!(fs::read(home_b.join("config.toml")).unwrap(), home_b_config);
        assert_eq!(fs::read(home_b.join("auth.json")).unwrap(), home_b_auth);
        assert!(!state.join(MANAGED_STATE_FILE).exists());
    }
}
