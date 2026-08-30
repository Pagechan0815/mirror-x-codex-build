use std::collections::BTreeMap;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::fs::OpenOptions;
use std::hash::{Hash, Hasher};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use codex_plus_core::install::SILENT_BINARY;
use codex_plus_core::models::{DeleteResult, SessionRef};
use codex_plus_core::script_market::{self, MarketScript, ScriptMarketManifest};
use codex_plus_core::settings::{BackendSettings, RelayProfile, SettingsStore};
use codex_plus_core::status::{LaunchStatus, StatusStore};
use codex_plus_core::user_scripts::UserScriptManager;
use codex_plus_core::zed_remote::{ZedOpenStrategy, ZedRemoteProject};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::Mutex as AsyncMutex;

use crate::install::{self, InstallActionResult, InstallOptions};

#[derive(Debug, Clone, Serialize)]
pub struct CommandResult<T>
where
    T: Serialize,
{
    pub status: String,
    pub message: String,
    #[serde(flatten)]
    pub payload: T,
}

#[derive(Debug, Clone, Serialize)]
pub struct VersionPayload {
    pub version: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PathState {
    pub status: String,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OverviewPayload {
    pub codex_app: PathState,
    pub codex_version: Option<String>,
    pub silent_shortcut: PathState,
    pub management_shortcut: PathState,
    pub latest_launch: Option<LaunchStatus>,
    pub current_version: String,
    pub update_status: String,
    pub settings_path: String,
    pub logs_path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SettingsPayload {
    pub settings: BackendSettings,
    pub settings_path: String,
    pub user_scripts: Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginMarketplaceRepairPayload {
    pub codex_home: String,
    pub marketplace_root: Option<String>,
    pub initialized: bool,
    pub configured: bool,
    pub needs_repair: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginMarketplaceStatusPayload {
    pub codex_home: String,
    pub marketplace_root: Option<String>,
    pub config_registered: bool,
    pub needs_repair: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePluginMarketplacePayload {
    pub codex_home: String,
    pub marketplace_root: Option<String>,
    pub config_registered: bool,
    pub needs_repair: bool,
    pub plugin_count: usize,
    pub skill_count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CcsProvidersPayload {
    pub db_path: String,
    pub providers: Vec<codex_plus_core::ccs_import::CcsProviderImport>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingProviderImportPayload {
    pub pending: Option<codex_plus_core::provider_import::ProviderImportRequest>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalSessionsPayload {
    pub db_path: String,
    pub db_paths: Vec<String>,
    pub sessions: Vec<codex_plus_data::LocalSession>,
    pub offset: usize,
    pub limit: usize,
    pub has_more: bool,
}

const DEFAULT_LOCAL_SESSIONS_PAGE_SIZE: usize = 50;
const MAX_LOCAL_SESSIONS_PAGE_SIZE: usize = 100;

fn default_local_sessions_page_size() -> usize {
    DEFAULT_LOCAL_SESSIONS_PAGE_SIZE
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListLocalSessionsRequest {
    #[serde(default)]
    pub offset: usize,
    #[serde(default = "default_local_sessions_page_size")]
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ZedRemoteProjectsPayload {
    pub projects: Vec<ZedRemoteProject>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ZedRemoteOpenPayload {
    pub url: String,
    pub strategy: ZedOpenStrategy,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteLocalSessionRequest {
    pub session_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub db_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayPayload {
    pub authenticated: bool,
    pub auth_source: String,
    pub account_label: Option<String>,
    pub config_path: String,
    pub configured: bool,
    pub requires_openai_auth: bool,
    pub has_bearer_token: bool,
    pub state_unreadable: bool,
    pub state_error: Option<String>,
    pub backup_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayFilesPayload {
    pub config_path: String,
    pub auth_path: String,
    pub config_contents: String,
    pub auth_contents: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelaySwitchPayload {
    pub settings: BackendSettings,
    pub relay: RelayPayload,
    pub settings_path: String,
    pub user_scripts: Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsBackfillPayload {
    pub settings: BackendSettings,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextEntriesPayload {
    pub settings: BackendSettings,
    pub entries: codex_plus_core::relay_config::CodexContextEntries,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveContextEntriesPayload {
    pub entries: codex_plus_core::relay_config::CodexContextEntries,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractRelayCommonConfigPayload {
    pub common_config_contents: String,
    pub profile_config_contents: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayProfileTestPayload {
    pub http_status: u16,
    pub endpoint: String,
    pub response_preview: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StepwiseTestPayload {
    pub item_count: usize,
    pub error: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayProfileModelsPayload {
    pub models: Vec<String>,
    pub endpoint: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderDoctorCheck {
    pub id: String,
    pub title: String,
    pub status: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderDoctorPayload {
    pub profile_name: String,
    pub model: String,
    pub summary: String,
    pub recommendation: String,
    pub checks: Vec<ProviderDoctorCheck>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvConflictsPayload {
    pub conflicts: Vec<codex_plus_core::env_conflicts::EnvConflict>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveEnvConflictsRequest {
    pub names: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveEnvConflictsPayload {
    pub removed: Vec<codex_plus_core::env_conflicts::EnvConflictRemoval>,
    pub backup_path: Option<String>,
    pub remaining: Vec<codex_plus_core::env_conflicts::EnvConflict>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveRelayFileRequest {
    pub kind: String,
    pub contents: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackfillRelayProfileRequest {
    pub settings: BackendSettings,
    pub profile_id: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextSettingsRequest {
    pub settings: BackendSettings,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextEntryRequest {
    pub settings: BackendSettings,
    pub kind: String,
    pub id: String,
    pub toml_body: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextDeleteRequest {
    pub settings: BackendSettings,
    pub kind: String,
    pub id: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractRelayCommonConfigRequest {
    pub config_contents: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchRequest {
    #[serde(default)]
    pub app_path: String,
    #[serde(default = "default_debug_port")]
    pub debug_port: u16,
    #[serde(default = "default_helper_port")]
    pub helper_port: u16,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogRequest {
    #[serde(default = "default_log_lines")]
    pub lines: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogsPayload {
    pub path: String,
    pub text: String,
    pub lines: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticsPayload {
    pub report: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WatcherPayload {
    pub enabled: bool,
    pub disabled_flag: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdsPayload {
    pub version: u64,
    pub ads: Vec<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScriptMarketPayload {
    pub market: Value,
    pub user_scripts: Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartupPayload {
    pub show_update: bool,
}

#[tauri::command]
pub fn backend_version() -> CommandResult<VersionPayload> {
    ok(
        "后端版本已读取。",
        VersionPayload {
            version: codex_plus_core::version::VERSION.to_string(),
        },
    )
}

#[tauri::command]
pub fn startup_options() -> CommandResult<StartupPayload> {
    ok(
        "启动参数已读取。",
        StartupPayload {
            show_update: startup_should_show_update(),
        },
    )
}

pub fn startup_should_show_update() -> bool {
    should_show_update(
        std::env::args(),
        std::env::var("CODEX_PLUS_SHOW_UPDATE").ok().as_deref(),
    )
}

fn should_show_update<I, S>(args: I, env_value: Option<&str>) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter().any(|arg| arg.as_ref() == "--show-update") || env_value == Some("1")
}

#[tauri::command]
pub async fn load_overview() -> CommandResult<OverviewPayload> {
    let payload = tauri::async_runtime::spawn_blocking(load_overview_payload).await;
    let Ok((codex_app_path, entrypoints, latest_launch)) = payload else {
        return failed(
            "概览后台任务失败。",
            OverviewPayload {
                codex_app: path_state(None),
                codex_version: None,
                silent_shortcut: path_state(None),
                management_shortcut: path_state(None),
                latest_launch: None,
                current_version: codex_plus_core::version::VERSION.to_string(),
                update_status: "not_checked".to_string(),
                settings_path: codex_plus_core::paths::default_settings_path()
                    .to_string_lossy()
                    .to_string(),
                logs_path: codex_plus_core::paths::default_diagnostic_log_path()
                    .to_string_lossy()
                    .to_string(),
            },
        );
    };
    ok(
        "概览已加载。",
        OverviewPayload {
            codex_version: codex_app_path
                .as_deref()
                .and_then(codex_plus_core::app_paths::codex_app_version),
            codex_app: path_state(codex_app_path),
            silent_shortcut: shortcut_state(entrypoints.silent_shortcut),
            management_shortcut: shortcut_state(entrypoints.management_shortcut),
            latest_launch,
            current_version: codex_plus_core::version::VERSION.to_string(),
            update_status: "not_checked".to_string(),
            settings_path: codex_plus_core::paths::default_settings_path()
                .to_string_lossy()
                .to_string(),
            logs_path: codex_plus_core::paths::default_diagnostic_log_path()
                .to_string_lossy()
                .to_string(),
        },
    )
}

#[tauri::command]
pub async fn launch_codex_plus(request: LaunchRequest) -> CommandResult<Value> {
    let Ok(_guard) = launch_operation_mutex().try_lock() else {
        return degraded(
            "Codex 正在启动，本次点击不会重复启动；请等待现有窗口完成加载。",
            json!({ "launchStatus": StatusStore::default().load_latest().unwrap_or(None) }),
        );
    };
    spawn_codex_plus_launch(request).await
}

#[tauri::command]
pub async fn restart_codex_plus(request: LaunchRequest) -> CommandResult<Value> {
    let Ok(_guard) = launch_operation_mutex().try_lock() else {
        return failed(
            "已有 Codex 启动或重启操作正在进行，本次重复请求未执行。",
            json!({ "launchStatus": null }),
        );
    };
    if let Err(message) = validate_settings_before_launch("重启 Codex") {
        return failed(&message, json!({ "launchStatus": null }));
    }
    let prepared = tauri::async_runtime::spawn_blocking(|| -> Result<(), &'static str> {
        if !codex_plus_core::watcher::request_codex_shutdown_and_wait() {
            return Err(
                "Codex 未能在 15 秒内正常退出。为保护正在写入的会话，本工具没有停止当前增强服务、没有强制结束进程，也没有启动第二个实例；请在 Codex 中停止当前任务并正常退出后再重试。",
            );
        }
        if !codex_plus_core::watcher::wait_for_launcher_processes_to_exit() {
            return Err(
                "Codex 已正常退出，但旧启动服务仍在完成会话同步或资源清理。为保护会话，本工具没有强制结束该进程，也没有启动第二个实例；请稍后重试或从维护页查看诊断日志。",
            );
        }
        Ok(())
    })
    .await;
    match prepared {
        Ok(Ok(())) => {}
        Ok(Err(message)) => return failed(message, json!({ "launchStatus": null })),
        Err(error) => {
            return failed(
                &format!("重启准备任务异常结束，未启动第二个 Codex 实例：{error}。请稍后重试。"),
                json!({ "launchStatus": null }),
            );
        }
    }
    spawn_codex_plus_launch(request).await
}

async fn spawn_codex_plus_launch(mut request: LaunchRequest) -> CommandResult<Value> {
    if let Err(message) = validate_settings_before_launch("启动 Codex") {
        return failed(&message, json!({ "launchStatus": null }));
    }
    let requested_app_path = request.app_path.trim().to_string();
    if !requested_app_path.is_empty()
        && codex_plus_core::app_paths::normalize_codex_app_path(Path::new(&requested_app_path))
            .is_none()
    {
        let _ = codex_plus_core::diagnostic_log::append_diagnostic_log(
            "manager.launch_app_path_rejected",
            json!({ "app_path": requested_app_path }),
        );
        request.app_path = String::new();
    }
    if codex_plus_core::watcher::find_codex_processes().is_empty() {
        let launcher_ready = tauri::async_runtime::spawn_blocking(|| {
            codex_plus_core::watcher::wait_for_launcher_processes_to_exit()
        })
        .await;
        match launcher_ready {
            Ok(true) => {}
            Ok(false) => {
                return failed(
                    "上一次启动服务仍在清理本地路由，30 秒内未退出；本次没有启动第二个实例。请稍后重试。",
                    json!({ "launchStatus": null }),
                );
            }
            Err(error) => {
                return failed(
                    &format!(
                        "等待上一次启动服务清理时任务异常结束：{error}。本次没有启动第二个实例。"
                    ),
                    json!({ "launchStatus": null }),
                );
            }
        }
    }
    let debug_port = request.debug_port;
    let helper_port = request.helper_port;
    let requested_at_ms = launch_now_ms();
    let _ = codex_plus_core::diagnostic_log::append_diagnostic_log(
        "manager.launch_requested",
        json!({
            "debug_port": debug_port,
            "helper_port": helper_port,
            "app_path": request.app_path.trim()
        }),
    );
    let status_store = StatusStore::default();
    let starting = LaunchStatus {
        status: "starting".to_string(),
        message: "Codex 启动中，正在等待应用与本地 Helper 就绪。".to_string(),
        started_at_ms: requested_at_ms,
        debug_port: Some(debug_port),
        helper_port: Some(helper_port),
        codex_app: (!request.app_path.trim().is_empty())
            .then(|| request.app_path.trim().to_string()),
    };
    if let Err(error) = status_store.save_latest(&starting) {
        return failed(
            &format!("无法建立启动状态记录，未启动 Codex：{error}"),
            json!({ "launchStatus": starting }),
        );
    }
    match spawn_silent_launcher(&request) {
        Ok(child) => {
            wait_for_launch_readiness(
                child,
                &status_store,
                requested_at_ms,
                debug_port,
                helper_port,
            )
            .await
        }
        Err(error) => {
            let failure = LaunchStatus {
                status: "failed".to_string(),
                message: format!("启动静默入口失败：{error}"),
                started_at_ms: requested_at_ms,
                debug_port: Some(debug_port),
                helper_port: Some(helper_port),
                codex_app: starting.codex_app,
            };
            let _ = status_store.save_latest(&failure);
            failed(&failure.message, json!({ "launchStatus": failure }))
        }
    }
}

fn validate_settings_before_launch(action: &str) -> Result<(), String> {
    SettingsStore::default().load().map(|_| ()).map_err(|error| {
        format!(
            "{action}已停止：无法读取 Manager 设置：{error:#}。为避免使用默认值启动或改写配置，原 settings.json 保持不变。"
        )
    })
}

fn spawn_silent_launcher(request: &LaunchRequest) -> anyhow::Result<Child> {
    let launcher = codex_plus_core::install::companion_binary_path(SILENT_BINARY);
    let mut command = std::process::Command::new(&launcher);
    if !request.app_path.trim().is_empty() {
        command.arg("--app-path").arg(request.app_path.trim());
    }
    command
        .arg("--debug-port")
        .arg(request.debug_port.to_string())
        .arg("--helper-port")
        .arg(request.helper_port.to_string());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
        .spawn()
        .map_err(|error| anyhow::anyhow!("无法启动 {}：{error}", launcher.to_string_lossy()))
}

async fn wait_for_launch_readiness(
    mut child: Child,
    status_store: &StatusStore,
    requested_at_ms: u64,
    debug_port: u16,
    helper_port: u16,
) -> CommandResult<Value> {
    const LAUNCH_READY_TIMEOUT: Duration = Duration::from_secs(135);
    let deadline = Instant::now() + LAUNCH_READY_TIMEOUT;
    loop {
        if let Ok(Some(status)) = status_store.load_latest() {
            if status.started_at_ms >= requested_at_ms {
                match status.status.as_str() {
                    "running" => {
                        return ok(
                            "Codex 已启动，本地 Helper 与桌面增强已就绪。",
                            json!({ "launchStatus": status }),
                        );
                    }
                    "running_degraded" => {
                        return CommandResult {
                            status: "degraded".to_string(),
                            message: degraded_launch_message(&status).to_string(),
                            payload: json!({ "launchStatus": status }),
                        };
                    }
                    "failed" => {
                        return failed(
                            &format!("Codex 启动失败：{}", status.message),
                            json!({ "launchStatus": status }),
                        );
                    }
                    _ => {}
                }
            }
        }

        match child.try_wait() {
            Ok(Some(exit)) => {
                // Launcher 会先原子写入终态再退出；再读一次覆盖极窄的调度窗口。
                tokio::time::sleep(Duration::from_millis(100)).await;
                if let Ok(Some(status)) = status_store.load_latest() {
                    if status.started_at_ms >= requested_at_ms && status.status == "failed" {
                        return failed(
                            &format!("Codex 启动失败：{}", status.message),
                            json!({ "launchStatus": status }),
                        );
                    }
                }
                return failed(
                    &format!("Launcher 在 Codex 就绪前退出（{exit}），未确认启动成功。"),
                    json!({
                        "debugPort": debug_port,
                        "helperPort": helper_port,
                    }),
                );
            }
            Ok(None) => {}
            Err(error) => {
                return failed(
                    &format!("无法确认 Launcher 运行状态：{error}"),
                    json!({ "debugPort": debug_port, "helperPort": helper_port }),
                );
            }
        }

        if Instant::now() >= deadline {
            return failed(
                "Codex 启动确认超时；Launcher 仍在运行，但尚未确认应用和本地 Helper 就绪。",
                json!({ "debugPort": debug_port, "helperPort": helper_port }),
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn degraded_launch_message(status: &LaunchStatus) -> &'static str {
    if status.message.contains("Existing Codex window activated") {
        "检测到 Codex 正在启动或已经打开，已切换到现有窗口；本次没有启动第二个实例。"
    } else {
        "Codex 窗口已打开，本地路由可用；页面仍在加载桌面增强，请继续等待，无需再次点击启动。"
    }
}

fn launch_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[tauri::command]
pub fn load_settings() -> CommandResult<SettingsPayload> {
    settings_payload("设置已加载。", "设置读取失败")
}

#[tauri::command]
pub fn save_settings(settings: BackendSettings) -> CommandResult<SettingsPayload> {
    let settings = normalize_settings_before_save(settings);
    match SettingsStore::default().save(&settings) {
        Ok(()) => settings_payload("设置已保存。", "设置保存后重新读取失败"),
        Err(error) => failed(
            &format!("保存设置失败：{error}"),
            SettingsPayload {
                settings,
                settings_path: codex_plus_core::paths::default_settings_path()
                    .to_string_lossy()
                    .to_string(),
                user_scripts: user_script_inventory(),
            },
        ),
    }
}

#[tauri::command]
pub fn load_ccs_providers() -> CommandResult<CcsProvidersPayload> {
    let db_path = codex_plus_core::ccs_import::default_ccs_db_path();
    match codex_plus_core::ccs_import::list_codex_providers_from_db(&db_path) {
        Ok(providers) => ok(
            &format!(
                "已读取 cc-switch Codex 供应商配置：{} 个。",
                providers.len()
            ),
            CcsProvidersPayload {
                db_path: db_path.to_string_lossy().to_string(),
                providers,
            },
        ),
        Err(error) => failed(
            &format!("读取 cc-switch 供应商配置失败：{error}"),
            CcsProvidersPayload {
                db_path: db_path.to_string_lossy().to_string(),
                providers: Vec::new(),
            },
        ),
    }
}

#[tauri::command]
pub fn import_ccs_providers() -> CommandResult<SettingsPayload> {
    let providers = match codex_plus_core::ccs_import::list_codex_providers_from_default_db() {
        Ok(providers) => providers,
        Err(error) => {
            let payload = settings_payload_value().unwrap_or_else(|(_, payload)| payload);
            return failed(&format!("读取 cc-switch 供应商配置失败：{error}"), payload);
        }
    };

    let store = SettingsStore::default();
    let mut settings = match store.load() {
        Ok(settings) => settings,
        Err(error) => {
            return failed(
                &format!(
                    "导入 cc-switch 供应商已停止：无法读取 Manager 设置：{error:#}。原 settings.json 保持不变。"
                ),
                fallback_settings_payload(),
            );
        }
    };
    let mut existing_keys: Vec<String> = settings
        .relay_profiles
        .iter()
        .map(codex_plus_core::ccs_import::imported_provider_identity)
        .collect();
    let mut existing_ids: Vec<String> = settings
        .relay_profiles
        .iter()
        .map(|profile| profile.id.clone())
        .collect();
    let mut imported = 0usize;

    for provider in providers {
        let key = codex_plus_core::ccs_import::provider_identity_from_ccs(&provider);
        if existing_keys.iter().any(|existing| existing == &key) {
            continue;
        }
        let profile = codex_plus_core::ccs_import::relay_profile_from_ccs(&provider, &existing_ids);
        existing_ids.push(profile.id.clone());
        existing_keys.push(key);
        settings.relay_profiles.push(profile);
        imported += 1;
    }

    if imported == 0 {
        return settings_payload("没有新的 cc-switch 供应商配置需要导入。", "设置读取失败");
    }

    settings = normalize_settings_before_save(settings);
    match store.save(&settings) {
        Ok(()) => settings_payload(
            &format!("已从 cc-switch 导入供应商配置：{imported} 个。"),
            "导入供应商配置后重新读取设置失败",
        ),
        Err(error) => failed(
            &format!("保存 cc-switch 供应商配置失败：{error}"),
            settings_payload_value().unwrap_or_else(|(_, payload)| payload),
        ),
    }
}

#[tauri::command]
pub fn load_pending_provider_import() -> CommandResult<PendingProviderImportPayload> {
    match codex_plus_core::provider_import::load_pending_provider_import() {
        Ok(pending) => ok(
            "待确认供应商导入已读取。",
            PendingProviderImportPayload { pending },
        ),
        Err(error) => failed(
            &format!("读取待确认供应商导入失败：{error}"),
            PendingProviderImportPayload { pending: None },
        ),
    }
}

#[tauri::command]
pub fn confirm_pending_provider_import() -> CommandResult<SettingsPayload> {
    match codex_plus_core::provider_import::confirm_pending_provider_import() {
        Ok(Some(result)) => {
            let message = if result.imported {
                format!("已导入供应商配置：{}。", result.profile_name)
            } else {
                format!("供应商配置已存在：{}。", result.profile_name)
            };
            settings_payload(&message, "供应商导入后重新读取设置失败")
        }
        Ok(None) => settings_payload("没有待确认的供应商导入。", "设置读取失败"),
        Err(error) => failed(
            &format!("导入供应商配置失败：{error}"),
            settings_payload_value().unwrap_or_else(|(_, payload)| payload),
        ),
    }
}

#[tauri::command]
pub fn dismiss_pending_provider_import() -> CommandResult<PendingProviderImportPayload> {
    match codex_plus_core::provider_import::clear_pending_provider_import() {
        Ok(()) => ok(
            "已取消供应商导入。",
            PendingProviderImportPayload { pending: None },
        ),
        Err(error) => failed(
            &format!("取消供应商导入失败：{error}"),
            PendingProviderImportPayload { pending: None },
        ),
    }
}

#[tauri::command]
pub fn list_local_sessions(
    request: Option<ListLocalSessionsRequest>,
) -> CommandResult<LocalSessionsPayload> {
    let request = request.unwrap_or(ListLocalSessionsRequest {
        offset: 0,
        limit: DEFAULT_LOCAL_SESSIONS_PAGE_SIZE,
    });
    let offset = request.offset;
    let limit = request.limit.clamp(1, MAX_LOCAL_SESSIONS_PAGE_SIZE);
    let fetch_limit = offset.saturating_add(limit).saturating_add(1);
    let home = codex_plus_core::codex_sqlite::default_codex_home_dir();
    let db_paths = codex_plus_core::codex_sqlite::codex_session_db_paths_from_home(&home);
    let mut sessions = Vec::new();
    let mut errors = Vec::new();
    for db_path in &db_paths {
        let adapter = local_session_adapter(db_path);
        match adapter.list_local_sessions_limited(fetch_limit) {
            Ok(mut items) => sessions.append(&mut items),
            Err(error) if db_path.exists() => {
                errors.push(format!("{}: {error}", db_path.to_string_lossy()));
            }
            Err(_) => {}
        }
    }
    sessions.sort_by(|left, right| {
        right
            .updated_at_ms
            .cmp(&left.updated_at_ms)
            .then_with(|| right.id.cmp(&left.id))
    });
    let mut seen_session_ids = std::collections::HashSet::new();
    sessions.retain(|session| seen_session_ids.insert(session.id.clone()));
    let has_more = sessions.len() > offset.saturating_add(limit);
    let sessions = sessions.into_iter().skip(offset).take(limit).collect();
    let payload = LocalSessionsPayload {
        db_path: db_paths
            .first()
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_default(),
        db_paths: db_paths
            .iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect(),
        sessions,
        offset,
        limit,
        has_more,
    };
    if errors.is_empty() {
        ok(
            &format!(
                "已读取第 {} 页，共 {} 个本地会话。",
                offset / limit + 1,
                payload.sessions.len()
            ),
            payload,
        )
    } else {
        failed(
            &format!("读取部分本地会话失败：{}", errors.join("; ")),
            payload,
        )
    }
}

#[tauri::command]
pub fn list_zed_remote_projects() -> CommandResult<ZedRemoteProjectsPayload> {
    let result = codex_plus_core::zed_remote::list_zed_remote_projects_response(&json!({}));
    if result.get("status").and_then(Value::as_str) == Some("ok") {
        let projects = serde_json::from_value::<Vec<ZedRemoteProject>>(
            result
                .get("projects")
                .cloned()
                .unwrap_or_else(|| Value::Array(Vec::new())),
        )
        .unwrap_or_default();
        return ok(
            &format!("已读取 {} 个 Zed 远程项目。", projects.len()),
            ZedRemoteProjectsPayload { projects },
        );
    }
    failed(
        result
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("读取 Zed 远程项目失败。"),
        ZedRemoteProjectsPayload {
            projects: Vec::new(),
        },
    )
}

#[tauri::command]
pub fn open_zed_remote(payload: Value) -> CommandResult<ZedRemoteOpenPayload> {
    let result = codex_plus_core::zed_remote::open_zed_remote(&payload);
    let strategy = result
        .get("strategy")
        .cloned()
        .and_then(|value| serde_json::from_value::<ZedOpenStrategy>(value).ok())
        .unwrap_or_default();
    let url = result
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if result.get("status").and_then(Value::as_str) == Some("ok") {
        return ok(
            "已在 Zed Remote 打开项目。",
            ZedRemoteOpenPayload { url, strategy },
        );
    }
    failed(
        result
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("无法在 Zed Remote 打开项目。"),
        ZedRemoteOpenPayload { url, strategy },
    )
}

#[tauri::command]
pub fn forget_zed_remote_project(id: String) -> CommandResult<ZedRemoteProjectsPayload> {
    let result =
        codex_plus_core::zed_remote::forget_zed_remote_project_response(&json!({ "id": id }));
    if result.get("status").and_then(Value::as_str) != Some("ok") {
        return failed(
            result
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("移除 Zed 远程项目失败。"),
            ZedRemoteProjectsPayload {
                projects: Vec::new(),
            },
        );
    }
    list_zed_remote_projects()
}

#[tauri::command]
pub fn delete_local_session(request: DeleteLocalSessionRequest) -> CommandResult<DeleteResult> {
    let session_id = request.session_id.trim();
    if session_id.is_empty() {
        return failed(
            "会话 ID 不能为空。",
            DeleteResult {
                status: codex_plus_core::models::DeleteStatus::Failed,
                session_id: String::new(),
                message: "会话 ID 不能为空。".to_string(),
                undo_token: None,
                backup_path: None,
            },
        );
    }
    let session = SessionRef {
        session_id: session_id.to_string(),
        title: request.title,
    };
    let home = codex_plus_core::codex_sqlite::default_codex_home_dir();
    let discovered_paths = codex_plus_core::codex_sqlite::codex_session_db_paths_from_home(&home);
    let mut candidate_paths = Vec::new();
    if let Some(path) = request.db_path.as_deref() {
        let path = PathBuf::from(path);
        let requested_is_discovered = discovered_paths.iter().any(|candidate| {
            candidate == &path
                || fs::canonicalize(candidate)
                    .ok()
                    .zip(fs::canonicalize(&path).ok())
                    .is_some_and(|(candidate, requested)| candidate == requested)
        });
        if requested_is_discovered {
            candidate_paths.push(path);
        }
    }
    for path in discovered_paths {
        if !candidate_paths.iter().any(|candidate| candidate == &path) {
            candidate_paths.push(path);
        }
    }
    log_manager_event(
        "manager.delete_local_session.start",
        json!({
            "session_id": session_id,
            "title": session.title,
            "requested_db_path": request.db_path,
            "candidate_paths": candidate_paths
                .iter()
                .map(|path| path.to_string_lossy().to_string())
                .collect::<Vec<_>>(),
        }),
    );
    let result = codex_plus_data::delete_local_from_paths(
        candidate_paths.clone(),
        codex_plus_data::BackupStore::new(
            codex_plus_core::paths::default_app_state_dir().join("backups"),
        ),
        &session,
        Some(&home),
    );
    log_manager_event(
        "manager.delete_local_session.finish",
        json!({
            "session_id": session_id,
            "final_status": format!("{:?}", result.status),
            "final_message": result.message,
            "candidate_paths": candidate_paths
                .iter()
                .map(|path| path.to_string_lossy().to_string())
                .collect::<Vec<_>>(),
        }),
    );
    let status = if matches!(
        result.status,
        codex_plus_core::models::DeleteStatus::LocalDeleted
    ) {
        "ok"
    } else {
        "failed"
    };
    CommandResult {
        status: status.to_string(),
        message: result.message.clone(),
        payload: result,
    }
}

fn local_session_adapter(db_path: &Path) -> codex_plus_data::SQLiteStorageAdapter {
    codex_plus_data::SQLiteStorageAdapter::new(
        db_path,
        codex_plus_data::BackupStore::new(
            codex_plus_core::paths::default_app_state_dir().join("backups"),
        ),
    )
}

fn normalized_codex_app_path_for_save(raw: &str) -> String {
    if raw.trim().is_empty() {
        return String::new();
    }
    codex_plus_core::app_paths::normalize_codex_app_path(Path::new(raw))
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn normalize_settings_before_save(mut settings: BackendSettings) -> BackendSettings {
    settings.codex_app_path = normalized_codex_app_path_for_save(&settings.codex_app_path);
    settings.relay_common_config_contents =
        codex_plus_core::relay_config::sanitize_common_config_contents(
            &settings.relay_common_config_contents,
        );
    let (common_without_context, extracted_context) =
        split_relay_context_config_sections(&settings.relay_common_config_contents);
    settings.relay_common_config_contents = common_without_context;
    settings.relay_context_config_contents =
        relay_join_config_sections(&[&settings.relay_context_config_contents, &extracted_context]);
    settings.relay_context_config_contents =
        codex_plus_core::relay_config::sanitize_common_config_contents(
            &settings.relay_context_config_contents,
        );
    for profile in &mut settings.relay_profiles {
        if let Err(error) =
            codex_plus_core::relay_config::normalize_relay_profile_for_storage(profile)
        {
            log_manager_event(
                "manager.normalize_relay_profile_for_storage.failed",
                json!({
                    "profileId": profile.id,
                    "profileName": profile.name,
                    "error": error.to_string()
                }),
            );
        }
    }
    let common_config = relay_combined_common_config(&settings);
    if !common_config.trim().is_empty() {
        for profile in &mut settings.relay_profiles {
            if !profile.use_common_config || profile.config_contents.trim().is_empty() {
                continue;
            }
            match codex_plus_core::relay_config::strip_common_config_from_config(
                &profile.config_contents,
                &common_config,
            ) {
                Ok(stripped) => {
                    profile.config_contents =
                        strip_common_config_text_fallback(&stripped, &common_config);
                }
                Err(_) => {
                    profile.config_contents =
                        strip_common_config_text_fallback(&profile.config_contents, &common_config);
                }
            }
        }
    }
    settings.provider_sync_saved_providers =
        normalize_provider_sync_provider_list(settings.provider_sync_saved_providers);
    settings.provider_sync_manual_providers =
        normalize_provider_sync_provider_list(settings.provider_sync_manual_providers);
    settings.provider_sync_last_selected_provider = settings
        .provider_sync_last_selected_provider
        .trim()
        .to_string();
    settings
}

fn normalize_provider_sync_provider_list(values: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() || trimmed.chars().any(char::is_control) {
            continue;
        }
        if seen.insert(trimmed.to_string()) {
            result.push(trimmed.to_string());
        }
    }
    result.sort();
    result
}

fn relay_combined_common_config(settings: &BackendSettings) -> String {
    relay_join_config_sections(&[
        &settings.relay_common_config_contents,
        &settings.relay_context_config_contents,
    ])
}

fn relay_join_config_sections(sections: &[&str]) -> String {
    let sections = sections
        .iter()
        .map(|section| section.trim())
        .filter(|section| !section.is_empty())
        .collect::<Vec<_>>();
    if sections.is_empty() {
        String::new()
    } else {
        codex_plus_core::relay_config::normalize_config_text(&format!(
            "{}\n",
            sections.join("\n\n")
        ))
    }
}

fn split_relay_context_config_sections(config: &str) -> (String, String) {
    let mut common = Vec::new();
    let mut context = Vec::new();
    let mut in_context_table = false;

    for line in config.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_context_table = trimmed.starts_with("[mcp_servers.")
                || trimmed.starts_with("[skills.")
                || trimmed.starts_with("[plugins.");
        }
        if in_context_table {
            context.push(line);
        } else {
            common.push(line);
        }
    }

    (
        relay_join_config_sections(&[&common.join("\n")]),
        relay_join_config_sections(&[&context.join("\n")]),
    )
}

fn strip_common_config_text_fallback(config_contents: &str, common_config: &str) -> String {
    let common = common_config_anchors(common_config);
    if common.root_keys.is_empty() && common.table_headers.is_empty() {
        return ensure_text_newline(config_contents.trim_end());
    }

    let mut kept = Vec::new();
    let mut skipping_table = false;
    let mut in_root_section = true;
    let mut removed_root_keys = std::collections::HashSet::new();
    let source_root_keys = toml_root_keys_before_first_table(config_contents);

    for line in config_contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_root_section = false;
            let header = trimmed.to_string();
            skipping_table = common.table_headers.contains(&header);
            if skipping_table {
                continue;
            }
        }

        if skipping_table {
            continue;
        }

        if in_root_section && let Some(key) = toml_key_from_line(trimmed) {
            if common.root_keys.contains(key) {
                let is_duplicate_common_key = removed_root_keys.contains(key)
                    || source_root_keys.contains(key)
                    || common.table_headers.contains("[features]")
                    || common
                        .table_headers
                        .contains("[marketplaces.openai-bundled]")
                    || common
                        .table_headers
                        .contains("[plugins.\"superpowers@openai-curated\"]");
                if is_duplicate_common_key {
                    removed_root_keys.insert(key.to_string());
                    continue;
                }
            }
        }

        kept.push(line);
    }

    ensure_text_newline(kept.join("\n").trim_end())
}

fn toml_root_keys_before_first_table(config_contents: &str) -> std::collections::HashSet<String> {
    let mut keys = std::collections::HashSet::new();
    for line in config_contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            break;
        }
        if let Some(key) = toml_key_from_line(trimmed) {
            keys.insert(key.to_string());
        }
    }
    keys
}

struct CommonConfigAnchors {
    root_keys: std::collections::HashSet<String>,
    table_headers: std::collections::HashSet<String>,
}

fn common_config_anchors(common_config: &str) -> CommonConfigAnchors {
    let mut root_keys = std::collections::HashSet::new();
    let mut table_headers = std::collections::HashSet::new();
    let mut in_table = false;

    for line in common_config.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_table = true;
            table_headers.insert(trimmed.to_string());
            continue;
        }
        if !in_table {
            if let Some(key) = toml_key_from_line(trimmed) {
                root_keys.insert(key.to_string());
            }
        }
    }

    CommonConfigAnchors {
        root_keys,
        table_headers,
    }
}

fn toml_key_from_line(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let (key, _) = trimmed.split_once('=')?;
    let key = key.trim();
    if key.is_empty() { None } else { Some(key) }
}

fn ensure_text_newline(value: &str) -> String {
    if value.trim().is_empty() {
        String::new()
    } else {
        format!("{}\n", value.trim_end())
    }
}

#[tauri::command]
pub async fn load_provider_sync_targets() -> CommandResult<Value> {
    let settings = match SettingsStore::default().load() {
        Ok(settings) => settings,
        Err(error) => {
            return failed(
                &format!(
                    "Provider 同步目标加载失败：无法读取 Manager 设置：{error:#}。未使用默认 Provider 列表。"
                ),
                json!({}),
            );
        }
    };
    let result =
        tauri::async_runtime::spawn_blocking(|| codex_plus_data::load_provider_sync_targets(None))
            .await
            .map_err(|error| anyhow::anyhow!("provider target discovery task failed: {error}"));
    match result {
        Ok(mut targets) => {
            let manual = settings
                .provider_sync_manual_providers
                .iter()
                .chain(settings.provider_sync_saved_providers.iter())
                .filter_map(|value| {
                    let trimmed = value.trim();
                    if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed.to_string())
                    }
                })
                .collect::<Vec<_>>();
            merge_manual_provider_sync_targets(&mut targets, &manual, &settings);
            ok(
                "Provider 同步目标已加载。",
                serde_json::to_value(targets).unwrap_or_else(|_| json!({})),
            )
        }
        Err(error) => failed(&format!("Provider 同步目标加载失败：{error}"), json!({})),
    }
}

fn merge_manual_provider_sync_targets(
    targets: &mut codex_plus_data::ProviderSyncTargetList,
    manual: &[String],
    settings: &BackendSettings,
) {
    for id in manual {
        if let Some(existing) = targets.targets.iter_mut().find(|target| target.id == *id) {
            if !existing
                .sources
                .contains(&codex_plus_data::ProviderSyncTargetSource::Manual)
            {
                existing
                    .sources
                    .push(codex_plus_data::ProviderSyncTargetSource::Manual);
                existing.sources.sort();
            }
            existing.is_manual = settings.provider_sync_manual_providers.contains(id);
            existing.is_saved = settings.provider_sync_saved_providers.contains(id);
        } else {
            targets
                .targets
                .push(codex_plus_data::ProviderSyncTargetOption {
                    id: id.clone(),
                    sources: vec![codex_plus_data::ProviderSyncTargetSource::Manual],
                    is_current_provider: *id == targets.current_provider,
                    is_manual: settings.provider_sync_manual_providers.contains(id),
                    is_saved: settings.provider_sync_saved_providers.contains(id),
                });
        }
    }
    targets.targets.sort_by(|left, right| {
        right
            .is_current_provider
            .cmp(&left.is_current_provider)
            .then_with(|| left.id.cmp(&right.id))
    });
}

#[tauri::command]
pub async fn sync_providers_now(target_provider: Option<String>) -> CommandResult<Value> {
    if let Err(error) = SettingsStore::default().load() {
        return failed(
            &format!(
                "Provider 同步已停止：无法读取 Manager 设置：{error:#}。未改写会话历史或 Provider 选择。"
            ),
            json!({}),
        );
    }
    if let Some((process_count, message)) = codex_running_mutation_message("同步 Provider") {
        return failed(
            &message,
            json!({
                "codexRunning": true,
                "codexProcessCount": process_count,
            }),
        );
    }
    let home = codex_plus_core::relay_config::default_codex_home_dir();
    if let Err(error) = codex_plus_core::codex_app_state::capture_app_state_snapshot(&home) {
        return failed(
            &format!(
                "Provider 同步已停止：无法创建 Codex 界面状态恢复快照：{error:#}。未改写会话历史或 Provider 选择。"
            ),
            json!({}),
        );
    }
    let target_provider = target_provider
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let target_for_settings = target_provider.clone();
    let result = tauri::async_runtime::spawn_blocking(move || -> anyhow::Result<_> {
        let process_count = codex_plus_core::watcher::find_codex_processes().len();
        if process_count > 0 {
            anyhow::bail!(
                "检测到 Codex 在同步开始前启动（{process_count} 个进程）；未改写会话历史或 Provider 选择"
            );
        }
        Ok(codex_plus_data::run_provider_sync_with_target(
            None,
            target_provider.as_deref(),
        ))
    })
    .await
    .map_err(|error| anyhow::anyhow!("provider sync task failed: {error}"))
    .and_then(|result| result);
    match result {
        Ok(sync) => {
            if is_success_sync_status(&sync.status) {
                codex_plus_core::codex_app_state::sync_app_state_after_provider_switch_nonfatal(
                    &home,
                    "manager.sync_providers_now.after",
                );
            }
            let selection_error = if is_success_sync_status(&sync.status) {
                persist_provider_sync_selection(
                    target_for_settings
                        .as_deref()
                        .unwrap_or(&sync.target_provider),
                )
                .err()
            } else {
                None
            };
            let message = if is_success_sync_status(&sync.status) {
                format!(
                    "供应商已同步一次：{} 个会话文件，{} 行索引，跳过 {} 个占用文件。",
                    sync.changed_session_files,
                    sync.sqlite_rows_updated,
                    sync.skipped_locked_rollout_files.len()
                )
            } else {
                sync.message.clone()
            };
            let payload = json!({
                "syncStatus": sync.status,
                "targetProvider": sync.target_provider,
                "changedSessionFiles": sync.changed_session_files,
                "skippedLockedRolloutFiles": sync.skipped_locked_rollout_files,
                "sqliteRowsUpdated": sync.sqlite_rows_updated,
                "sqliteProviderRowsUpdated": sync.sqlite_provider_rows_updated,
                "sqliteUserEventRowsUpdated": sync.sqlite_user_event_rows_updated,
                "sqliteCwdRowsUpdated": sync.sqlite_cwd_rows_updated,
                "updatedWorkspaceRoots": sync.updated_workspace_roots,
                "encryptedContentWarning": sync.encrypted_content_warning,
                "backupDir": sync.backup_dir,
                "syncMessage": sync.message,
            });
            if let Some(error) = selection_error {
                CommandResult {
                    status: "degraded".to_string(),
                    message: format!(
                        "{message} 但保存本次 Provider 选择失败：{error:#}；会话同步结果和备份已保留。"
                    ),
                    payload,
                }
            } else if is_success_sync_status(&sync.status) {
                ok(&message, payload)
            } else {
                failed(&message, payload)
            }
        }
        Err(error) => failed(&format!("供应商同步失败：{error}"), json!({})),
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MirrorPreflightCheck {
    pub id: String,
    pub ready: bool,
    pub label: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MirrorPreflight {
    pub ready: bool,
    pub codex_installed: bool,
    pub codex_app_path: Option<String>,
    pub codex_version: Option<String>,
    pub codex_running: bool,
    pub codex_home: String,
    pub checks: Vec<MirrorPreflightCheck>,
}

#[tauri::command]
pub fn get_mirror_access_status() -> CommandResult<Value> {
    let home = codex_plus_core::relay_config::default_codex_home_dir();
    let state_dir = codex_plus_core::paths::default_app_state_dir();
    let access = codex_plus_core::mirror_access::access_status(&home, &state_dir);
    if access.phase == "state_unreadable" {
        let message = access.last_message.clone();
        failed(&message, json!({ "access": access }))
    } else {
        ok("mirror x codex 状态已加载。", json!({ "access": access }))
    }
}

#[tauri::command]
pub async fn get_windows_sandbox_diagnostic() -> CommandResult<Value> {
    let diagnostic = codex_plus_core::windows_sandbox::diagnose_default_with_official().await;
    ok(
        "Windows 执行环境检查完成。",
        json!({ "sandboxDiagnostic": diagnostic }),
    )
}

#[tauri::command]
pub async fn enable_windows_sandbox_access() -> CommandResult<Value> {
    let Ok(_guard) = windows_sandbox_operation_mutex().try_lock() else {
        return failed(
            "Windows 文件执行环境操作正在进行，请等待当前操作完成。",
            json!({ "sandboxEnable": null }),
        );
    };
    if !codex_plus_core::watcher::find_codex_processes().is_empty() {
        return failed(
            "请先在 Codex 中停止当前任务并正常退出；为保护会话，本工具不会在 Codex 运行时修改 Windows 执行环境。",
            json!({ "sandboxEnable": null }),
        );
    }
    if let Some(result) = reject_invalid_codex_state_locations(true) {
        return result;
    }
    let home = codex_plus_core::codex_home::default_codex_home_dir();
    if let Err(error) = codex_plus_core::mirror_access::ensure_storage_headroom(
        &home,
        0,
        codex_plus_core::mirror_access::MIN_CODEX_RUNTIME_FREE_SPACE_BYTES,
    ) {
        return failed(
            &format!("Windows 文件执行环境初始化已停止：{error}"),
            json!({ "sandboxEnable": null }),
        );
    }
    match codex_plus_core::windows_sandbox::ensure_full_file_access(None).await {
        Ok(result) => {
            let diagnostic =
                codex_plus_core::windows_sandbox::diagnose_default_with_official().await;
            ok(
                &result.message,
                json!({
                    "sandboxEnable": result,
                    "sandboxDiagnostic": diagnostic,
                }),
            )
        }
        Err(error) => failed(
            &format!(
                "完整文件能力启用失败：{error:#}。未切换到受限访问；请保留当前窗口查看具体原因。"
            ),
            json!({
                "sandboxEnable": null,
                "sandboxDiagnostic": codex_plus_core::windows_sandbox::diagnose_default_with_official().await,
            }),
        ),
    }
}

#[tauri::command]
pub fn get_mirror_preflight() -> CommandResult<Value> {
    let settings_result = SettingsStore::default().load();
    let settings_issue = settings_result
        .as_ref()
        .err()
        .map(|error| format!("{error:#}"));
    let settings_ready = settings_issue.is_none();
    let app_dir = codex_plus_core::app_paths::resolve_codex_app_dir_with_saved(
        None,
        settings_result
            .as_ref()
            .ok()
            .map(|settings| settings.codex_app_path.as_str()),
    );
    let codex_app_launchable = app_dir
        .as_deref()
        .is_some_and(codex_plus_core::app_paths::is_codex_app_launchable);
    let bundled_cli_ready = app_dir
        .as_deref()
        .and_then(codex_plus_core::app_paths::find_bundled_codex_cli)
        .is_some();
    let codex_installed = codex_app_launchable && (!cfg!(windows) || bundled_cli_ready);
    let codex_app_path = codex_installed.then(|| {
        app_dir
            .as_ref()
            .expect("installed Codex has an app directory")
            .to_string_lossy()
            .to_string()
    });
    let codex_version = app_dir
        .as_deref()
        .filter(|_| codex_installed)
        .and_then(codex_plus_core::app_paths::codex_app_version);
    let home_resolution = codex_plus_core::codex_home::resolve_codex_home();
    let home = home_resolution.path.clone();
    let home_environment_issue = home_resolution.issue.clone();
    let sqlite_resolution = codex_plus_core::codex_sqlite::resolve_codex_sqlite_home();
    let sqlite_environment_issue = sqlite_resolution.issue.clone();
    let codex_running = !codex_plus_core::watcher::find_codex_processes().is_empty();
    let state_probe_allowed = should_probe_codex_state_directories(codex_installed, codex_running);
    let state_dir = codex_plus_core::paths::default_app_state_dir();
    let home_writable = state_probe_allowed
        && home_environment_issue.is_none()
        && probe_writable_directory(&home).is_ok();
    let state_writable = state_probe_allowed && probe_writable_directory(&state_dir).is_ok();
    let sqlite_writable = state_probe_allowed
        && sqlite_environment_issue.is_none()
        && sqlite_resolution
            .path
            .as_deref()
            .map(|path| probe_writable_directory(path).is_ok())
            .unwrap_or(true);
    let storage_paths = codex_plus_core::mirror_access::codex_runtime_storage_paths(
        &home,
        &state_dir,
        sqlite_resolution.path.as_deref(),
        app_dir.as_deref(),
    );
    let checked_storage_volumes = storage_paths.len();
    let storage_result = if home_writable && state_writable && sqlite_writable {
        (|| -> anyhow::Result<u64> {
            let mut minimum_available = u64::MAX;
            for path in &storage_paths {
                minimum_available =
                    minimum_available.min(codex_plus_core::mirror_access::ensure_storage_headroom(
                        path,
                        0,
                        codex_plus_core::mirror_access::MIN_CODEX_RUNTIME_FREE_SPACE_BYTES,
                    )?);
            }
            Ok(minimum_available)
        })()
    } else {
        Ok(0)
    };
    let storage_ready =
        home_writable && state_writable && sqlite_writable && storage_result.is_ok();
    let config_result = if home_writable && !codex_running {
        codex_plus_core::mirror_access::validate_existing_config(&home)
    } else {
        Ok(())
    };
    let config_ready = home_writable && config_result.is_ok();
    let project_configs_checked =
        home_writable && state_writable && sqlite_writable && !codex_running;
    let project_scan = if project_configs_checked {
        codex_plus_core::project_config::scan_recent_project_configs(
            &home,
            codex_plus_core::mirror_access::MIRROR_PROVIDER_ID,
            64,
        )
    } else {
        codex_plus_core::project_config::ProjectConfigScan::default()
    };
    let project_provider_overrides = project_scan
        .overrides
        .iter()
        .filter(|entry| entry.changes_active_provider)
        .collect::<Vec<_>>();
    let project_configs_ready = project_configs_checked && project_provider_overrides.is_empty();
    let project_config_detail = if !project_configs_checked {
        "等待配置与会话目录检查".to_string()
    } else if let Some(entry) = project_provider_overrides.first() {
        format!(
            "发现 {} 个近期项目会覆盖 mirrorplus 路由；请先处理 {}：{}",
            project_provider_overrides.len(),
            entry.config_path,
            entry.detail
        )
    } else if !project_scan.unreadable_configs.is_empty() {
        format!(
            "未发现路由冲突；另有 {} 个项目配置无法读取，已写入启动诊断",
            project_scan.unreadable_configs.len()
        )
    } else if project_scan.overrides.is_empty() {
        format!(
            "已检查 {} 个近期项目，未发现项目级路由覆盖",
            project_scan.scanned_projects
        )
    } else {
        format!(
            "已检查 {} 个近期项目；{} 个项目仅固定模型，不会切换 Provider",
            project_scan.scanned_projects,
            project_scan.overrides.len()
        )
    };
    let checks = vec![
        mirror_settings_preflight_check(settings_issue.as_deref()),
        MirrorPreflightCheck {
            id: "codex".to_string(),
            ready: codex_installed,
            label: "Codex Desktop".to_string(),
            detail: if codex_installed {
                codex_version
                    .as_ref()
                    .map(|version| format!("已检测到版本 {version}"))
                    .unwrap_or_else(|| "已检测到 Codex 应用及包内真实 CLI".to_string())
            } else if codex_app_launchable && !bundled_cli_ready {
                "检测到桌面程序，但包内真实 Codex CLI 缺失或不可读；请修复或重新安装 Codex"
                    .to_string()
            } else {
                "未检测到 Codex，请先安装后重新检测".to_string()
            },
        },
        MirrorPreflightCheck {
            id: "home".to_string(),
            ready: home_writable && state_writable && sqlite_writable && !codex_running,
            label: "配置与会话目录".to_string(),
            detail: if codex_running {
                "检测到 Codex 正在运行，请完全退出后再接管或修复".to_string()
            } else if let Some(issue) = &home_environment_issue {
                issue.clone()
            } else if let Some(issue) = &sqlite_environment_issue {
                issue.clone()
            } else if home_writable && state_writable && sqlite_writable {
                sqlite_resolution
                    .path
                    .as_ref()
                    .map(|path| {
                        format!(
                            "配置、Mirror 状态与外置会话目录 {} 均可安全写入",
                            path.display()
                        )
                    })
                    .unwrap_or_else(|| "配置、Mirror 状态与会话目录均可安全写入".to_string())
            } else if !state_writable && codex_installed {
                format!(
                    "无法写入 Mirror 状态目录 {}，不会开始接管",
                    state_dir.display()
                )
            } else if codex_installed {
                "无法写入配置或会话目录，请检查目录权限".to_string()
            } else {
                "安装并启动一次 Codex 后检查".to_string()
            },
        },
        MirrorPreflightCheck {
            id: "storage".to_string(),
            ready: storage_ready,
            label: "磁盘空间".to_string(),
            detail: match &storage_result {
                Ok(bytes) if storage_ready => {
                    format!(
                        "已检查 {checked_storage_volumes} 个相关磁盘卷；最低可用 {} MB，可安全创建快照、缓存和临时文件",
                        bytes / (1024 * 1024)
                    )
                }
                Err(error) => error.to_string(),
                _ => "等待配置目录检查".to_string(),
            },
        },
        MirrorPreflightCheck {
            id: "config".to_string(),
            ready: config_ready,
            label: "原始配置".to_string(),
            detail: match config_result {
                Ok(()) if config_ready => "格式正常，可以备份和回滚".to_string(),
                Err(error) => error.to_string(),
                _ => "等待配置目录检查".to_string(),
            },
        },
        MirrorPreflightCheck {
            id: "project-config".to_string(),
            ready: project_configs_ready,
            label: "项目级配置".to_string(),
            detail: project_config_detail,
        },
    ];
    let preflight = MirrorPreflight {
        ready: codex_installed
            && settings_ready
            && home_writable
            && state_writable
            && sqlite_writable
            && storage_ready
            && config_ready
            && project_configs_ready
            && !codex_running,
        codex_installed,
        codex_app_path,
        codex_version,
        codex_running,
        codex_home: home.to_string_lossy().to_string(),
        checks,
    };
    ok("安装前体检完成。", json!({ "preflight": preflight }))
}

fn mirror_settings_preflight_check(issue: Option<&str>) -> MirrorPreflightCheck {
    MirrorPreflightCheck {
        id: "settings".to_string(),
        ready: issue.is_none(),
        label: "Manager 设置".to_string(),
        detail: issue
            .map(|issue| {
                format!("{issue}；请先恢复 settings.json，本次不会使用默认设置继续接入或启动")
            })
            .unwrap_or_else(|| "settings.json 格式正常，可在写入后回读验证".to_string()),
    }
}

const MIRROR_KEY_VALIDATION_TTL: Duration = Duration::from_secs(15 * 60);
const MAX_MIRROR_KEY_VALIDATIONS: usize = 12;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct MixedAuthStatus {
    ready: bool,
    method: String,
    message: String,
}

fn classify_codex_login_status(success: bool, output: &str) -> MixedAuthStatus {
    let normalized = output.to_ascii_lowercase();
    let chatgpt_login = [
        "logged in using chatgpt",
        "logged in with chatgpt",
        "signed in using chatgpt",
        "signed in with chatgpt",
    ]
    .iter()
    .any(|marker| normalized.contains(marker));
    if success && chatgpt_login {
        return MixedAuthStatus {
            ready: true,
            method: "chatgpt".to_string(),
            message: "已通过真实 Codex CLI 确认 ChatGPT 登录，可使用混合 API。".to_string(),
        };
    }
    let api_key_login = [
        "logged in using an api key",
        "logged in with an api key",
        "signed in using an api key",
        "signed in with an api key",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
        || normalized.contains("api key")
        || normalized.contains("api-key");
    if success && api_key_login {
        return MixedAuthStatus {
            ready: false,
            method: "apiKey".to_string(),
            message: "当前 Codex 使用 API Key 登录。请选择纯 API，或先在 Codex 中改为 ChatGPT 登录后再使用混合 API。".to_string(),
        };
    }
    let signed_out = [
        "not logged in",
        "not signed in",
        "no active login",
        "no active authentication",
    ]
    .iter()
    .any(|marker| normalized.contains(marker));
    MixedAuthStatus {
        ready: false,
        method: if signed_out { "signedOut" } else { "unknown" }.to_string(),
        message: "未通过真实 Codex CLI 确认 ChatGPT 登录。请先打开 Codex 完成 ChatGPT 登录，再返回重新检测；也可以直接选择纯 API。".to_string(),
    }
}

fn bounded_codex_login_output(stdout: &[u8], stderr: &[u8]) -> String {
    const MAX_CAPTURED_BYTES: usize = 16 * 1024;
    let mut combined = Vec::with_capacity(
        stdout
            .len()
            .saturating_add(stderr.len())
            .min(MAX_CAPTURED_BYTES),
    );
    for source in [stdout, stderr] {
        let remaining = MAX_CAPTURED_BYTES.saturating_sub(combined.len());
        if remaining == 0 {
            break;
        }
        combined.extend_from_slice(&source[..source.len().min(remaining)]);
        if combined.len() < MAX_CAPTURED_BYTES {
            combined.push(b'\n');
        }
    }
    String::from_utf8_lossy(&combined).into_owned()
}

fn mixed_auth_with_file_fallback(home: &Path, inconclusive: MixedAuthStatus) -> MixedAuthStatus {
    let file_status = codex_plus_core::relay_config::chatgpt_auth_status_from_home(home);
    if file_status.authenticated {
        MixedAuthStatus {
            ready: true,
            method: "chatgpt".to_string(),
            message: "真实 Codex CLI 暂时无法给出明确结论；已通过当前 CODEX_HOME 的 auth.json 确认 ChatGPT 登录，可使用混合 API。".to_string(),
        }
    } else {
        inconclusive
    }
}

static MIXED_AUTH_PROBE_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TemporaryMixedAuthProbeHome {
    path: PathBuf,
    temp_root: PathBuf,
}

impl TemporaryMixedAuthProbeHome {
    fn create(credentials_store: &str) -> anyhow::Result<Self> {
        if !matches!(credentials_store, "auto" | "keyring") {
            anyhow::bail!("不支持隔离检查的凭据存储模式：{credentials_store}");
        }
        let temp_root = std::env::temp_dir();
        for _ in 0..8 {
            let counter = MIXED_AUTH_PROBE_COUNTER.fetch_add(1, Ordering::Relaxed);
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = temp_root.join(format!(
                "mirror-x-codex-auth-probe-{}-{timestamp}-{counter}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => {
                    let probe = Self {
                        path,
                        temp_root: temp_root.clone(),
                    };
                    let config = format!("cli_auth_credentials_store = \"{credentials_store}\"\n");
                    codex_plus_core::settings::atomic_write(
                        &probe.path.join("config.toml"),
                        config.as_bytes(),
                    )?;
                    return Ok(probe);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        anyhow::bail!("无法创建唯一的隔离登录检查目录")
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TemporaryMixedAuthProbeHome {
    fn drop(&mut self) {
        let safe_name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("mirror-x-codex-auth-probe-"));
        if self.path.parent() == Some(self.temp_root.as_path()) && safe_name {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

async fn inspect_codex_cli_chatgpt_auth(home: &Path) -> MixedAuthStatus {
    let cli = match codex_plus_core::windows_sandbox::resolve_codex_cli_for_app(None) {
        Ok(cli) => cli,
        Err(error) => {
            return MixedAuthStatus {
                ready: false,
                method: "unavailable".to_string(),
                message: format!(
                    "无法通过真实 Codex CLI 确认 ChatGPT 登录：{error:#}。请先修复 Codex 安装或选择纯 API。"
                ),
            };
        }
    };
    let mut command = tokio::process::Command::new(cli);
    command
        .args(["login", "status"])
        .env("CODEX_HOME", home)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(codex_plus_core::windows_create_no_window());
    let output = match tokio::time::timeout(Duration::from_secs(12), command.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            return MixedAuthStatus {
                ready: false,
                method: "unavailable".to_string(),
                message: format!(
                    "真实 Codex CLI 登录检查无法启动：{error}。请修复 Codex 安装或选择纯 API。"
                ),
            };
        }
        Err(_) => {
            return MixedAuthStatus {
                ready: false,
                method: "unavailable".to_string(),
                message: "真实 Codex CLI 登录检查超时。请重新检测，或选择纯 API。".to_string(),
            };
        }
    };
    let combined = bounded_codex_login_output(&output.stdout, &output.stderr);
    classify_codex_login_status(output.status.success(), &combined)
}

async fn inspect_mixed_chatgpt_auth() -> MixedAuthStatus {
    let home_resolution = codex_plus_core::codex_home::resolve_codex_home();
    if let Some(issue) = home_resolution.issue {
        return MixedAuthStatus {
            ready: false,
            method: "unavailable".to_string(),
            message: format!("无法确认 ChatGPT 登录：{issue}"),
        };
    }
    let state_dir = codex_plus_core::paths::default_app_state_dir();
    match codex_plus_core::mirror_access::restorable_chatgpt_login(&state_dir) {
        Ok(Some(codex_plus_core::mirror_access::RestorableChatgptLogin::AuthFile)) => {
            return MixedAuthStatus {
                ready: true,
                method: "chatgpt".to_string(),
                message: "当前 Pure API 接管 baseline 中保留了可恢复的 ChatGPT 登录，可安全切换到混合 API。".to_string(),
            };
        }
        Ok(Some(codex_plus_core::mirror_access::RestorableChatgptLogin::CredentialStore {
            credentials_store,
            ..
        })) => {
            let probe = match TemporaryMixedAuthProbeHome::create(&credentials_store) {
                Ok(probe) => probe,
                Err(error) => {
                    return MixedAuthStatus {
                        ready: false,
                        method: "unavailable".to_string(),
                        message: format!(
                            "无法创建隔离的 keyring 登录检查环境：{error:#}。本机现有 CODEX_HOME 未被修改；请重试或选择纯 API。"
                        ),
                    };
                }
            };
            let mut status = inspect_codex_cli_chatgpt_auth(probe.path()).await;
            if status.ready {
                status.message = "已通过隔离的真实 Codex CLI 检查确认 Pure API baseline 对应的 keyring 中仍为 ChatGPT 登录，可安全切换到混合 API。".to_string();
            }
            return status;
        }
        Ok(None) => {}
        Err(error) => {
            return MixedAuthStatus {
                ready: false,
                method: "unavailable".to_string(),
                message: format!(
                    "无法确认受保护的登录 baseline：{error:#}。请先恢复接管状态，或选择纯 API。"
                ),
            };
        }
    }

    let cli_status = inspect_codex_cli_chatgpt_auth(&home_resolution.path).await;
    if matches!(cli_status.method.as_str(), "unknown" | "unavailable") {
        mixed_auth_with_file_fallback(&home_resolution.path, cli_status)
    } else {
        cli_status
    }
}

#[tauri::command]
pub async fn get_mirror_mixed_auth_status() -> CommandResult<Value> {
    let status = inspect_mixed_chatgpt_auth().await;
    ok(
        &status.message,
        json!({
            "mixedAuth": status,
        }),
    )
}

#[derive(Clone)]
struct MirrorKeyValidation {
    fingerprint: u64,
    key_length: usize,
    discovery: codex_plus_core::mirror_access::MirrorModelDiscovery,
    probe_model: String,
    http_status: u16,
    endpoint: String,
    validated_at: Instant,
}

impl MirrorKeyValidation {
    fn response_probe(&self, group: &str, now: Instant) -> Value {
        json!({
            "group": group,
            "model": self.probe_model,
            "httpStatus": self.http_status,
            "endpoint": self.endpoint,
            "source": "recentKeyValidation",
            "reused": true,
            "ageSeconds": now.saturating_duration_since(self.validated_at).as_secs(),
        })
    }
}

fn mirror_key_identity(api_key: &str) -> (u64, usize) {
    let key = api_key.trim();
    let mut hasher = DefaultHasher::new();
    "mirror-x-key-validation-v1".hash(&mut hasher);
    key.hash(&mut hasher);
    (hasher.finish(), key.len())
}

fn cached_mirror_key_validation(
    cache: &mut Vec<MirrorKeyValidation>,
    api_key: &str,
    now: Instant,
) -> Option<MirrorKeyValidation> {
    cache.retain(|entry| {
        now.saturating_duration_since(entry.validated_at) <= MIRROR_KEY_VALIDATION_TTL
    });
    let (fingerprint, key_length) = mirror_key_identity(api_key);
    cache
        .iter()
        .rev()
        .find(|entry| entry.fingerprint == fingerprint && entry.key_length == key_length)
        .cloned()
}

fn remember_mirror_key_validation(
    cache: &mut Vec<MirrorKeyValidation>,
    api_key: &str,
    discovery: codex_plus_core::mirror_access::MirrorModelDiscovery,
    probe_model: String,
    http_status: u16,
    endpoint: String,
    now: Instant,
) {
    let (fingerprint, key_length) = mirror_key_identity(api_key);
    cache.retain(|entry| {
        now.saturating_duration_since(entry.validated_at) <= MIRROR_KEY_VALIDATION_TTL
            && !(entry.fingerprint == fingerprint && entry.key_length == key_length)
    });
    cache.push(MirrorKeyValidation {
        fingerprint,
        key_length,
        discovery,
        probe_model,
        http_status,
        endpoint,
        validated_at: now,
    });
    if cache.len() > MAX_MIRROR_KEY_VALIDATIONS {
        cache.drain(..cache.len() - MAX_MIRROR_KEY_VALIDATIONS);
    }
}

fn mirror_key_validation_cache() -> &'static AsyncMutex<Vec<MirrorKeyValidation>> {
    static CACHE: OnceLock<AsyncMutex<Vec<MirrorKeyValidation>>> = OnceLock::new();
    CACHE.get_or_init(|| AsyncMutex::new(Vec::new()))
}

async fn recent_mirror_key_validation(api_key: &str) -> Option<MirrorKeyValidation> {
    let mut cache = mirror_key_validation_cache().lock().await;
    cached_mirror_key_validation(&mut cache, api_key, Instant::now())
}

async fn store_mirror_key_validation(
    api_key: &str,
    discovery: codex_plus_core::mirror_access::MirrorModelDiscovery,
    probe_model: String,
    http_status: u16,
    endpoint: String,
) {
    let mut cache = mirror_key_validation_cache().lock().await;
    remember_mirror_key_validation(
        &mut cache,
        api_key,
        discovery,
        probe_model,
        http_status,
        endpoint,
        Instant::now(),
    );
}

#[tauri::command]
pub async fn validate_mirror_key(api_key: String) -> CommandResult<Value> {
    let api_key = api_key.trim().to_string();
    if api_key.is_empty() {
        return failed(
            "请输入镜子AI API Key。",
            json!({ "discovery": null, "responseProbe": null }),
        );
    }
    if let Some(validation) = recent_mirror_key_validation(&api_key).await {
        return ok(
            &format!(
                "已复用最近完成的真实流式验证，发现 {} 个模型；应用接入时不会重复发送付费请求。",
                validation.discovery.models.len()
            ),
            json!({
                "discovery": validation.discovery,
                "responseProbe": validation.response_probe("当前 Key", Instant::now()),
                "validationReused": true,
                "validForSeconds": MIRROR_KEY_VALIDATION_TTL.as_secs(),
            }),
        );
    }
    match codex_plus_core::mirror_access::discover_models(&api_key).await {
        Ok(discovery) => {
            let group = codex_plus_core::mirror_access::MirrorAccessGroup {
                id: "validation".to_string(),
                label: "当前 Key".to_string(),
                api_key: api_key.clone(),
                discovery: discovery.clone(),
            };
            let (probe_model, probe) =
                match codex_plus_core::mirror_access::probe_profile_for_group(&group) {
                    Ok(profile) => {
                        let model = profile.model.clone();
                        (
                            model,
                            codex_plus_core::relay_config::test_relay_profile_stream(
                                &profile.profile,
                                &profile.model,
                            )
                            .await,
                        )
                    }
                    Err(error) => (String::new(), Err(error)),
                };
            match probe {
                Ok(probe) if (200..300).contains(&probe.http_status) => {
                    store_mirror_key_validation(
                        &api_key,
                        discovery.clone(),
                        probe_model.clone(),
                        probe.http_status,
                        probe.endpoint.clone(),
                    )
                    .await;
                    ok(
                        &format!(
                            "API Key 已通过模型列表和一次真实流式 /responses 验证，发现 {} 个模型；15 分钟内应用接入不会重复发送付费请求。",
                            discovery.models.len()
                        ),
                        json!({
                            "discovery": discovery,
                            "responseProbe": {
                                "group": "当前 Key",
                                "model": probe_model,
                                "httpStatus": probe.http_status,
                                "endpoint": probe.endpoint,
                                "source": "keyValidation",
                                "reused": false,
                            },
                            "validationReused": false,
                            "validForSeconds": MIRROR_KEY_VALIDATION_TTL.as_secs(),
                        }),
                    )
                }
                Ok(probe) => failed(
                    &format!(
                        "Key 能读取模型列表，但真实流式 /responses 返回 HTTP {}，未标记为已验证。",
                        probe.http_status
                    ),
                    json!({
                        "discovery": null,
                        "responseProbe": {
                            "httpStatus": probe.http_status,
                            "endpoint": probe.endpoint,
                        }
                    }),
                ),
                Err(error) => failed(
                    &format!(
                        "Key 能读取模型列表，但无法完成真实流式 /responses 请求，未标记为已验证：{error}"
                    ),
                    json!({ "discovery": null, "responseProbe": null }),
                ),
            }
        }
        Err(error) => failed(&format!("验证失败：{error}"), json!({ "discovery": null })),
    }
}

#[tauri::command]
pub fn get_mirror_imagegen_status() -> CommandResult<Value> {
    let home = codex_plus_core::relay_config::default_codex_home_dir();
    let state_dir = codex_plus_core::paths::default_app_state_dir();
    ok(
        "镜子AI生图状态已读取。",
        json!({
            "imagegen": codex_plus_core::imagegen_skill::status(&home, &state_dir)
        }),
    )
}

#[tauri::command]
pub async fn validate_mirror_image_key(api_key: String) -> CommandResult<Value> {
    if api_key.trim().is_empty() {
        return failed(
            "请输入镜子AI Image Key。",
            json!({ "valid": false, "model": null }),
        );
    }
    match codex_plus_core::imagegen_skill::validate_saved_or_provided_key(Some(&api_key)).await {
        Ok(_) => ok(
            "已确认该 Image Key 的模型列表包含 gpt-image-2；尚未发送真实生图请求。",
            json!({ "valid": true, "model": "gpt-image-2" }),
        ),
        Err(error) => failed(&error.to_string(), json!({ "valid": false, "model": null })),
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MirrorKeyGroupRequest {
    pub id: String,
    pub label: String,
    pub api_key: String,
    pub selected_model_ids: Vec<String>,
}

async fn probe_mirror_profile_stream(
    profile: &codex_plus_core::mirror_access::MirrorProbeProfile,
) -> Result<Value, (String, Value)> {
    match codex_plus_core::relay_config::test_relay_profile_stream(&profile.profile, &profile.model)
        .await
    {
        Ok(probe) if (200..300).contains(&probe.http_status) => Ok(json!({
            "group": profile.label,
            "model": profile.model,
            "httpStatus": probe.http_status,
            "endpoint": probe.endpoint,
        })),
        Ok(probe) => Err((
            format!("HTTP {}", probe.http_status),
            json!({
                "group": profile.label,
                "model": profile.model,
                "httpStatus": probe.http_status,
                "endpoint": probe.endpoint,
            }),
        )),
        Err(error) => Err((
            error.to_string(),
            json!({
                "group": profile.label,
                "model": profile.model,
                "httpStatus": 0,
                "endpoint": null,
            }),
        )),
    }
}

#[tauri::command]
pub async fn enable_mirror_access(
    api_key: String,
    mode: codex_plus_core::mirror_access::MirrorAccessMode,
    selected_model_ids: Option<Vec<String>>,
    default_model: Option<String>,
    key_groups: Option<Vec<MirrorKeyGroupRequest>>,
    imagegen_enabled: Option<bool>,
    image_api_key: Option<String>,
    replace_existing_groups: Option<bool>,
) -> CommandResult<Value> {
    let preflight = get_mirror_preflight();
    let ready = preflight
        .payload
        .get("preflight")
        .and_then(|value| value.get("ready"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !ready {
        return failed(
            "安装前体检未通过，请先处理环境检查项。",
            json!({ "access": null, "models": [], "preflight": preflight.payload.get("preflight") }),
        );
    }
    if mode == codex_plus_core::mirror_access::MirrorAccessMode::MixedApi {
        let auth = inspect_mixed_chatgpt_auth().await;
        if !auth.ready {
            let message = auth.message.clone();
            return failed(
                &message,
                json!({
                    "access": null,
                    "models": [],
                    "blockReason": "mixed_chatgpt_login_required",
                    "mixedAuth": auth,
                }),
            );
        }
    }
    let mut access_groups = Vec::new();
    let mut cached_validations = Vec::new();
    let mut validation_discoveries = Vec::new();
    if let Some(key_groups) = key_groups.filter(|groups| !groups.is_empty()) {
        for group in key_groups {
            let cached_validation = recent_mirror_key_validation(&group.api_key).await;
            let discovery = match &cached_validation {
                Some(validation) => validation.discovery.clone(),
                None => match codex_plus_core::mirror_access::discover_models(&group.api_key).await
                {
                    Ok(discovery) => discovery,
                    Err(error) => {
                        return failed(
                            &format!("分组「{}」启用前验证失败：{error}", group.label),
                            json!({ "access": null, "models": [] }),
                        );
                    }
                },
            };
            validation_discoveries.push(discovery.clone());
            let group_default = if group.selected_model_ids.contains(&discovery.default_model) {
                discovery.default_model.clone()
            } else {
                group
                    .selected_model_ids
                    .first()
                    .cloned()
                    .unwrap_or_default()
            };
            let discovery = match codex_plus_core::mirror_access::select_models(
                discovery,
                &group.selected_model_ids,
                &group_default,
            ) {
                Ok(discovery) => discovery,
                Err(error) => {
                    return failed(
                        &format!("分组「{}」模型选择无效：{error}", group.label),
                        json!({ "access": null, "models": [] }),
                    );
                }
            };
            access_groups.push(codex_plus_core::mirror_access::MirrorAccessGroup {
                id: group.id,
                label: group.label,
                api_key: group.api_key,
                discovery,
            });
            cached_validations.push(cached_validation);
        }
    } else {
        let cached_validation = recent_mirror_key_validation(&api_key).await;
        let discovery = match &cached_validation {
            Some(validation) => validation.discovery.clone(),
            None => match codex_plus_core::mirror_access::discover_models(&api_key).await {
                Ok(discovery) => discovery,
                Err(error) => {
                    return failed(
                        &format!("启用前验证失败：{error}"),
                        json!({ "access": null, "models": [] }),
                    );
                }
            },
        };
        validation_discoveries.push(discovery.clone());
        let selected_model_ids = selected_model_ids.unwrap_or_else(|| {
            discovery
                .models
                .iter()
                .map(|model| model.id.clone())
                .collect()
        });
        let group_default = if selected_model_ids.contains(&discovery.default_model) {
            discovery.default_model.clone()
        } else {
            selected_model_ids.first().cloned().unwrap_or_default()
        };
        let discovery = match codex_plus_core::mirror_access::select_models(
            discovery,
            &selected_model_ids,
            &group_default,
        ) {
            Ok(discovery) => discovery,
            Err(error) => {
                return failed(
                    &format!("模型选择无效：{error}"),
                    json!({ "access": null, "models": [] }),
                );
            }
        };
        access_groups.push(codex_plus_core::mirror_access::MirrorAccessGroup {
            id: "default".to_string(),
            label: "镜子AI".to_string(),
            api_key: api_key.clone(),
            discovery,
        });
        cached_validations.push(cached_validation);
    }
    let default_model = default_model.unwrap_or_else(|| {
        access_groups
            .iter()
            .flat_map(|group| group.discovery.models.iter())
            .map(|model| model.id.clone())
            .next()
            .unwrap_or_default()
    });
    let validated_image_key = if imagegen_enabled == Some(true) {
        match codex_plus_core::imagegen_skill::validate_saved_or_provided_key(
            image_api_key.as_deref(),
        )
        .await
        {
            Ok(key) => Some(key),
            Err(error) => {
                return failed(
                    &format!(
                        "镜子AI Image Key 启用前权限复检失败：{error}。本次未写入任何文件，也未发送真实生图请求。"
                    ),
                    json!({
                        "access": null,
                        "models": [],
                        "imagegen": null,
                    }),
                );
            }
        }
    } else {
        None
    };
    let mut preflight_response_probes = Vec::with_capacity(access_groups.len());
    for (index, group) in access_groups.iter().enumerate() {
        if let Some(validation) = &cached_validations[index] {
            preflight_response_probes.push(validation.response_probe(&group.label, Instant::now()));
            continue;
        }
        let profile = match codex_plus_core::mirror_access::probe_profile_for_group(group) {
            Ok(profile) => profile,
            Err(error) => {
                return failed(
                    &format!("分组「{}」启用前探测配置无效：{error}", group.label),
                    json!({
                        "access": null,
                        "models": [],
                        "preflightResponseProbes": preflight_response_probes,
                    }),
                );
            }
        };
        match probe_mirror_profile_stream(&profile).await {
            Ok(mut probe) => {
                let http_status = probe
                    .get("httpStatus")
                    .and_then(Value::as_u64)
                    .unwrap_or_default() as u16;
                let endpoint = probe
                    .get("endpoint")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                store_mirror_key_validation(
                    &group.api_key,
                    validation_discoveries[index].clone(),
                    profile.model.clone(),
                    http_status,
                    endpoint,
                )
                .await;
                if let Some(object) = probe.as_object_mut() {
                    object.insert("source".to_string(), json!("enablePreflight"));
                    object.insert("reused".to_string(), json!(false));
                }
                preflight_response_probes.push(probe);
            }
            Err((detail, failed_probe)) => {
                return failed(
                    &format!(
                        "分组「{}」启用前真实流式 /responses 验证失败：{detail}。本次未写入任何文件。",
                        profile.label
                    ),
                    json!({
                        "access": null,
                        "models": [],
                        "preflightResponseProbes": preflight_response_probes,
                        "failedResponseProbe": failed_probe,
                    }),
                );
            }
        }
    }
    let _guard = mirror_access_mutex().lock().await;
    if let Some(result) = reject_mirror_mutation_while_codex_runs("启用镜子AI接入") {
        return result;
    }
    if let Some(result) = reject_invalid_codex_state_locations(true) {
        return result;
    }
    let home = codex_plus_core::relay_config::default_codex_home_dir();
    if let Some(result) = reject_mirror_project_provider_overrides(&home) {
        return result;
    }
    if mode == codex_plus_core::mirror_access::MirrorAccessMode::MixedApi {
        let auth = inspect_mixed_chatgpt_auth().await;
        if !auth.ready {
            let message = auth.message.clone();
            return failed(
                &format!("写入前登录复检未通过：{message} 本次未改写任何 Codex 配置、插件或会话。"),
                json!({
                    "access": null,
                    "models": [],
                    "blockReason": "mixed_chatgpt_login_required",
                    "mixedAuth": auth,
                }),
            );
        }
    }
    let state_dir = codex_plus_core::paths::default_app_state_dir();
    let settings_path = codex_plus_core::paths::default_settings_path();
    if let Err(error) = codex_plus_core::codex_app_state::capture_app_state_snapshot(&home) {
        return failed(
            &format!(
                "启用已停止：无法创建 Codex 界面状态恢复快照：{error:#}。未改写配置、插件或会话。"
            ),
            json!({ "access": null, "models": [], "responseProbes": [] }),
        );
    }
    let enable_result = if replace_existing_groups.unwrap_or(false) {
        codex_plus_core::mirror_access::enable_grouped_access_transaction_replacing_groups(
            &home,
            &state_dir,
            &settings_path,
            mode,
            access_groups,
            &default_model,
        )
    } else {
        codex_plus_core::mirror_access::enable_grouped_access_transaction(
            &home,
            &state_dir,
            &settings_path,
            mode,
            access_groups,
            &default_model,
        )
    };
    let mut enable_transaction = match enable_result {
        Ok(result) => result,
        Err(error) => {
            return failed(
                &format!("启用失败：{error}"),
                json!({ "access": null, "models": [], "responseProbes": [] }),
            );
        }
    };
    let enabled = enable_transaction.result.clone();
    let grouped_access = enable_transaction.probe_profiles.len() > 1;
    let response_probes = preflight_response_probes.clone();
    let configuration_readback = json!({
        "verified": true,
        "networkRequestSent": false,
        "checks": ["config.toml", "auth.json", "manager settings", "model catalog", "managed state"],
    });
    if let Some(result) = pause_mirror_enable_if_codex_started(
        "接入配置写入",
        &home,
        &state_dir,
        &enabled.models,
        &preflight_response_probes,
    ) {
        return result;
    }
    let marketplace = match codex_plus_core::plugin_marketplace::ensure_openai_curated_remote_marketplace_available(
        &home,
    ) {
        Ok(marketplace) => marketplace,
        Err(error) => {
            if let Some(result) = pause_mirror_enable_if_codex_started(
                "插件市场准备",
                &home,
                &state_dir,
                &enabled.models,
                &response_probes,
            ) {
                return result;
            }
            let rollback = enable_transaction.rollback(&home, &state_dir, &settings_path);
            let (message, access) = match rollback {
                Ok(access) => (
                    format!(
                        "Codex 必需插件市场准备失败：{error}。已恢复到本次操作前状态。"
                    ),
                    access,
                ),
                Err(rollback_error) => (
                    format!(
                        "Codex 必需插件市场准备失败：{error}，且自动回滚未完整完成：{rollback_error}。操作快照已保留，请勿反复重试。"
                    ),
                    codex_plus_core::mirror_access::access_status(&home, &state_dir),
                ),
            };
            return failed(
                &message,
                json!({
                    "access": access,
                    "models": enabled.models,
                    "responseProbes": response_probes,
                    "preflightResponseProbes": preflight_response_probes,
                    "pluginMarketplaceReady": false,
                }),
            );
        }
    };
    enable_transaction.record_plugin_marketplace_initialization(marketplace.initialized);
    if let Some(result) = pause_mirror_enable_if_codex_started(
        "插件市场准备",
        &home,
        &state_dir,
        &enabled.models,
        &response_probes,
    ) {
        return result;
    }
    let marketplace_ready = true;
    let mut marketplace_message = if marketplace.initialized {
        "插件市场已初始化并注册。".to_string()
    } else {
        "插件市场配置已保留。".to_string()
    };
    let imagegen_result = match imagegen_enabled {
        Some(true) => codex_plus_core::imagegen_skill::enable(
            &home,
            &state_dir,
            validated_image_key.as_deref(),
        ),
        Some(false) => codex_plus_core::imagegen_skill::disable(&home, &state_dir),
        None => Ok(()),
    };
    if let Err(error) = imagegen_result {
        if let Some(result) = pause_mirror_enable_if_codex_started(
            "生图配置",
            &home,
            &state_dir,
            &enabled.models,
            &response_probes,
        ) {
            return result;
        }
        let rollback = enable_transaction.rollback(&home, &state_dir, &settings_path);
        let (message, access) = match rollback {
            Ok(access) => (
                format!(
                    "镜子AI生图功能配置失败：{error}。文本模型接入已恢复到本次操作前状态，请修正后重试。"
                ),
                access,
            ),
            Err(rollback_error) => (
                format!(
                    "镜子AI生图功能配置失败：{error}，且文本模型配置自动回滚未完整完成：{rollback_error}。操作快照已保留，请勿反复重试。"
                ),
                codex_plus_core::mirror_access::access_status(&home, &state_dir),
            ),
        };
        return failed(
            &message,
            json!({
                "access": access,
                "models": enabled.models,
                "responseProbes": response_probes,
                "preflightResponseProbes": preflight_response_probes,
                "imagegen": codex_plus_core::imagegen_skill::status(&home, &state_dir),
            }),
        );
    }
    let imagegen_status = codex_plus_core::imagegen_skill::status(&home, &state_dir);
    if let Some(result) = pause_mirror_enable_if_codex_started(
        "生图配置",
        &home,
        &state_dir,
        &enabled.models,
        &response_probes,
    ) {
        return result;
    }
    if let Err(error) =
        codex_plus_core::plugin_marketplace::commit_openai_curated_remote_marketplace_initialization(
            &home,
        )
    {
        marketplace_message.push_str(&format!(
            "旧插件缓存备份暂未清理（{error}），新缓存仍可用且备份已保留。"
        ));
    }
    if let Some(result) = pause_mirror_enable_if_codex_started(
        "会话修复",
        &home,
        &state_dir,
        &enabled.models,
        &response_probes,
    ) {
        return result;
    }
    let sync = codex_plus_data::provider_sync::run_provider_sync_with_target(
        Some(&home),
        Some(codex_plus_core::mirror_access::MIRROR_PROVIDER_ID),
    );
    let synced = is_success_sync_status(&sync.status);
    if synced {
        codex_plus_core::codex_app_state::sync_app_state_after_provider_switch_nonfatal(
            &home,
            "manager.enable_mirror_access.after",
        );
    }
    let inserted_model_count = enabled.models.len();
    let restart_message = if grouped_access {
        "请完全退出 Codex，然后点击本工具的“打开 Codex”启动本地分组路由。"
    } else {
        "请完全退出并重新打开 Codex 后选择模型。"
    };
    let sync_message = if synced {
        format!(
            "已向 Codex 插入 {inserted_model_count} 个模型；修复了 {} 个会话文件和 {} 行索引。{}{}{restart_message}",
            sync.changed_session_files,
            sync.sqlite_rows_updated,
            marketplace_message,
            if imagegen_status.enabled {
                "镜子AI生图 Skill 已启用。"
            } else {
                ""
            },
        )
    } else {
        format!(
            "已向 Codex 插入 {inserted_model_count} 个模型，但会话修复未完整完成：{}；{}{}{restart_message}",
            sync.message,
            marketplace_message,
            if imagegen_status.enabled {
                "镜子AI生图 Skill 已启用。"
            } else {
                ""
            },
        )
    };
    let access = match codex_plus_core::mirror_access::record_session_sync(
        &home,
        &state_dir,
        synced,
        &sync_message,
    ) {
        Ok(access) => access,
        Err(error) => {
            return degraded(
                &format!(
                    "模型配置已经生效，但会话修复状态无法保存：{error}。Codex 配置未回滚；请先保留当前窗口并修复 managed-access.json，再重试会话修复。"
                ),
                json!({
                    "access": codex_plus_core::mirror_access::access_status(&home, &state_dir),
                    "models": enabled.models,
                    "responseProbes": response_probes,
                    "preflightResponseProbes": preflight_response_probes,
                    "configurationReadback": configuration_readback,
                    "sessionSync": sync,
                    "pluginMarketplaceReady": marketplace_ready,
                    "imagegen": imagegen_status,
                }),
            );
        }
    };
    let fully_ready = access.active;
    let payload = json!({
        "access": access,
        "models": enabled.models,
        "responseProbes": response_probes,
        "preflightResponseProbes": preflight_response_probes,
        "configurationReadback": configuration_readback,
        "sessionSync": sync,
        "pluginMarketplaceReady": marketplace_ready,
        "imagegen": imagegen_status,
        "fullyReady": fully_ready,
    });
    if synced {
        ok(&sync_message, payload)
    } else {
        degraded(&sync_message, payload)
    }
}

fn probe_writable_directory(path: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(path)?;
    let probe = path.join(format!(
        ".mirrorplus-write-probe-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let result = OpenOptions::new().write(true).create_new(true).open(&probe);
    match result {
        Ok(file) => {
            drop(file);
            fs::remove_file(probe)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn should_probe_codex_state_directories(codex_installed: bool, codex_running: bool) -> bool {
    codex_installed && !codex_running
}

fn reject_mirror_project_provider_overrides(home: &Path) -> Option<CommandResult<Value>> {
    let scan = codex_plus_core::project_config::scan_recent_project_configs(
        home,
        codex_plus_core::mirror_access::MIRROR_PROVIDER_ID,
        64,
    );
    let overrides = scan
        .overrides
        .iter()
        .filter(|entry| entry.changes_active_provider)
        .collect::<Vec<_>>();
    let first = overrides.first()?;
    Some(failed(
        &format!(
            "近期项目配置会覆盖 mirrorplus 路由，本次未写入任何文件。请先处理 {}：{}",
            first.config_path, first.detail
        ),
        json!({
            "access": null,
            "models": [],
            "projectConfigScan": scan,
        }),
    ))
}

#[tauri::command]
pub async fn repair_mirror_sessions() -> CommandResult<Value> {
    let _guard = mirror_access_mutex().lock().await;
    if let Some(result) = reject_mirror_mutation_while_codex_runs("修复会话") {
        return result;
    }
    if let Some(result) = reject_invalid_codex_state_locations(true) {
        return result;
    }
    let home = codex_plus_core::relay_config::default_codex_home_dir();
    let state_dir = codex_plus_core::paths::default_app_state_dir();
    let status = match codex_plus_core::mirror_access::try_access_status(&home, &state_dir) {
        Ok(status) => status,
        Err(error) => {
            return failed(
                &format!("接管状态无法读取，未修改任何会话：{error}"),
                json!({
                    "access": codex_plus_core::mirror_access::access_status(&home, &state_dir),
                    "sessionSync": null,
                }),
            );
        }
    };
    let target = if status.active {
        codex_plus_core::mirror_access::MIRROR_PROVIDER_ID.to_string()
    } else {
        status
            .original_provider
            .clone()
            .unwrap_or_else(|| status.current_provider.clone())
    };
    if let Err(error) = codex_plus_core::codex_app_state::capture_app_state_snapshot(&home) {
        return failed(
            &format!(
                "会话修复已停止：无法创建 Codex 界面状态恢复快照：{error:#}。未修改任何会话。"
            ),
            json!({
                "access": status,
                "sessionSync": null,
            }),
        );
    }
    let sync =
        codex_plus_data::provider_sync::run_provider_sync_with_target(Some(&home), Some(&target));
    let synced = is_success_sync_status(&sync.status);
    if synced {
        codex_plus_core::codex_app_state::sync_app_state_after_provider_switch_nonfatal(
            &home,
            "manager.repair_mirror_sessions.after",
        );
    }
    let message = if synced {
        format!(
            "会话修复完成：{} 个文件，{} 行索引。",
            sync.changed_session_files, sync.sqlite_rows_updated
        )
    } else {
        format!("会话修复未完成：{}", sync.message)
    };
    let access = match codex_plus_core::mirror_access::record_session_sync(
        &home, &state_dir, synced, &message,
    ) {
        Ok(access) => access,
        Err(error) => {
            return degraded(
                &format!("{message}，但接管状态记录失败：{error}"),
                json!({
                    "access": codex_plus_core::mirror_access::access_status(&home, &state_dir),
                    "sessionSync": sync,
                }),
            );
        }
    };
    if synced {
        ok(&message, json!({ "access": access, "sessionSync": sync }))
    } else {
        degraded(&message, json!({ "access": access, "sessionSync": sync }))
    }
}

#[tauri::command]
pub async fn preview_session_index_cleanup() -> CommandResult<Value> {
    let result = tauri::async_runtime::spawn_blocking(|| {
        codex_plus_data::preview_session_index_cleanup(None)
    })
    .await
    .map_err(|error| anyhow::anyhow!("session index cleanup preview task failed: {error}"))
    .and_then(|result| result);
    match result {
        Ok(preview) => ok(
            &format!("发现 {} 条失效会话索引候选。", preview.candidates.len()),
            json!({
                "snapshotSha256": preview.snapshot_sha256,
                "candidates": preview.candidates,
            }),
        ),
        Err(error) => failed(&format!("预览失效会话索引失败：{error}"), json!({})),
    }
}

#[tauri::command]
pub async fn apply_session_index_cleanup(
    snapshot_sha256: String,
    thread_ids: Vec<String>,
) -> CommandResult<Value> {
    let result = tauri::async_runtime::spawn_blocking(move || {
        codex_plus_data::apply_session_index_cleanup(None, &snapshot_sha256, &thread_ids)
    })
    .await
    .map_err(|error| anyhow::anyhow!("session index cleanup task failed: {error}"));
    match result {
        Ok(Ok(cleanup)) => ok(
            &format!(
                "已清理 {} 条失效会话索引；原索引已完整备份。",
                cleanup.pruned_entries
            ),
            json!({
                "prunedEntries": cleanup.pruned_entries,
                "backupDir": cleanup.backup_dir,
            }),
        ),
        Ok(Err(error)) => failed(
            &format!("清理失效会话索引失败：{}", error.message),
            json!({ "backupDir": error.backup_dir }),
        ),
        Err(error) => failed(&format!("清理失效会话索引失败：{error}"), json!({})),
    }
}

#[tauri::command]
pub async fn restore_pre_mirror_state() -> CommandResult<Value> {
    let _guard = mirror_access_mutex().lock().await;
    if let Some(result) = reject_mirror_mutation_while_codex_runs("恢复原始状态") {
        return result;
    }
    if let Some(result) = reject_invalid_codex_state_locations(false) {
        return result;
    }
    let home = codex_plus_core::relay_config::default_codex_home_dir();
    let state_dir = codex_plus_core::paths::default_app_state_dir();
    let settings_path = codex_plus_core::paths::default_settings_path();
    if let Err(error) = codex_plus_core::codex_app_state::capture_app_state_snapshot(&home) {
        return failed(
            &format!(
                "恢复已停止：无法创建 Codex 界面状态恢复快照：{error:#}。未修改任何配置或会话。"
            ),
            json!({ "access": codex_plus_core::mirror_access::access_status(&home, &state_dir) }),
        );
    }
    let restored = match codex_plus_core::mirror_access::restore_access(
        &home,
        &state_dir,
        &settings_path,
    ) {
        Ok(result) => result,
        Err(error) => {
            return failed(
                &format!("恢复失败：{error}"),
                json!({ "access": codex_plus_core::mirror_access::access_status(&home, &state_dir) }),
            );
        }
    };
    if let Err(error) = codex_plus_core::imagegen_skill::restore_baseline(&home, &state_dir) {
        let message = format!(
            "Codex 主配置已恢复，但生图 Skill 或 Image Key 恢复失败：{error}。当前恢复数据已保留，可直接重试“恢复使用前状态”。"
        );
        let access =
            codex_plus_core::mirror_access::record_session_sync(&home, &state_dir, false, &message)
                .unwrap_or(restored.status);
        return degraded(
            &message,
            json!({
                "access": access,
                "imagegen": codex_plus_core::imagegen_skill::status(&home, &state_dir),
            }),
        );
    }
    let sync = codex_plus_data::provider_sync::run_provider_sync_with_target(
        Some(&home),
        Some(&restored.original_provider),
    );
    let synced = is_success_sync_status(&sync.status);
    if synced {
        codex_plus_core::codex_app_state::sync_app_state_after_provider_switch_nonfatal(
            &home,
            "manager.restore_pre_mirror_state.after",
        );
    }
    let message = if synced {
        "已恢复到首次使用 mirror x codex 前的 Codex 配置，会话归属也已恢复。".to_string()
    } else {
        format!(
            "原始 Codex 配置已恢复，但会话归属需要重试：{}",
            sync.message
        )
    };
    let access = match codex_plus_core::mirror_access::record_session_sync(
        &home, &state_dir, synced, &message,
    ) {
        Ok(access) => access,
        Err(error) => {
            return degraded(
                &format!("{message}，但恢复状态记录失败：{error}"),
                json!({
                    "access": codex_plus_core::mirror_access::access_status(&home, &state_dir),
                    "sessionSync": sync,
                }),
            );
        }
    };
    if synced {
        ok(
            &message,
            json!({
                "access": access,
                "sessionSync": sync,
                "imagegen": codex_plus_core::imagegen_skill::status(&home, &state_dir),
            }),
        )
    } else {
        degraded(
            &message,
            json!({
                "access": access,
                "sessionSync": sync,
                "imagegen": codex_plus_core::imagegen_skill::status(&home, &state_dir),
            }),
        )
    }
}

#[tauri::command]
pub async fn recover_mirror_from_baseline() -> CommandResult<Value> {
    let _guard = mirror_access_mutex().lock().await;
    if let Some(result) = reject_mirror_mutation_while_codex_runs("从还原点恢复") {
        return result;
    }
    if let Some(result) = reject_invalid_codex_state_locations(false) {
        return result;
    }
    let home = codex_plus_core::relay_config::default_codex_home_dir();
    let state_dir = codex_plus_core::paths::default_app_state_dir();
    let settings_path = codex_plus_core::paths::default_settings_path();
    if let Err(error) = codex_plus_core::codex_app_state::capture_app_state_snapshot(&home) {
        return failed(
            &format!(
                "还原点恢复已停止：无法创建 Codex 界面状态恢复快照：{error:#}。未修改任何配置或会话。"
            ),
            json!({ "access": codex_plus_core::mirror_access::access_status(&home, &state_dir) }),
        );
    }
    let restored = match codex_plus_core::mirror_access::recover_access_from_baseline(
        &home,
        &state_dir,
        &settings_path,
    ) {
        Ok(result) => result,
        Err(error) => {
            return failed(
                &format!(
                    "还原点恢复失败：{error}。当前文件和操作快照均已保留，请进入高级诊断查看详情。"
                ),
                json!({ "access": codex_plus_core::mirror_access::access_status(&home, &state_dir) }),
            );
        }
    };
    if let Err(error) = codex_plus_core::imagegen_skill::restore_baseline(&home, &state_dir) {
        let message = format!(
            "Codex 主配置已从校验通过的还原点恢复，但生图配置恢复未完成：{error}。可直接重试本操作。"
        );
        let access =
            codex_plus_core::mirror_access::record_session_sync(&home, &state_dir, false, &message)
                .unwrap_or(restored.status);
        return degraded(
            &message,
            json!({
                "access": access,
                "imagegen": codex_plus_core::imagegen_skill::status(&home, &state_dir),
            }),
        );
    }
    let sync = codex_plus_data::provider_sync::run_provider_sync_with_target(
        Some(&home),
        Some(&restored.original_provider),
    );
    let synced = is_success_sync_status(&sync.status);
    if synced {
        codex_plus_core::codex_app_state::sync_app_state_after_provider_switch_nonfatal(
            &home,
            "manager.recover_mirror_from_baseline.after",
        );
    }
    let message = if synced {
        "已从首次接入还原点恢复 Codex 配置和会话归属；损坏文件的操作前快照已保留。".to_string()
    } else {
        format!(
            "Codex 配置已从首次接入还原点恢复，但会话归属需要重试：{}",
            sync.message
        )
    };
    let access = match codex_plus_core::mirror_access::record_session_sync(
        &home, &state_dir, synced, &message,
    ) {
        Ok(access) => access,
        Err(error) => {
            return degraded(
                &format!("{message}，但恢复状态记录失败：{error}"),
                json!({
                    "access": codex_plus_core::mirror_access::access_status(&home, &state_dir),
                    "sessionSync": sync,
                }),
            );
        }
    };
    let payload = json!({
        "access": access,
        "sessionSync": sync,
        "imagegen": codex_plus_core::imagegen_skill::status(&home, &state_dir),
    });
    if synced {
        ok(&message, payload)
    } else {
        degraded(&message, payload)
    }
}

fn mirror_access_mutex() -> &'static AsyncMutex<()> {
    static LOCK: OnceLock<AsyncMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| AsyncMutex::new(()))
}

fn launch_operation_mutex() -> &'static AsyncMutex<()> {
    static LOCK: OnceLock<AsyncMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| AsyncMutex::new(()))
}

fn windows_sandbox_operation_mutex() -> &'static AsyncMutex<()> {
    static LOCK: OnceLock<AsyncMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| AsyncMutex::new(()))
}

fn pause_mirror_enable_if_codex_started(
    stage: &str,
    home: &Path,
    state_dir: &Path,
    models: &[codex_plus_core::mirror_access::MirrorModel],
    response_probes: &[Value],
) -> Option<CommandResult<Value>> {
    let process_count = codex_plus_core::watcher::find_codex_processes().len();
    pause_mirror_enable_for_process_count(
        process_count,
        stage,
        home,
        state_dir,
        models,
        response_probes,
    )
}

fn pause_mirror_enable_for_process_count(
    process_count: usize,
    stage: &str,
    home: &Path,
    state_dir: &Path,
    models: &[codex_plus_core::mirror_access::MirrorModel],
    response_probes: &[Value],
) -> Option<CommandResult<Value>> {
    (process_count > 0).then(|| {
        degraded(
            &format!(
                "检测到 Codex 在{stage}期间启动（{process_count} 个进程）。为避免同时改写配置或会话，本次已停止后续操作，未继续会话同步，也未在 Codex 运行时自动回滚。当前接入配置和操作快照均已保留；请完全退出 Codex 后重新点击启用，以完成剩余步骤。"
            ),
            json!({
                "access": codex_plus_core::mirror_access::access_status(home, state_dir),
                "models": models,
                "responseProbes": response_probes,
                "preflightResponseProbes": response_probes,
                "sessionSync": null,
                "codexRunning": true,
                "codexProcessCount": process_count,
                "pausedStage": stage,
            }),
        )
    })
}

fn reject_mirror_mutation_while_codex_runs(action: &str) -> Option<CommandResult<Value>> {
    codex_running_mutation_message(action).map(|(process_count, message)| {
        failed(
            &message,
            json!({
                "access": null,
                "codexRunning": true,
                "codexProcessCount": process_count,
            }),
        )
    })
}

fn codex_running_mutation_message(action: &str) -> Option<(usize, String)> {
    let process_count = codex_plus_core::watcher::find_codex_processes().len();
    (process_count > 0).then(|| {
        (
            process_count,
            format!(
                "检测到 Codex 仍在运行（{process_count} 个进程）。请先完全退出 Codex 后再{action}；本次未写入任何文件。"
            ),
        )
    })
}

fn reject_invalid_codex_state_locations(require_sqlite_home: bool) -> Option<CommandResult<Value>> {
    let validation =
        codex_plus_core::codex_home::validate_codex_home_environment().and_then(|_| {
            if require_sqlite_home {
                codex_plus_core::codex_sqlite::validate_codex_sqlite_home_environment()
            } else {
                Ok(())
            }
        });
    validation.err().map(|error| {
        failed(
            &format!("Codex 状态目录检查失败：{error} 本次未写入任何文件。"),
            json!({
                "access": null,
                "codexRunning": false,
                "stateLocationValid": false,
            }),
        )
    })
}

fn is_success_sync_status(status: &codex_plus_data::ProviderSyncStatus) -> bool {
    matches!(status, codex_plus_data::ProviderSyncStatus::Synced)
}

fn persist_provider_sync_selection(provider: &str) -> anyhow::Result<()> {
    let trimmed = provider.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let store = SettingsStore::default();
    let mut settings = store.load().map_err(|error| {
        anyhow::anyhow!("读取 Provider 同步设置失败：{error:#}；未使用默认设置保存本次选择")
    })?;
    settings.provider_sync_last_selected_provider = trimmed.to_string();
    if !settings
        .provider_sync_saved_providers
        .iter()
        .any(|item| item == trimmed)
    {
        settings
            .provider_sync_saved_providers
            .push(trimmed.to_string());
    }
    settings.provider_sync_saved_providers =
        normalize_provider_sync_provider_list(settings.provider_sync_saved_providers);
    store.save(&settings)
}

#[tauri::command]
pub async fn load_ads() -> CommandResult<AdsPayload> {
    match codex_plus_core::ads::fetch_ad_list().await {
        Ok(payload) => ok("推荐内容已加载。", ads_payload(payload)),
        Err(error) => failed(
            &format!("推荐内容加载失败：{error}"),
            AdsPayload {
                version: 1,
                ads: Vec::new(),
            },
        ),
    }
}

#[tauri::command]
pub async fn refresh_script_market() -> CommandResult<ScriptMarketPayload> {
    match script_market::fetch_market_manifest(script_market::DEFAULT_MARKET_INDEX_URL).await {
        Ok(manifest) => ok(
            "脚本市场已刷新。",
            script_market_payload_from_manifest(&manifest, "ok", "脚本市场已刷新。"),
        ),
        Err(error) => failed(
            &format!("脚本市场加载失败：{error}"),
            failed_script_market_payload(&format!("脚本市场加载失败：{error}")),
        ),
    }
}

#[tauri::command]
pub async fn install_market_script(id: String) -> CommandResult<ScriptMarketPayload> {
    let trimmed = id.trim();
    if trimmed.is_empty() {
        return failed(
            "脚本 id 不能为空。",
            failed_script_market_payload("脚本 id 不能为空。"),
        );
    }
    let manifest =
        match script_market::fetch_market_manifest(script_market::DEFAULT_MARKET_INDEX_URL).await {
            Ok(manifest) => manifest,
            Err(error) => {
                return failed(
                    &format!("脚本市场加载失败：{error}"),
                    failed_script_market_payload(&format!("脚本市场加载失败：{error}")),
                );
            }
        };
    let Some(script) = manifest.scripts.iter().find(|script| script.id == trimmed) else {
        return failed(
            "市场清单中未找到该脚本。",
            script_market_payload_from_manifest(&manifest, "failed", "市场清单中未找到该脚本。"),
        );
    };
    let manager = default_user_script_manager();
    match script_market::install_market_script(&manager, script).await {
        Ok(()) => ok(
            "脚本已安装。",
            script_market_payload_from_manifest(&manifest, "ok", "脚本已安装。"),
        ),
        Err(error) => failed(
            &format!("安装脚本失败：{error}"),
            script_market_payload_from_manifest(
                &manifest,
                "failed",
                &format!("安装脚本失败：{error}"),
            ),
        ),
    }
}

#[tauri::command]
pub fn set_user_script_enabled(key: String, enabled: bool) -> CommandResult<SettingsPayload> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return failed("脚本 key 不能为空。", fallback_settings_payload());
    }
    let manager = default_user_script_manager();
    match manager.set_script_enabled(trimmed, enabled) {
        Ok(_) => settings_payload(
            if enabled {
                "脚本已启用。"
            } else {
                "脚本已禁用。"
            },
            "脚本启停失败",
        ),
        Err(error) => failed(
            &format!("脚本启停失败：{error}"),
            fallback_settings_payload(),
        ),
    }
}

#[tauri::command]
pub fn delete_user_script(key: String) -> CommandResult<SettingsPayload> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return failed("脚本 key 不能为空。", fallback_settings_payload());
    }
    let manager = default_user_script_manager();
    match manager.delete_user_script(trimmed) {
        Ok(_) => settings_payload("脚本已删除。", "脚本删除失败"),
        Err(error) => failed(
            &format!("脚本删除失败：{error}"),
            fallback_settings_payload(),
        ),
    }
}

#[tauri::command]
pub fn open_external_url(url: String) -> CommandResult<Value> {
    let trimmed = url.trim();
    if !(trimmed.starts_with("https://") || trimmed.starts_with("http://")) {
        return failed("只允许打开 http 或 https 链接。", json!({}));
    }
    match open_url(trimmed) {
        Ok(()) => ok("已在系统浏览器打开链接。", json!({ "url": trimmed })),
        Err(error) => failed(&format!("打开链接失败：{error}"), json!({ "url": trimmed })),
    }
}

#[tauri::command]
pub async fn install_entrypoints() -> InstallActionResult {
    tauri::async_runtime::spawn_blocking(install::install_entrypoints)
        .await
        .unwrap_or_else(|error| install_background_failure("安装入口", error))
}

#[tauri::command]
pub async fn uninstall_entrypoints(options: InstallOptions) -> InstallActionResult {
    tauri::async_runtime::spawn_blocking(move || install::uninstall_entrypoints(options))
        .await
        .unwrap_or_else(|error| install_background_failure("卸载入口", error))
}

#[tauri::command]
pub async fn repair_shortcuts() -> InstallActionResult {
    tauri::async_runtime::spawn_blocking(install::repair_shortcuts)
        .await
        .unwrap_or_else(|error| install_background_failure("修复快捷方式", error))
}

#[tauri::command]
pub fn plugin_marketplace_status() -> CommandResult<PluginMarketplaceStatusPayload> {
    let home = codex_plus_core::codex_home::default_codex_home_dir();
    let status = codex_plus_core::plugin_marketplace::openai_curated_marketplace_status(&home);
    ok(
        if status.needs_repair() {
            "插件市场需要初始化或注册。"
        } else {
            "插件市场已可用。"
        },
        PluginMarketplaceStatusPayload {
            codex_home: home.to_string_lossy().to_string(),
            marketplace_root: status
                .marketplace_root
                .as_ref()
                .map(|path| path.to_string_lossy().to_string()),
            config_registered: status.config_registered,
            needs_repair: status.needs_repair(),
        },
    )
}

#[tauri::command]
pub async fn repair_plugin_marketplace() -> CommandResult<PluginMarketplaceRepairPayload> {
    let home = codex_plus_core::codex_home::default_codex_home_dir();
    match codex_plus_core::plugin_marketplace::initialize_openai_curated_marketplace_and_configure(
        &home,
    )
    .await
    {
        Ok(result) => ok(
            if result.initialized {
                "插件市场已从 openai/plugins 初始化并注册。"
            } else if result.configured {
                "已注册本地插件市场。"
            } else {
                "插件市场已可用，无需修复。"
            },
            PluginMarketplaceRepairPayload {
                codex_home: home.to_string_lossy().to_string(),
                marketplace_root:
                    codex_plus_core::plugin_marketplace::openai_curated_marketplace_status(&home)
                        .marketplace_root
                        .as_ref()
                        .map(|path| path.to_string_lossy().to_string()),
                initialized: result.initialized,
                configured: result.configured,
                needs_repair: false,
            },
        ),
        Err(error) => failed(
            &format!("插件市场修复失败：{error}"),
            PluginMarketplaceRepairPayload {
                codex_home: home.to_string_lossy().to_string(),
                marketplace_root:
                    codex_plus_core::plugin_marketplace::openai_curated_marketplace_status(&home)
                        .marketplace_root
                        .as_ref()
                        .map(|path| path.to_string_lossy().to_string()),
                initialized: false,
                configured: false,
                needs_repair: true,
            },
        ),
    }
}

#[tauri::command]
pub fn remote_plugin_marketplace_status() -> CommandResult<RemotePluginMarketplacePayload> {
    let home = codex_plus_core::codex_home::default_codex_home_dir();
    let status =
        codex_plus_core::plugin_marketplace::openai_curated_remote_marketplace_status(&home);
    let (plugin_count, skill_count) =
        remote_plugin_marketplace_counts(status.marketplace_root.as_deref());
    ok(
        if status.needs_repair() {
            "官方远端插件缓存需要释放或注册。"
        } else {
            "官方远端插件缓存已可用。"
        },
        RemotePluginMarketplacePayload {
            codex_home: home.to_string_lossy().to_string(),
            marketplace_root: status
                .marketplace_root
                .as_ref()
                .map(|path| path.to_string_lossy().to_string()),
            config_registered: status.config_registered,
            needs_repair: status.needs_repair(),
            plugin_count,
            skill_count,
        },
    )
}

#[tauri::command]
pub fn repair_remote_plugin_marketplace() -> CommandResult<RemotePluginMarketplacePayload> {
    let home = codex_plus_core::codex_home::default_codex_home_dir();
    match codex_plus_core::plugin_marketplace::ensure_openai_curated_remote_marketplace_available(
        &home,
    ) {
        Ok(result) => {
            if let Err(error) = codex_plus_core::plugin_marketplace::commit_openai_curated_remote_marketplace_initialization(
                &home,
            ) {
                let status = codex_plus_core::plugin_marketplace::openai_curated_remote_marketplace_status(
                    &home,
                );
                let (plugin_count, skill_count) =
                    remote_plugin_marketplace_counts(status.marketplace_root.as_deref());
                return failed(
                    &format!(
                        "官方远端插件缓存已修复，但旧缓存备份暂时无法清理：{error}。新缓存可继续使用，备份仍保留。"
                    ),
                    RemotePluginMarketplacePayload {
                        codex_home: home.to_string_lossy().to_string(),
                        marketplace_root: status
                            .marketplace_root
                            .as_ref()
                            .map(|path| path.to_string_lossy().to_string()),
                        config_registered: status.config_registered,
                        needs_repair: status.needs_repair(),
                        plugin_count,
                        skill_count,
                    },
                );
            }
            let status =
                codex_plus_core::plugin_marketplace::openai_curated_remote_marketplace_status(
                    &home,
                );
            let (plugin_count, skill_count) =
                remote_plugin_marketplace_counts(status.marketplace_root.as_deref());
            ok(
                if result.initialized {
                    "已释放并注册内置官方远端插件缓存。"
                } else if result.configured {
                    "已注册官方远端插件缓存。"
                } else {
                    "官方远端插件缓存已可用，无需修复。"
                },
                RemotePluginMarketplacePayload {
                    codex_home: home.to_string_lossy().to_string(),
                    marketplace_root: status
                        .marketplace_root
                        .as_ref()
                        .map(|path| path.to_string_lossy().to_string()),
                    config_registered: status.config_registered,
                    needs_repair: status.needs_repair(),
                    plugin_count,
                    skill_count,
                },
            )
        }
        Err(error) => {
            let status =
                codex_plus_core::plugin_marketplace::openai_curated_remote_marketplace_status(
                    &home,
                );
            let (plugin_count, skill_count) =
                remote_plugin_marketplace_counts(status.marketplace_root.as_deref());
            failed(
                &format!("官方远端插件缓存修复失败：{error}"),
                RemotePluginMarketplacePayload {
                    codex_home: home.to_string_lossy().to_string(),
                    marketplace_root: status
                        .marketplace_root
                        .as_ref()
                        .map(|path| path.to_string_lossy().to_string()),
                    config_registered: status.config_registered,
                    needs_repair: status.needs_repair(),
                    plugin_count,
                    skill_count,
                },
            )
        }
    }
}

fn remote_plugin_marketplace_counts(root: Option<&Path>) -> (usize, usize) {
    let Some(root) = root else {
        return (0, 0);
    };
    let marketplace_path = root
        .join(".agents")
        .join("plugins")
        .join("marketplace.json");
    let plugin_count = std::fs::read_to_string(&marketplace_path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|marketplace| {
            marketplace
                .get("plugins")
                .and_then(Value::as_array)
                .map(Vec::len)
        })
        .unwrap_or(0);
    let skill_count = count_skill_files(&root.join("plugins")).unwrap_or(0);
    (plugin_count, skill_count)
}

fn count_skill_files(root: &Path) -> std::io::Result<usize> {
    if !root.is_dir() {
        return Ok(0);
    }
    let mut total = 0;
    for entry in std::fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            total += count_skill_files(&path)?;
        } else if path.file_name().and_then(|name| name.to_str()) == Some("SKILL.md") {
            total += 1;
        }
    }
    Ok(total)
}

#[tauri::command]
pub async fn check_update() -> CommandResult<Value> {
    match codex_plus_core::update::check_for_update(codex_plus_core::version::VERSION).await {
        Ok(update) => {
            let status = if update.update_available {
                "ok"
            } else {
                "not_checked"
            };
            CommandResult {
                status: status.to_string(),
                message: if update.update_available {
                    "发现可用更新。".to_string()
                } else {
                    "当前已是最新版本。".to_string()
                },
                payload: json!({
                    "currentVersion": update.current_version,
                    "latestVersion": update.latest_version,
                    "releaseSummary": update.release_summary,
                    "assetName": update.asset_name,
                    "assetUrl": update.asset_url,
                    "updateAvailable": update.update_available,
                    "progress": 0
                }),
            }
        }
        Err(error) => failed(
            &format!("检查更新失败：{error}"),
            json!({
                "currentVersion": codex_plus_core::version::VERSION,
                "latestVersion": Value::Null,
                "releaseSummary": "",
                "assetName": Value::Null,
                "assetUrl": Value::Null,
                "updateAvailable": false,
                "progress": 0
            }),
        ),
    }
}

#[tauri::command]
pub async fn perform_update(
    release: Option<codex_plus_core::update::Release>,
) -> CommandResult<Value> {
    let Some(release) = release else {
        return failed(
            "请先检查更新并选择可下载的 Release asset。",
            json!({
                "currentVersion": codex_plus_core::version::VERSION,
                "progress": 0
            }),
        );
    };
    let download_dir = codex_plus_core::paths::default_app_state_dir().join("updates");
    match codex_plus_core::update::perform_update(&release, &download_dir).await {
        Ok(result) => ok(
            "安装包已下载并启动，请按安装向导完成更新。",
            json!({
                "currentVersion": codex_plus_core::version::VERSION,
                "latestVersion": result.release.version,
                "releaseSummary": result.release.body,
                "installedPath": result.installer_path.to_string_lossy(),
                "launched": result.launched,
                "progress": 100
            }),
        ),
        Err(error) => failed(
            &format!("安装更新失败：{error}"),
            json!({
                "currentVersion": codex_plus_core::version::VERSION,
                "latestVersion": release.version,
                "releaseSummary": release.body,
                "progress": 0
            }),
        ),
    }
}

#[tauri::command]
pub fn load_watcher_state() -> CommandResult<WatcherPayload> {
    ok("watcher 状态已加载。", watcher_payload())
}

#[tauri::command]
pub fn install_watcher() -> CommandResult<WatcherPayload> {
    let launcher_path =
        codex_plus_core::install::companion_binary_path(codex_plus_core::install::SILENT_BINARY);
    match codex_plus_core::watcher::install_watcher(&launcher_path, default_debug_port()) {
        Ok(()) => ok("watcher 已安装。", watcher_payload()),
        Err(error) => failed(&format!("安装 watcher 失败：{error}"), watcher_payload()),
    }
}

#[tauri::command]
pub fn uninstall_watcher() -> CommandResult<WatcherPayload> {
    match codex_plus_core::watcher::uninstall_watcher() {
        Ok(()) => ok("watcher 已移除。", watcher_payload()),
        Err(error) => failed(&format!("移除 watcher 失败：{error}"), watcher_payload()),
    }
}

#[tauri::command]
pub fn enable_watcher() -> CommandResult<WatcherPayload> {
    match codex_plus_core::watcher::enable_watcher() {
        Ok(()) => ok("watcher 已启用。", watcher_payload()),
        Err(error) => failed(&format!("启用 watcher 失败：{error}"), watcher_payload()),
    }
}

#[tauri::command]
pub fn disable_watcher() -> CommandResult<WatcherPayload> {
    match codex_plus_core::watcher::disable_watcher() {
        Ok(()) => ok("watcher 已禁用。", watcher_payload()),
        Err(error) => failed(&format!("禁用 watcher 失败：{error}"), watcher_payload()),
    }
}

#[tauri::command]
pub fn read_latest_logs(request: LogRequest) -> CommandResult<LogsPayload> {
    let path = codex_plus_core::paths::default_diagnostic_log_path();
    match read_tail(&path, request.lines) {
        Ok(text) => ok(
            "日志已读取。",
            LogsPayload {
                path: path.to_string_lossy().to_string(),
                text,
                lines: request.lines,
            },
        ),
        Err(error) => failed(
            &format!("读取日志失败：{error}"),
            LogsPayload {
                path: path.to_string_lossy().to_string(),
                text: String::new(),
                lines: request.lines,
            },
        ),
    }
}

#[tauri::command]
pub fn copy_diagnostics() -> CommandResult<DiagnosticsPayload> {
    ok(
        "诊断报告已生成。",
        DiagnosticsPayload {
            report: diagnostics_report(),
        },
    )
}

#[tauri::command]
pub fn reset_settings() -> CommandResult<SettingsPayload> {
    let settings = BackendSettings::default();
    match SettingsStore::default().save(&settings) {
        Ok(()) => settings_payload("设置已重置为默认值。", "设置重置后重新读取失败"),
        Err(error) => failed(
            &format!("重置设置失败：{error}"),
            SettingsPayload {
                settings,
                settings_path: codex_plus_core::paths::default_settings_path()
                    .to_string_lossy()
                    .to_string(),
                user_scripts: user_script_inventory(),
            },
        ),
    }
}

#[tauri::command]
pub fn reset_image_overlay_settings() -> CommandResult<SettingsPayload> {
    let store = SettingsStore::default();
    let mut settings = match store.load() {
        Ok(settings) => settings,
        Err(error) => {
            return failed(
                &format!(
                    "重置图片覆盖层已停止：无法读取 Manager 设置：{error:#}。原 settings.json 保持不变。"
                ),
                fallback_settings_payload(),
            );
        }
    };
    let defaults = BackendSettings::default();
    settings.codex_app_image_overlay_enabled = defaults.codex_app_image_overlay_enabled;
    settings.codex_app_image_overlay_path = defaults.codex_app_image_overlay_path;
    settings.codex_app_image_overlay_opacity = defaults.codex_app_image_overlay_opacity;
    settings.codex_app_image_overlay_fit_mode = defaults.codex_app_image_overlay_fit_mode;
    let settings = normalize_settings_before_save(settings);
    match store.save(&settings) {
        Ok(()) => settings_payload("图片覆盖层设置已重置。", "图片覆盖层重置后重新读取失败"),
        Err(error) => failed(
            &format!("重置图片覆盖层失败：{error}"),
            SettingsPayload {
                settings,
                settings_path: codex_plus_core::paths::default_settings_path()
                    .to_string_lossy()
                    .to_string(),
                user_scripts: user_script_inventory(),
            },
        ),
    }
}

#[tauri::command]
pub fn relay_status() -> CommandResult<RelayPayload> {
    let status = codex_plus_core::relay_config::default_relay_status();
    if status.state_unreadable {
        let message = status
            .state_error
            .clone()
            .unwrap_or_else(|| "Codex 配置状态不可读，已停止将其显示为“未配置”。".to_string());
        return failed(&message, relay_payload(status, None));
    }
    let message = if status.authenticated {
        "已检测到 ChatGPT 登录状态。"
    } else {
        "未检测到 ChatGPT 登录状态，请先在 Codex/ChatGPT 中正常登录。"
    };
    ok(message, relay_payload(status, None))
}

#[tauri::command]
pub fn read_relay_files() -> CommandResult<RelayFilesPayload> {
    let home = codex_plus_core::relay_config::default_codex_home_dir();
    match relay_files_payload_from_home(&home) {
        Ok(payload) => ok("配置文件内容已读取。", payload),
        Err(error) => failed(
            &format!("读取配置文件失败：{error}"),
            RelayFilesPayload {
                config_path: home.join("config.toml").to_string_lossy().to_string(),
                auth_path: home.join("auth.json").to_string_lossy().to_string(),
                config_contents: String::new(),
                auth_contents: String::new(),
            },
        ),
    }
}

#[tauri::command]
pub fn check_env_conflicts() -> CommandResult<EnvConflictsPayload> {
    let conflicts = codex_plus_core::env_conflicts::detect_env_conflicts();
    let message = if conflicts.is_empty() {
        "未检测到会覆盖 Codex 供应商配置的 OPENAI 环境变量。"
    } else {
        "检测到可能覆盖 Codex 供应商配置的 OPENAI 环境变量。"
    };
    ok(message, EnvConflictsPayload { conflicts })
}

#[tauri::command]
pub fn remove_env_conflicts(
    request: RemoveEnvConflictsRequest,
) -> CommandResult<RemoveEnvConflictsPayload> {
    let backup_dir = codex_plus_core::paths::default_app_state_dir().join("backups");
    match codex_plus_core::env_conflicts::remove_env_conflicts(&request.names, backup_dir) {
        Ok(result) => {
            let remaining = codex_plus_core::env_conflicts::detect_env_conflicts();
            ok(
                "环境变量已按确认项删除；重新启动 Codex 后生效。",
                RemoveEnvConflictsPayload {
                    removed: result.removed,
                    backup_path: result.backup_path,
                    remaining,
                },
            )
        }
        Err(error) => failed(
            &format!("删除环境变量失败：{error}"),
            RemoveEnvConflictsPayload {
                removed: Vec::new(),
                backup_path: None,
                remaining: codex_plus_core::env_conflicts::detect_env_conflicts(),
            },
        ),
    }
}

#[tauri::command]
pub async fn save_relay_file(request: SaveRelayFileRequest) -> CommandResult<RelayFilesPayload> {
    let _guard = relay_switch_mutex().lock().await;
    let home = codex_plus_core::relay_config::default_codex_home_dir();
    match save_relay_file_in_home(&home, &request.kind, &request.contents)
        .and_then(|_| relay_files_payload_from_home(&home))
    {
        Ok(payload) => ok("配置文件已保存。", payload),
        Err(error) => failed(
            &format!("保存配置文件失败：{error}"),
            relay_files_payload_from_home(&home).unwrap_or_else(|_| RelayFilesPayload {
                config_path: home.join("config.toml").to_string_lossy().to_string(),
                auth_path: home.join("auth.json").to_string_lossy().to_string(),
                config_contents: String::new(),
                auth_contents: String::new(),
            }),
        ),
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayProfileSwitchRequest {
    pub settings: BackendSettings,
    #[serde(default)]
    pub previous_active_relay_id: String,
}

#[tauri::command]
pub async fn switch_relay_profile(
    request: RelayProfileSwitchRequest,
) -> CommandResult<RelaySwitchPayload> {
    let _guard = relay_switch_mutex().lock().await;
    let home = codex_plus_core::relay_config::default_codex_home_dir();
    let store = SettingsStore::default();
    let previous_active_relay_id = request.previous_active_relay_id;
    let settings = normalize_settings_before_save(request.settings);
    if let Some((_process_count, message)) = codex_running_mutation_message("切换供应商") {
        let status = codex_plus_core::relay_config::relay_status_from_home(&home);
        let current_settings = store.load().unwrap_or_else(|_| settings.clone());
        return failed(
            &message,
            relay_switch_payload(current_settings, status, None),
        );
    }
    let requested_settings = settings.clone();
    log_manager_event(
        "manager.switch_relay_profile.start",
        json!({
            "previousActiveRelayId": previous_active_relay_id,
            "targetRelayId": settings.active_relay_id
        }),
    );
    match codex_plus_core::relay_switch::switch_relay_profile_in_home_verified(
        &store,
        &home,
        settings,
        &previous_active_relay_id,
    )
    .await
    {
        Ok(result) => {
            let status = codex_plus_core::relay_config::relay_status_from_home(&home);
            log_manager_event(
                "manager.switch_relay_profile.ok",
                json!({
                    "targetRelayId": result.settings.active_relay_id,
                    "configured": status.configured,
                    "backupPath": result.backup_path.as_ref()
                }),
            );
            ok(
                "供应商已切换。",
                relay_switch_payload(result.settings, status, result.backup_path),
            )
        }
        Err(error) => {
            let status = codex_plus_core::relay_config::relay_status_from_home(&home);
            let settings = store.load().unwrap_or(requested_settings);
            log_manager_event(
                "manager.switch_relay_profile.failed",
                json!({
                    "previousActiveRelayId": previous_active_relay_id,
                    "activeRelayId": settings.active_relay_id,
                    "error": error.to_string()
                }),
            );
            failed(
                &format!("供应商切换失败：{error}"),
                relay_switch_payload(settings, status, None),
            )
        }
    }
}

#[tauri::command]
pub fn write_diagnostic_event(event: String, detail: Value) -> CommandResult<Value> {
    let event = sanitize_manager_event(&event);
    match codex_plus_core::diagnostic_log::append_diagnostic_log(&event, detail) {
        Ok(()) => ok("诊断日志已写入。", json!({})),
        Err(error) => failed(&format!("写入诊断日志失败：{error}"), json!({})),
    }
}

#[tauri::command]
pub fn backfill_relay_profile_from_live(
    request: BackfillRelayProfileRequest,
) -> CommandResult<SettingsBackfillPayload> {
    let home = codex_plus_core::relay_config::default_codex_home_dir();
    let mut settings = request.settings;
    let requested_profile_id = request.profile_id.clone();
    log_manager_event(
        "manager.backfill_relay_profile_from_live.start",
        json!({
            "profileId": requested_profile_id,
            "activeRelayId": settings.active_relay_id
        }),
    );
    let Some(profile) = settings
        .relay_profiles
        .iter_mut()
        .find(|profile| profile.id == request.profile_id)
    else {
        log_manager_event(
            "manager.backfill_relay_profile_from_live.missing_profile",
            json!({
                "profileId": requested_profile_id
            }),
        );
        return failed(
            "当前供应商已不在配置列表中，已停止切换以避免覆盖用户改动。",
            SettingsBackfillPayload { settings },
        );
    };

    match codex_plus_core::relay_config::backfill_relay_profile_from_home_with_common(
        &home,
        profile,
        &mut settings.relay_context_config_contents,
    ) {
        Ok(()) => {
            log_manager_event(
                "manager.backfill_relay_profile_from_live.ok",
                json!({
                    "profileId": requested_profile_id
                }),
            );
            ok(
                "当前供应商配置已从 live 文件回填。",
                SettingsBackfillPayload { settings },
            )
        }
        Err(error) => {
            log_manager_event(
                "manager.backfill_relay_profile_from_live.failed",
                json!({
                    "profileId": requested_profile_id,
                    "error": error.to_string()
                }),
            );
            failed(
                &format!("回填当前供应商配置失败：{error}"),
                SettingsBackfillPayload { settings },
            )
        }
    }
}

#[tauri::command]
pub fn list_context_entries(
    request: ContextSettingsRequest,
) -> CommandResult<ContextEntriesPayload> {
    match codex_plus_core::relay_config::list_context_entries_from_common_config(
        &request.settings.relay_context_config_contents,
    ) {
        Ok(entries) => ok(
            "工具与插件列表已读取。",
            ContextEntriesPayload {
                settings: request.settings,
                entries,
            },
        ),
        Err(error) => failed(
            &format!("读取工具与插件列表失败：{error}"),
            ContextEntriesPayload {
                settings: request.settings,
                entries: empty_context_entries(),
            },
        ),
    }
}

#[tauri::command]
pub fn read_live_context_entries() -> CommandResult<LiveContextEntriesPayload> {
    let home = codex_plus_core::relay_config::default_codex_home_dir();
    let config_path = home.join("config.toml");
    let config = read_optional_text_file(&config_path).unwrap_or_default();
    match codex_plus_core::relay_config::list_context_entries_from_common_config(&config) {
        Ok(entries) => ok(
            "live 工具与插件已读取。",
            LiveContextEntriesPayload { entries },
        ),
        Err(error) => failed(
            &format!("读取 live 工具与插件失败：{error}"),
            LiveContextEntriesPayload {
                entries: empty_context_entries(),
            },
        ),
    }
}

#[tauri::command]
pub fn upsert_context_entry(request: ContextEntryRequest) -> CommandResult<ContextEntriesPayload> {
    let mut settings = request.settings;
    match codex_plus_core::relay_config::upsert_context_entry_in_common_config(
        &settings.relay_context_config_contents,
        &request.kind,
        &request.id,
        &request.toml_body,
    ) {
        Ok(common) => {
            settings.relay_context_config_contents = common;
            list_context_entries(ContextSettingsRequest { settings })
        }
        Err(error) => failed(
            &format!("保存工具与插件失败：{error}"),
            ContextEntriesPayload {
                settings,
                entries: empty_context_entries(),
            },
        ),
    }
}

#[tauri::command]
pub async fn sync_live_context_entries(
    request: ContextSettingsRequest,
) -> CommandResult<LiveContextEntriesPayload> {
    let _guard = relay_switch_mutex().lock().await;
    let home = codex_plus_core::relay_config::default_codex_home_dir();
    let config_path = home.join("config.toml");
    let current_config = match read_optional_text_file(&config_path) {
        Ok(config) => config,
        Err(error) => {
            return failed(
                &format!("读取 live config.toml 失败：{error}"),
                LiveContextEntriesPayload {
                    entries: empty_context_entries(),
                },
            );
        }
    };
    let updated_config = match codex_plus_core::relay_config::sync_live_config_context_entries(
        &current_config,
        &request.settings.relay_context_config_contents,
    ) {
        Ok(config) => config,
        Err(error) => {
            return failed(
                &format!("同步 live 工具与插件失败：{error}"),
                LiveContextEntriesPayload {
                    entries: empty_context_entries(),
                },
            );
        }
    };
    if let Err(error) =
        codex_plus_core::relay_config::apply_relay_config_file_to_home(&home, &updated_config)
    {
        return failed(
            &format!("写入 live config.toml 失败：{error}"),
            LiveContextEntriesPayload {
                entries: empty_context_entries(),
            },
        );
    }
    match codex_plus_core::relay_config::list_context_entries_from_common_config(&updated_config) {
        Ok(entries) => ok(
            "live 工具与插件已同步。",
            LiveContextEntriesPayload { entries },
        ),
        Err(error) => failed(
            &format!("读取同步后的 live 工具与插件失败：{error}"),
            LiveContextEntriesPayload {
                entries: empty_context_entries(),
            },
        ),
    }
}

#[tauri::command]
pub fn delete_context_entry(request: ContextDeleteRequest) -> CommandResult<ContextEntriesPayload> {
    let mut settings = request.settings;
    match codex_plus_core::relay_config::delete_context_entry_from_common_config(
        &settings.relay_context_config_contents,
        &request.kind,
        &request.id,
    ) {
        Ok(common) => {
            settings.relay_context_config_contents = common;
            list_context_entries(ContextSettingsRequest { settings })
        }
        Err(error) => failed(
            &format!("删除工具与插件失败：{error}"),
            ContextEntriesPayload {
                settings,
                entries: empty_context_entries(),
            },
        ),
    }
}

#[tauri::command]
pub fn extract_relay_common_config(
    request: ExtractRelayCommonConfigRequest,
) -> CommandResult<ExtractRelayCommonConfigPayload> {
    match codex_plus_core::relay_config::extract_common_config_from_config(&request.config_contents)
        .and_then(|common_config_contents| {
            let profile_config_contents =
                codex_plus_core::relay_config::strip_common_config_from_config(
                    &request.config_contents,
                    &common_config_contents,
                )?;
            Ok(ExtractRelayCommonConfigPayload {
                common_config_contents,
                profile_config_contents,
            })
        }) {
        Ok(payload) => ok("通用配置已按兼容切换规则提取。", payload),
        Err(error) => failed(
            &format!("提取通用配置失败：{error}"),
            ExtractRelayCommonConfigPayload {
                common_config_contents: String::new(),
                profile_config_contents: request.config_contents,
            },
        ),
    }
}

#[tauri::command]
pub async fn test_relay_profile(profile: RelayProfile) -> CommandResult<RelayProfileTestPayload> {
    let profile_name = if profile.name.trim().is_empty() {
        "未命名供应商"
    } else {
        profile.name.trim()
    };
    let settings = match SettingsStore::default().load() {
        Ok(settings) => settings,
        Err(error) => {
            return failed(
                &format!(
                    "测试供应商「{profile_name}」已停止：无法读取 Manager 设置：{error:#}。未发送上游请求。"
                ),
                RelayProfileTestPayload {
                    http_status: 0,
                    endpoint: String::new(),
                    response_preview: error.to_string(),
                },
            );
        }
    };
    let test_model: String = if !profile.test_model.trim().is_empty() {
        // 1. 使用者在該供應商明確填的測試模型
        profile.test_model.trim().to_string()
    } else {
        // 2. 該供應商自己 config.toml 裡的 model（避免串味）
        let from_profile = codex_plus_core::relay_config::relay_profile_model(&profile);
        if from_profile.trim().is_empty() {
            // 3. 最後才用全域預設
            settings.relay_test_model.trim().to_string()
        } else {
            from_profile
        }
    };
    match codex_plus_core::relay_config::test_relay_profile(&profile, &test_model).await {
        Ok(result) => {
            let status = if result.http_status < 400 {
                "ok"
            } else {
                "failed"
            };
            let preview = result.response_preview.trim();
            let detail = if preview.is_empty() {
                "响应内容为空".to_string()
            } else {
                format!("响应：{preview}")
            };
            CommandResult {
                status: status.to_string(),
                message: format!(
                    "已向「{profile_name}」用模型「{test_model}」发送 hi，HTTP {}。{detail}",
                    result.http_status
                ),
                payload: RelayProfileTestPayload {
                    http_status: result.http_status,
                    endpoint: result.endpoint,
                    response_preview: result.response_preview,
                },
            }
        }
        Err(error) => failed(
            &format!("测试「{profile_name}」失败：{error}"),
            RelayProfileTestPayload {
                http_status: 0,
                endpoint: String::new(),
                response_preview: String::new(),
            },
        ),
    }
}

#[tauri::command]
pub async fn test_stepwise_settings(
    settings: BackendSettings,
) -> CommandResult<StepwiseTestPayload> {
    let configured_protocol = codex_plus_core::settings::normalize_stepwise_protocol(
        &settings.codex_app_stepwise_protocol,
    );
    match codex_plus_core::stepwise::test_connection(&settings).await {
        Ok(result) => {
            let error = result
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let item_count = result
                .get("items")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or_default();
            let protocol = result
                .get("protocol")
                .and_then(Value::as_str)
                .unwrap_or(&configured_protocol)
                .to_string();
            if error.is_empty() {
                ok(
                    &format!(
                        "Stepwise 连接正常（{}），测试返回 {item_count} 条建议。",
                        stepwise_protocol_label(&protocol)
                    ),
                    StepwiseTestPayload { item_count, error },
                )
            } else {
                failed(
                    &format!("Stepwise 测试失败：{error}"),
                    StepwiseTestPayload { item_count, error },
                )
            }
        }
        Err(error) => failed(
            &format!("Stepwise 测试失败：{error}"),
            StepwiseTestPayload {
                item_count: 0,
                error: error.to_string(),
            },
        ),
    }
}

fn stepwise_protocol_label(protocol: &str) -> &str {
    match protocol {
        "chat_completions" => "Chat Completions",
        "responses" => "Responses",
        "anthropic_messages" => "Anthropic Messages",
        "auto" => "自动兼容",
        _ => protocol,
    }
}

#[tauri::command]
pub async fn fetch_relay_profile_models(
    profile: RelayProfile,
) -> CommandResult<RelayProfileModelsPayload> {
    let profile_name = if profile.name.trim().is_empty() {
        "未命名供应商"
    } else {
        profile.name.trim()
    };
    match codex_plus_core::model_catalog::fetch_relay_profile_model_ids(&profile).await {
        Ok((models, endpoint)) => ok(
            &format!("已从「{profile_name}」获取 {} 个模型。", models.len()),
            RelayProfileModelsPayload { models, endpoint },
        ),
        Err(error) => failed(
            &format!("从「{profile_name}」获取模型失败：{error}"),
            RelayProfileModelsPayload {
                models: Vec::new(),
                endpoint: String::new(),
            },
        ),
    }
}

#[tauri::command]
pub async fn diagnose_relay_profile(profile: RelayProfile) -> CommandResult<ProviderDoctorPayload> {
    let profile_name = if profile.name.trim().is_empty() {
        "未命名供应商".to_string()
    } else {
        profile.name.trim().to_string()
    };
    let settings = match SettingsStore::default().load() {
        Ok(settings) => settings,
        Err(error) => {
            let detail = format!("无法读取 Manager 设置：{error:#}");
            return failed(
                &format!("供应商诊断已停止：{detail}。未发送上游请求。"),
                ProviderDoctorPayload {
                    profile_name,
                    model: String::new(),
                    summary: "Manager 设置不可读取".to_string(),
                    recommendation: "请先从备份恢复 settings.json，再重新诊断供应商。".to_string(),
                    checks: vec![ProviderDoctorCheck {
                        id: "settings".to_string(),
                        title: "Manager 设置".to_string(),
                        status: "failed".to_string(),
                        detail,
                    }],
                },
            );
        }
    };
    let test_model = if !profile.test_model.trim().is_empty() {
        profile.test_model.trim().to_string()
    } else {
        let from_profile = codex_plus_core::relay_config::relay_profile_model(&profile);
        if from_profile.trim().is_empty() {
            settings.relay_test_model.trim().to_string()
        } else {
            from_profile
        }
    };
    let mut checks = Vec::new();

    if profile.relay_mode == codex_plus_core::settings::RelayMode::Official
        && !profile.official_mix_api_key
    {
        checks.push(ProviderDoctorCheck {
            id: "config".to_string(),
            title: "配置完整性".to_string(),
            status: "ok".to_string(),
            detail: "官方登录供应商不需要 Base URL / API Key。".to_string(),
        });
        let payload = ProviderDoctorPayload {
            profile_name,
            model: test_model,
            summary: "官方登录供应商无需 API 诊断。".to_string(),
            recommendation: "如果 Codex 官方账号可用，直接使用官方登录模式即可。".to_string(),
            checks,
        };
        return ok("Provider Doctor：官方登录供应商无需 API 诊断。", payload);
    }

    if codex_plus_core::relay_config::relay_profile_base_url(&profile)
        .trim()
        .is_empty()
        || codex_plus_core::relay_config::relay_profile_api_key(&profile)
            .trim()
            .is_empty()
    {
        checks.push(ProviderDoctorCheck {
            id: "config".to_string(),
            title: "配置完整性".to_string(),
            status: "failed".to_string(),
            detail: "Base URL 或 API Key 为空。".to_string(),
        });
        let payload = ProviderDoctorPayload {
            profile_name,
            model: test_model,
            summary: "配置不完整，无法发起上游诊断。".to_string(),
            recommendation: "先填写 Base URL 和 API Key；如果是官方账号，请切换到官方登录模式。"
                .to_string(),
            checks,
        };
        return failed("Provider Doctor：配置不完整。", payload);
    }

    checks.push(ProviderDoctorCheck {
        id: "config".to_string(),
        title: "配置完整性".to_string(),
        status: "ok".to_string(),
        detail: format!(
            "{} / {}",
            codex_plus_core::relay_config::relay_profile_base_url(&profile),
            match profile.protocol {
                codex_plus_core::settings::RelayProtocol::Responses => "Responses API",
                codex_plus_core::settings::RelayProtocol::ChatCompletions => "Chat Completions",
            }
        ),
    });

    match codex_plus_core::model_catalog::fetch_relay_profile_model_ids(&profile).await {
        Ok((models, endpoint)) => {
            let contains_model = !test_model.trim().is_empty()
                && models.iter().any(|model| model == test_model.trim());
            let status = if models.is_empty() {
                "failed"
            } else if contains_model || test_model.trim().is_empty() {
                "ok"
            } else {
                "warning"
            };
            let detail = if models.is_empty() {
                format!("{endpoint} 返回 0 个模型。")
            } else if contains_model || test_model.trim().is_empty() {
                format!("{endpoint} 返回 {} 个模型。", models.len())
            } else {
                format!(
                    "{endpoint} 返回 {} 个模型，但未看到测试模型「{}」。",
                    models.len(),
                    test_model
                )
            };
            checks.push(ProviderDoctorCheck {
                id: "models".to_string(),
                title: "模型列表".to_string(),
                status: status.to_string(),
                detail,
            });
        }
        Err(error) => checks.push(ProviderDoctorCheck {
            id: "models".to_string(),
            title: "模型列表".to_string(),
            status: "failed".to_string(),
            detail: error.to_string(),
        }),
    }

    match codex_plus_core::relay_config::test_relay_profile(&profile, &test_model).await {
        Ok(result) => {
            let status = if result.http_status < 400 {
                "ok"
            } else {
                "failed"
            };
            let preview = result.response_preview.trim();
            checks.push(ProviderDoctorCheck {
                id: "request".to_string(),
                title: "真实请求".to_string(),
                status: status.to_string(),
                detail: if preview.is_empty() {
                    format!(
                        "{} 返回 HTTP {}，响应内容为空。",
                        result.endpoint, result.http_status
                    )
                } else {
                    format!(
                        "{} 返回 HTTP {}：{}",
                        result.endpoint, result.http_status, preview
                    )
                },
            });
        }
        Err(error) => checks.push(ProviderDoctorCheck {
            id: "request".to_string(),
            title: "真实请求".to_string(),
            status: "failed".to_string(),
            detail: error.to_string(),
        }),
    }

    let failed_count = checks
        .iter()
        .filter(|check| check.status == "failed")
        .count();
    let warning_count = checks
        .iter()
        .filter(|check| check.status == "warning")
        .count();
    let status = if failed_count > 0 {
        "failed"
    } else if warning_count > 0 {
        "ok"
    } else {
        "ok"
    };
    let summary = if failed_count > 0 {
        format!("发现 {failed_count} 项失败，Codex 可能无法使用该供应商。")
    } else if warning_count > 0 {
        format!("基础连接可用，但有 {warning_count} 项需要确认。")
    } else {
        "供应商基础诊断通过。".to_string()
    };
    let recommendation = provider_doctor_recommendation(&checks);
    let message = format!("Provider Doctor：{summary}");
    CommandResult {
        status: status.to_string(),
        message,
        payload: ProviderDoctorPayload {
            profile_name,
            model: test_model,
            summary,
            recommendation,
            checks,
        },
    }
}

fn provider_doctor_recommendation(checks: &[ProviderDoctorCheck]) -> String {
    if checks
        .iter()
        .any(|check| check.id == "config" && check.status == "failed")
    {
        return "先补齐 Base URL 和 API Key；如果使用官方账号，请切换到官方登录模式。".to_string();
    }
    if checks
        .iter()
        .any(|check| check.id == "models" && check.status == "failed")
    {
        return "优先检查 Base URL 是否包含正确的 /v1 前缀，以及供应商是否支持 /v1/models。"
            .to_string();
    }
    if checks
        .iter()
        .any(|check| check.id == "request" && check.status == "failed")
    {
        return "优先检查测试模型名称、上游协议选择和 Key 权限；如果 Chat Completions 可用，请切到对应协议。".to_string();
    }
    if checks.iter().any(|check| check.status == "warning") {
        return "连接可用，但测试模型没有出现在模型列表里；建议改用上游返回的模型名。".to_string();
    }
    "可以作为 Codex 供应商使用；如果真实对话仍失败，请查看协议代理日志里的上游响应。".to_string()
}

async fn execute_verified_relay_apply<F>(
    home: &Path,
    settings: &BackendSettings,
    operation: F,
) -> anyhow::Result<codex_plus_core::relay_config::RelayApplyResult>
where
    F: FnOnce() -> anyhow::Result<codex_plus_core::relay_config::RelayApplyResult>,
{
    codex_plus_core::relay_switch::verify_backend_settings_live(settings)
        .await
        .map_err(|error| {
            anyhow::anyhow!("写入前真实请求验证失败：{error:#}；未改写 config.toml 或 auth.json")
        })?;
    let process_count = codex_plus_core::watcher::find_codex_processes().len();
    if process_count > 0 {
        anyhow::bail!(
            "检测到 Codex 在验证期间启动（{process_count} 个进程）。请完全退出 Codex 后重试；未改写 config.toml、auth.json 或会话历史"
        );
    }
    codex_plus_core::codex_app_state::capture_app_state_snapshot(home)
        .context("创建 Codex 界面状态恢复快照失败；未改写供应商配置")?;
    let snapshot =
        codex_plus_core::relay_config::capture_relay_live_snapshot(home, &settings.relay_profiles)?;
    let result = match operation() {
        Ok(result) => result,
        Err(error) => {
            return match codex_plus_core::relay_config::restore_relay_live_snapshot(&snapshot) {
                Ok(()) => Err(anyhow::anyhow!(
                    "{error:#}；已恢复写入前的 config.toml、auth.json 和模型目录"
                )),
                Err(restore_error) => Err(anyhow::anyhow!(
                    "{error:#}；自动恢复未完整成功：{restore_error:#}"
                )),
            };
        }
    };
    let active = settings.active_relay_profile();
    let expects_custom_provider = settings.active_aggregate_relay_profile().is_some()
        || active.relay_mode != codex_plus_core::settings::RelayMode::Official
        || active.official_mix_api_key;
    if expects_custom_provider && !result.configured {
        return match codex_plus_core::relay_config::restore_relay_live_snapshot(&snapshot) {
            Ok(()) => Err(anyhow::anyhow!(
                "写入后的 Codex 配置未形成可用 custom provider；已恢复写入前状态"
            )),
            Err(restore_error) => Err(anyhow::anyhow!(
                "写入后的 Codex 配置未形成可用 custom provider，且自动恢复未完整成功：{restore_error:#}"
            )),
        };
    }
    if let Err(error) =
        codex_plus_core::relay_switch::verify_backend_settings_from_home(home, settings).await
    {
        return match codex_plus_core::relay_config::restore_relay_live_snapshot(&snapshot) {
            Ok(()) => Err(anyhow::anyhow!(
                "写入后真实请求复核失败：{error:#}；已恢复写入前状态"
            )),
            Err(restore_error) => Err(anyhow::anyhow!(
                "写入后真实请求复核失败：{error:#}；自动恢复未完整成功：{restore_error:#}"
            )),
        };
    }
    codex_plus_core::codex_app_state::sync_app_state_after_provider_switch_nonfatal(
        home,
        "manager.execute_verified_relay_apply.after",
    );
    Ok(result)
}

#[tauri::command]
pub async fn apply_relay_injection() -> CommandResult<RelayPayload> {
    let _guard = relay_switch_mutex().lock().await;
    let home = codex_plus_core::relay_config::default_codex_home_dir();
    if let Some((_process_count, message)) = codex_running_mutation_message("切换供应商") {
        let status = codex_plus_core::relay_config::relay_status_from_home(&home);
        return failed(&message, relay_payload(status, None));
    }
    let settings = match load_settings_for_relay_mutation(&home, "切换供应商") {
        Ok(settings) => settings,
        Err(result) => return result,
    };
    if !settings.relay_profiles_enabled {
        let status = codex_plus_core::relay_config::relay_status_from_home(&home);
        return failed(
            "供应商配置总开关已关闭，未写入 config.toml / auth.json。",
            relay_payload(status, None),
        );
    }
    let relay = settings.active_relay_profile();
    log_relay_apply_request("manager.apply_relay_injection", &settings, &relay);
    if settings.active_aggregate_relay_profile().is_some() {
        return match execute_verified_relay_apply(&home, &settings, || {
            apply_aggregate_relay_injection_to_home(&home)
        })
        .await
        {
            Ok(result) => {
                let status = codex_plus_core::relay_config::relay_status_from_home(&home);
                ok(
                    "聚合供应商配置已写入，真实请求会由本地代理按策略轮转。",
                    relay_payload(status, result.backup_path),
                )
            }
            Err(error) => {
                let status = codex_plus_core::relay_config::relay_status_from_home(&home);
                failed(
                    &format!("写入聚合供应商配置失败：{error:#}"),
                    relay_payload(status, None),
                )
            }
        };
    }
    if relay_has_complete_files(&relay) {
        return match execute_verified_relay_apply(&home, &settings, || {
            codex_plus_core::relay_config::apply_relay_profile_to_home_with_switch_rules_and_computer_use_guard(
                &home,
                &relay,
                &relay_combined_common_config(&settings),
                settings.computer_use_guard_enabled,
            )
        })
        .await
        {
            Ok(result) => {
                let status = codex_plus_core::relay_config::relay_status_from_home(&home);
                log_relay_apply_result(
                    "manager.apply_relay_injection.ok",
                    &relay,
                    &status,
                    result.backup_path.as_ref(),
                    None,
                );
                ok(
                    "已按兼容切换规则切换供应商。",
                    relay_payload(status, result.backup_path),
                )
            }
            Err(error) => {
                let status = codex_plus_core::relay_config::relay_status_from_home(&home);
                log_relay_apply_result(
                    "manager.apply_relay_injection.failed",
                    &relay,
                    &status,
                    None,
                    Some(error.to_string()),
                );
                failed(
                    &format!("切换完整中转配置失败：{error}"),
                    relay_payload(status, None),
                )
            }
        };
    }

    let auth = codex_plus_core::relay_config::chatgpt_auth_status_from_home(&home);
    if !auth.authenticated {
        let status = codex_plus_core::relay_config::relay_status_from_home(&home);
        log_relay_apply_result(
            "manager.apply_relay_injection.failed",
            &relay,
            &status,
            None,
            Some("未检测到 ChatGPT 登录状态".to_string()),
        );
        return failed(
            "未检测到 ChatGPT 登录状态，已停止写入中转配置。",
            relay_payload(status, None),
        );
    }

    match execute_verified_relay_apply(&home, &settings, || {
        codex_plus_core::relay_config::apply_relay_config_to_home_with_protocol(
            &home,
            &relay.base_url,
            &relay.api_key,
            relay.protocol,
            codex_plus_core::protocol_proxy::DEFAULT_PROTOCOL_PROXY_PORT,
        )
    })
    .await
    {
        Ok(result) => {
            let status = codex_plus_core::relay_config::relay_status_from_home(&home);
            log_relay_apply_result(
                "manager.apply_relay_injection.ok",
                &relay,
                &status,
                result.backup_path.as_ref(),
                None,
            );
            ok(
                "中转配置已写入，密钥未在界面明文显示。",
                relay_payload(status, result.backup_path),
            )
        }
        Err(error) => {
            let status = codex_plus_core::relay_config::relay_status_from_home(&home);
            log_relay_apply_result(
                "manager.apply_relay_injection.failed",
                &relay,
                &status,
                None,
                Some(error.to_string()),
            );
            failed(
                &format!("写入中转配置失败：{error}"),
                relay_payload(status, None),
            )
        }
    }
}

fn apply_aggregate_relay_injection_to_home(
    home: &Path,
) -> anyhow::Result<codex_plus_core::relay_config::RelayApplyResult> {
    codex_plus_core::relay_config::apply_relay_config_to_home_with_protocol(
        home,
        &codex_plus_core::protocol_proxy::local_responses_proxy_base_url(
            codex_plus_core::protocol_proxy::DEFAULT_PROTOCOL_PROXY_PORT,
        ),
        "codex-plus-aggregate",
        codex_plus_core::settings::RelayProtocol::Responses,
        codex_plus_core::protocol_proxy::DEFAULT_PROTOCOL_PROXY_PORT,
    )
}

#[tauri::command]
pub async fn apply_pure_api_injection() -> CommandResult<RelayPayload> {
    let _guard = relay_switch_mutex().lock().await;
    let home = codex_plus_core::relay_config::default_codex_home_dir();
    if let Some((_process_count, message)) = codex_running_mutation_message("切换纯 API 模式")
    {
        let status = codex_plus_core::relay_config::relay_status_from_home(&home);
        return failed(&message, relay_payload(status, None));
    }
    let settings = match load_settings_for_relay_mutation(&home, "切换纯 API 模式") {
        Ok(settings) => settings,
        Err(result) => return result,
    };
    if !settings.relay_profiles_enabled {
        let status = codex_plus_core::relay_config::relay_status_from_home(&home);
        return failed(
            "供应商配置总开关已关闭，未写入 config.toml / auth.json。",
            relay_payload(status, None),
        );
    }
    let relay = settings.active_relay_profile();
    log_relay_apply_request("manager.apply_pure_api_injection", &settings, &relay);
    if relay_has_complete_files(&relay) {
        return match execute_verified_relay_apply(&home, &settings, || {
            codex_plus_core::relay_config::apply_relay_profile_to_home_with_switch_rules_and_computer_use_guard(
                &home,
                &relay,
                &relay_combined_common_config(&settings),
                settings.computer_use_guard_enabled,
            )
        })
        .await
        {
            Ok(result) => {
                let status = codex_plus_core::relay_config::relay_status_from_home(&home);
                log_relay_apply_result(
                    "manager.apply_pure_api_injection.ok",
                    &relay,
                    &status,
                    result.backup_path.as_ref(),
                    None,
                );
                if !status.configured {
                    return failed(
                        "纯 API 配置写入后未检测到完整 custom provider，请检查 config.toml 和供应商 API Key。",
                        relay_payload(status, result.backup_path),
                    );
                }
                ok(
                    "已按兼容切换规则切换供应商。",
                    relay_payload(status, result.backup_path),
                )
            }
            Err(error) => {
                let status = codex_plus_core::relay_config::relay_status_from_home(&home);
                log_relay_apply_result(
                    "manager.apply_pure_api_injection.failed",
                    &relay,
                    &status,
                    None,
                    Some(error.to_string()),
                );
                failed(
                    &format!("切换纯 API 配置失败：{error}"),
                    relay_payload(status, None),
                )
            }
        };
    }

    match execute_verified_relay_apply(&home, &settings, || {
        codex_plus_core::relay_config::apply_pure_api_config_to_home_with_protocol(
            &home,
            &relay.base_url,
            &relay.api_key,
            relay.protocol,
            codex_plus_core::protocol_proxy::DEFAULT_PROTOCOL_PROXY_PORT,
        )
    })
    .await
    {
        Ok(result) => {
            let status = codex_plus_core::relay_config::relay_status_from_home(&home);
            log_relay_apply_result(
                "manager.apply_pure_api_injection.ok",
                &relay,
                &status,
                result.backup_path.as_ref(),
                None,
            );
            if !status.configured {
                return failed(
                    "纯 API 配置写入后未检测到完整 custom provider，请检查 config.toml 和供应商 API Key。",
                    relay_payload(status, result.backup_path),
                );
            }
            ok(
                "纯 API 模式已写入：config.toml 已写入 custom provider，auth.json 已切换为当前供应商。",
                relay_payload(status, result.backup_path),
            )
        }
        Err(error) => {
            let status = codex_plus_core::relay_config::relay_status_from_home(&home);
            log_relay_apply_result(
                "manager.apply_pure_api_injection.failed",
                &relay,
                &status,
                None,
                Some(error.to_string()),
            );
            failed(
                &format!("写入纯 API 模式失败：{error}"),
                relay_payload(status, None),
            )
        }
    }
}

#[tauri::command]
pub async fn clear_relay_injection() -> CommandResult<RelayPayload> {
    let _guard = relay_switch_mutex().lock().await;
    let home = codex_plus_core::relay_config::default_codex_home_dir();
    if let Some((_process_count, message)) = codex_running_mutation_message("清除中转配置") {
        let status = codex_plus_core::relay_config::relay_status_from_home(&home);
        return failed(&message, relay_payload(status, None));
    }
    let settings = match load_settings_for_relay_mutation(&home, "清除中转配置") {
        Ok(settings) => settings,
        Err(result) => return result,
    };
    let relay = settings.active_relay_profile();
    log_manager_event("manager.clear_relay_injection.start", json!({}));
    let auth_contents = (relay.relay_mode == codex_plus_core::settings::RelayMode::Official
        && !relay.official_mix_api_key
        && !relay.auth_contents.trim().is_empty())
    .then_some(relay.auth_contents.as_str());
    if let Err(error) = codex_plus_core::codex_app_state::capture_app_state_snapshot(&home) {
        let status = codex_plus_core::relay_config::relay_status_from_home(&home);
        return failed(
            &format!(
                "清除中转配置已停止：无法创建 Codex 界面状态恢复快照：{error:#}。未改写 config.toml 或 auth.json。"
            ),
            relay_payload(status, None),
        );
    }
    match codex_plus_core::relay_config::clear_relay_config_to_home_with_auth(&home, auth_contents)
    {
        Ok(result) => {
            codex_plus_core::codex_app_state::sync_app_state_after_provider_switch_nonfatal(
                &home,
                "manager.clear_relay_injection.after",
            );
            let status = codex_plus_core::relay_config::relay_status_from_home(&home);
            log_manager_event(
                "manager.clear_relay_injection.ok",
                json!({
                    "configured": status.configured,
                    "backupPath": result.backup_path.as_ref()
                }),
            );
            ok(
                "已清除 custom 中转 API 模式，并切换到官方 ChatGPT 登录模式。",
                relay_payload(status, result.backup_path),
            )
        }
        Err(error) => {
            let status = codex_plus_core::relay_config::relay_status_from_home(&home);
            log_manager_event(
                "manager.clear_relay_injection.failed",
                json!({
                    "configured": status.configured,
                    "error": error.to_string()
                }),
            );
            failed(
                &format!("清除中转配置失败：{error}"),
                relay_payload(status, None),
            )
        }
    }
}

fn relay_has_complete_files(relay: &codex_plus_core::settings::RelayProfile) -> bool {
    if relay.relay_mode == codex_plus_core::settings::RelayMode::Official
        && relay.official_mix_api_key
    {
        return !relay.config_contents.trim().is_empty();
    }
    !relay.config_contents.trim().is_empty() && !relay.auth_contents.trim().is_empty()
}

fn load_settings_for_relay_mutation(
    home: &Path,
    action: &str,
) -> Result<BackendSettings, CommandResult<RelayPayload>> {
    SettingsStore::default().load().map_err(|error| {
        let status = codex_plus_core::relay_config::relay_status_from_home(home);
        failed(
            &format!(
                "{action}已停止：无法读取 Manager 设置：{error:#}。原 settings.json 已保留，未改写 config.toml 或 auth.json。"
            ),
            relay_payload(status, None),
        )
    })
}

fn log_relay_apply_request(
    event: &str,
    settings: &BackendSettings,
    relay: &codex_plus_core::settings::RelayProfile,
) {
    let _ = codex_plus_core::diagnostic_log::append_diagnostic_log(
        event,
        json!({
            "activeRelayId": settings.active_relay_id,
            "relayId": relay.id,
            "relayName": relay.name,
            "relayMode": relay.relay_mode,
            "protocol": relay.protocol,
            "baseUrl": relay.base_url,
            "hasConfigContents": !relay.config_contents.trim().is_empty(),
            "hasAuthContents": !relay.auth_contents.trim().is_empty(),
            "configContainsProxy": relay.config_contents.contains("127.0.0.1:57321")
        }),
    );
}

fn log_relay_apply_result(
    event: &str,
    relay: &codex_plus_core::settings::RelayProfile,
    status: &codex_plus_core::relay_config::RelayStatus,
    backup_path: Option<&String>,
    error: Option<String>,
) {
    log_manager_event(
        event,
        json!({
            "relayId": relay.id,
            "relayName": relay.name,
            "relayMode": relay.relay_mode,
            "protocol": relay.protocol,
            "configured": status.configured,
            "requiresOpenaiAuth": status.requires_openai_auth,
            "hasBearerToken": status.has_bearer_token,
            "backupPath": backup_path,
            "error": error
        }),
    );
}

fn log_manager_event(event: &str, detail: Value) {
    let _ = codex_plus_core::diagnostic_log::append_diagnostic_log(event, detail);
}

fn sanitize_manager_event(event: &str) -> String {
    let suffix = event
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    let suffix = suffix.trim_matches(['.', '_', '-']).trim();
    if suffix.is_empty() {
        "manager.ui.event".to_string()
    } else if suffix.starts_with("manager.") {
        suffix.to_string()
    } else {
        format!("manager.ui.{suffix}")
    }
}

fn relay_payload(
    status: codex_plus_core::relay_config::RelayStatus,
    backup_path: Option<String>,
) -> RelayPayload {
    RelayPayload {
        authenticated: status.authenticated,
        auth_source: status.auth_source,
        account_label: status.account_label,
        config_path: status.config_path,
        configured: status.configured,
        requires_openai_auth: status.requires_openai_auth,
        has_bearer_token: status.has_bearer_token,
        state_unreadable: status.state_unreadable,
        state_error: status.state_error,
        backup_path,
    }
}

fn relay_switch_payload(
    settings: BackendSettings,
    status: codex_plus_core::relay_config::RelayStatus,
    backup_path: Option<String>,
) -> RelaySwitchPayload {
    RelaySwitchPayload {
        settings,
        relay: relay_payload(status, backup_path),
        settings_path: codex_plus_core::paths::default_settings_path()
            .to_string_lossy()
            .to_string(),
        user_scripts: user_script_inventory(),
    }
}

fn relay_switch_mutex() -> &'static AsyncMutex<()> {
    static RELAY_SWITCH_LOCK: OnceLock<AsyncMutex<()>> = OnceLock::new();
    RELAY_SWITCH_LOCK.get_or_init(|| AsyncMutex::new(()))
}

fn empty_context_entries() -> codex_plus_core::relay_config::CodexContextEntries {
    codex_plus_core::relay_config::CodexContextEntries {
        mcp_servers: Vec::new(),
        skills: Vec::new(),
        plugins: Vec::new(),
    }
}

fn relay_files_payload_from_home(home: &std::path::Path) -> anyhow::Result<RelayFilesPayload> {
    let config_path = home.join("config.toml");
    let auth_path = home.join("auth.json");
    Ok(RelayFilesPayload {
        config_path: config_path.to_string_lossy().to_string(),
        auth_path: auth_path.to_string_lossy().to_string(),
        config_contents: read_optional_text_file(&config_path)?,
        auth_contents: read_optional_text_file(&auth_path)?,
    })
}

fn save_relay_file_in_home(
    home: &std::path::Path,
    kind: &str,
    contents: &str,
) -> anyhow::Result<()> {
    match kind {
        "config" => {
            codex_plus_core::relay_config::apply_relay_config_file_to_home(home, contents)?;
        }
        "auth" => {
            codex_plus_core::relay_config::apply_relay_auth_file_to_home(home, contents)?;
        }
        other => anyhow::bail!("未知配置文件类型：{other}"),
    }
    Ok(())
}

fn read_optional_text_file(path: &std::path::Path) -> anyhow::Result<String> {
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error.into()),
    }
}

fn ads_payload(payload: Value) -> AdsPayload {
    AdsPayload {
        version: payload.get("version").and_then(Value::as_u64).unwrap_or(1),
        ads: payload
            .get("ads")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    }
}

fn open_url(url: &str) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        codex_plus_core::windows_open_url(url)
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .map(|_| ())
            .map_err(|error| anyhow::anyhow!("启动系统浏览器失败：{error}"))
    }
}

fn settings_payload(message: &str, failure_context: &str) -> CommandResult<SettingsPayload> {
    match settings_payload_value() {
        Ok(payload) => ok(message, payload),
        Err((error, payload)) => failed(&format!("{failure_context}：{error}"), payload),
    }
}

fn settings_payload_value() -> Result<SettingsPayload, (anyhow::Error, SettingsPayload)> {
    let store = SettingsStore::default();
    let settings_path = codex_plus_core::paths::default_settings_path()
        .to_string_lossy()
        .to_string();
    match store.load() {
        Ok(settings) => Ok(SettingsPayload {
            settings,
            settings_path,
            user_scripts: user_script_inventory(),
        }),
        Err(error) => Err((
            error,
            SettingsPayload {
                settings: BackendSettings::default(),
                settings_path,
                user_scripts: user_script_inventory(),
            },
        )),
    }
}

fn fallback_settings_payload() -> SettingsPayload {
    settings_payload_value().unwrap_or_else(|(_, payload)| payload)
}

fn user_script_inventory() -> Value {
    default_user_script_manager()
        .inventory()
        .unwrap_or_else(|error| {
            json!({
                "enabled": true,
                "scripts": [],
                "error": error.to_string()
            })
        })
}

fn failed_script_market_payload(message: &str) -> ScriptMarketPayload {
    ScriptMarketPayload {
        market: json!({
            "status": "failed",
            "message": message,
            "indexUrl": script_market::DEFAULT_MARKET_INDEX_URL,
            "updatedAt": "",
            "scripts": []
        }),
        user_scripts: user_script_inventory(),
    }
}

fn script_market_payload_from_manifest(
    manifest: &ScriptMarketManifest,
    status: &str,
    message: &str,
) -> ScriptMarketPayload {
    let user_scripts = user_script_inventory();
    let installed = installed_market_versions(&user_scripts);
    let scripts = manifest
        .scripts
        .iter()
        .map(|script| market_script_payload(script, &installed))
        .collect::<Vec<_>>();
    ScriptMarketPayload {
        market: json!({
            "status": status,
            "message": message,
            "indexUrl": script_market::DEFAULT_MARKET_INDEX_URL,
            "updatedAt": manifest.updated_at.clone().unwrap_or_default(),
            "scripts": scripts
        }),
        user_scripts,
    }
}

fn installed_market_versions(user_scripts: &Value) -> BTreeMap<String, String> {
    user_scripts
        .get("scripts")
        .and_then(Value::as_array)
        .map(|scripts| {
            scripts
                .iter()
                .filter_map(|script| {
                    let id = script.get("market_id").and_then(Value::as_str)?;
                    if id.is_empty() {
                        return None;
                    }
                    let version = script
                        .get("version")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    Some((id.to_string(), version))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn market_script_payload(script: &MarketScript, installed: &BTreeMap<String, String>) -> Value {
    let installed_version = installed.get(&script.id).cloned().unwrap_or_default();
    let is_installed = !installed_version.is_empty();
    json!({
        "id": script.id,
        "name": script.name,
        "description": script.description,
        "version": script.version,
        "author": script.author,
        "tags": script.tags,
        "homepage": script.homepage,
        "script_url": script.script_url,
        "sha256": script.sha256,
        "installed": is_installed,
        "installedVersion": installed_version,
        "updateAvailable": is_installed && installed.get(&script.id).map(|version| version != &script.version).unwrap_or(false)
    })
}

fn default_user_script_manager() -> UserScriptManager {
    let config_dir = user_scripts_config_dir();
    UserScriptManager::new(
        builtin_user_scripts_dir(),
        config_dir.join("user_scripts"),
        config_dir.join("user_scripts.json"),
    )
}

fn user_scripts_config_dir() -> PathBuf {
    if cfg!(windows) {
        if let Some(roaming) = std::env::var_os("APPDATA") {
            return PathBuf::from(roaming).join("mirrorplus");
        }
    }
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| directories::BaseDirs::new().map(|dirs| dirs.home_dir().join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("mirrorplus")
}

fn builtin_user_scripts_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .map(|path| path.join("user_scripts"))
        .unwrap_or_else(|| PathBuf::from("user_scripts"))
}

fn diagnostics_report() -> String {
    let (codex_app_path, entrypoints, latest_launch) = load_overview_payload();
    let overview = ok(
        "概览已加载。",
        OverviewPayload {
            codex_version: codex_app_path
                .as_deref()
                .and_then(codex_plus_core::app_paths::codex_app_version),
            codex_app: path_state(codex_app_path),
            silent_shortcut: shortcut_state(entrypoints.silent_shortcut),
            management_shortcut: shortcut_state(entrypoints.management_shortcut),
            latest_launch,
            current_version: codex_plus_core::version::VERSION.to_string(),
            update_status: "not_checked".to_string(),
            settings_path: codex_plus_core::paths::default_settings_path()
                .to_string_lossy()
                .to_string(),
            logs_path: codex_plus_core::paths::default_diagnostic_log_path()
                .to_string_lossy()
                .to_string(),
        },
    );
    let settings_result = SettingsStore::default().load();
    let settings_error = settings_result
        .as_ref()
        .err()
        .map(|error| format!("{error:#}"));
    let settings = settings_result.ok();
    let generated_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    serde_json::to_string_pretty(&json!({
        "generatedAtMs": generated_at_ms,
        "version": codex_plus_core::version::VERSION,
        "overview": overview.payload,
        "settings": settings,
        "settingsError": settings_error,
        "logs": {
            "diagnosticLogPath": codex_plus_core::paths::default_diagnostic_log_path(),
            "latestStatusPath": codex_plus_core::paths::default_latest_status_path()
        },
        "platform": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH
        }
    }))
    .unwrap_or_else(|error| format!("诊断报告序列化失败：{error}"))
}

fn load_overview_payload() -> (
    Option<PathBuf>,
    install::EntryPointState,
    Option<LaunchStatus>,
) {
    let settings = SettingsStore::default().load().ok();
    (
        codex_plus_core::app_paths::resolve_codex_app_dir_with_saved(
            None,
            settings
                .as_ref()
                .map(|settings| settings.codex_app_path.as_str()),
        ),
        install::inspect_entrypoints(),
        StatusStore::default().load_latest().unwrap_or(None),
    )
}

fn install_background_failure(action: &str, error: impl std::fmt::Display) -> InstallActionResult {
    let state = install::inspect_entrypoints();
    InstallActionResult {
        status: "failed".to_string(),
        message: format!("{action}后台任务失败：{error}"),
        silent_shortcut: state.silent_shortcut,
        management_shortcut: state.management_shortcut,
    }
}

fn watcher_payload() -> WatcherPayload {
    let flag = codex_plus_core::watcher::default_watcher_disabled_flag();
    WatcherPayload {
        enabled: !flag.exists(),
        disabled_flag: flag.to_string_lossy().to_string(),
    }
}

const MAX_LOG_TAIL_READ_BYTES: u64 = 2 * 1024 * 1024;

fn read_tail(path: &Path, max_lines: usize) -> std::io::Result<String> {
    let mut file = fs::File::open(path)?;
    let file_size = file.metadata()?.len();
    if max_lines == 0 || file_size == 0 {
        return Ok(String::new());
    }
    let read_len = MAX_LOG_TAIL_READ_BYTES.min(file_size);
    let start = file_size - read_len;
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity(read_len as usize);
    file.read_to_end(&mut bytes)?;
    if start > 0 {
        if let Some(pos) = bytes.iter().position(|byte| *byte == b'\n') {
            bytes.drain(..=pos);
        }
    }
    let contents = String::from_utf8_lossy(&bytes);
    let mut lines = contents.lines().rev().take(max_lines).collect::<Vec<_>>();
    lines.reverse();
    Ok(lines.join("\n"))
}

fn path_state(path: Option<PathBuf>) -> PathState {
    match path {
        Some(path) => PathState {
            status: "found".to_string(),
            path: Some(path.to_string_lossy().to_string()),
        },
        None => PathState {
            status: "missing".to_string(),
            path: None,
        },
    }
}

fn shortcut_state(shortcut: install::ShortcutState) -> PathState {
    PathState {
        status: if shortcut.installed {
            "installed".to_string()
        } else {
            "missing".to_string()
        },
        path: shortcut.path,
    }
}

fn ok<T: Serialize>(message: &str, payload: T) -> CommandResult<T> {
    CommandResult {
        status: "ok".to_string(),
        message: message.to_string(),
        payload,
    }
}

fn failed<T: Serialize>(message: &str, payload: T) -> CommandResult<T> {
    CommandResult {
        status: "failed".to_string(),
        message: message.to_string(),
        payload,
    }
}

fn degraded<T: Serialize>(message: &str, payload: T) -> CommandResult<T> {
    CommandResult {
        status: "degraded".to_string(),
        message: message.to_string(),
        payload,
    }
}

fn default_debug_port() -> u16 {
    9229
}

fn default_helper_port() -> u16 {
    57321
}

fn default_log_lines() -> usize {
    200
}

// ---------------------------------------------------------------------------
// Mobile control (phone access to local Codex through the encrypted relay)
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn get_mobile_control_status() -> CommandResult<Value> {
    ok(
        "手机控制状态已读取。",
        json!({ "mobileControl": crate::mobile_control::status() }),
    )
}

#[tauri::command]
pub async fn enable_mobile_control() -> CommandResult<Value> {
    let store = SettingsStore::default();
    let mut settings = match store.load() {
        Ok(settings) => settings,
        Err(error) => {
            return failed(
                &format!("开启手机控制已停止：无法读取 Manager 设置：{error:#}。未启动桌面桥接。"),
                json!({ "mobileControl": crate::mobile_control::status() }),
            );
        }
    };

    match crate::mobile_control::start(&settings).await {
        Ok(_) => {
            settings.mobile_control_enabled = true;
            if let Err(error) = store.save(&settings) {
                // The host is already running; surface the persistence failure so
                // the user knows it will not survive a restart.
                return failed(
                    &format!("手机控制已启动，但设置保存失败：{error}"),
                    json!({ "mobileControl": crate::mobile_control::status() }),
                );
            }
            ok(
                "手机控制已开启，请用手机扫码或打开链接。",
                json!({ "mobileControl": crate::mobile_control::status() }),
            )
        }
        Err(error) => failed(
            &error,
            json!({ "mobileControl": crate::mobile_control::status() }),
        ),
    }
}

#[tauri::command]
pub async fn disable_mobile_control() -> CommandResult<Value> {
    crate::mobile_control::stop_async().await;
    let store = SettingsStore::default();
    let mut settings = match store.load() {
        Ok(settings) => settings,
        Err(error) => {
            return failed(
                &format!(
                    "手机控制运行已关闭，但无法读取 Manager 设置并持久化关闭状态：{error:#}。原 settings.json 保持不变。"
                ),
                json!({ "mobileControl": crate::mobile_control::status() }),
            );
        }
    };
    settings.mobile_control_enabled = false;
    if let Err(error) = store.save(&settings) {
        return failed(
            &format!("手机控制已关闭，但设置保存失败：{error}"),
            json!({ "mobileControl": crate::mobile_control::status() }),
        );
    }
    ok(
        "手机控制已关闭，手机端将立即断开。",
        json!({ "mobileControl": crate::mobile_control::status() }),
    )
}

#[tauri::command]
pub fn generate_mobile_qr_code() -> CommandResult<Value> {
    let settings = match SettingsStore::default().load() {
        Ok(settings) => settings,
        Err(error) => {
            return failed(
                &format!("生成手机二维码已停止：无法读取 Manager 设置：{error:#}。"),
                json!({ "svg": null, "mobileUrl": null }),
            );
        }
    };
    let api_key = crate::mobile_control::active_api_key(&settings);
    if api_key.is_empty() {
        return failed(
            "请先填入镜子AI Key，再生成手机二维码。",
            json!({ "svg": null, "mobileUrl": null }),
        );
    }
    let relay_url = crate::mobile_control::effective_relay_url(&settings);
    let config = match codex_plus_core::mobile_relay_host::MobileRelayHostConfig::from_api_key(
        &api_key, &relay_url,
    ) {
        Ok(config) => config,
        Err(error) => {
            return failed(
                &format!("手机配对信息生成失败：{error}"),
                json!({ "svg": null, "mobileUrl": null }),
            );
        }
    };
    let mobile_url = config.mobile_url();
    match crate::mobile_control::qr_svg(&mobile_url) {
        Ok(svg) => ok(
            "二维码已生成，扫码即可在手机上打开。",
            json!({ "svg": svg, "mobileUrl": mobile_url }),
        ),
        Err(error) => failed(&error, json!({ "svg": null, "mobileUrl": mobile_url })),
    }
}

#[tauri::command]
pub async fn set_mobile_control_relay_url(relay_url: String) -> CommandResult<Value> {
    let trimmed = relay_url.trim().to_string();
    if !trimmed.is_empty() && !(trimmed.starts_with("wss://") || trimmed.starts_with("ws://")) {
        return failed(
            "中继地址必须以 wss:// 开头（iOS 只允许加密连接）。",
            json!({ "mobileControl": crate::mobile_control::status() }),
        );
    }
    let store = SettingsStore::default();
    let mut settings = match store.load() {
        Ok(settings) => settings,
        Err(error) => {
            return failed(
                &format!(
                    "中继地址更新已停止：无法读取 Manager 设置：{error:#}。原 settings.json 保持不变。"
                ),
                json!({ "mobileControl": crate::mobile_control::status() }),
            );
        }
    };
    settings.mobile_control_relay_url = if trimmed.is_empty() {
        codex_plus_core::settings::default_mobile_control_relay_url()
    } else {
        trimmed
    };
    if let Err(error) = store.save(&settings) {
        return failed(
            &format!("中继地址保存失败：{error}"),
            json!({ "mobileControl": crate::mobile_control::status() }),
        );
    }
    // Re-point an already running host at the new relay.
    if settings.mobile_control_enabled {
        let _ = crate::mobile_control::start(&settings).await;
    }
    ok(
        "中继地址已更新。",
        json!({ "mobileControl": crate::mobile_control::status() }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirror_preflight_exposes_corrupt_settings_as_blocking_check() {
        let check =
            mirror_settings_preflight_check(Some("settings.json 不是有效 JSON；原文件已保留"));

        assert_eq!(check.id, "settings");
        assert!(!check.ready);
        assert!(check.detail.contains("原文件已保留"));
        assert!(check.detail.contains("不会使用默认设置"));
    }

    #[test]
    fn mirror_preflight_never_probes_state_directories_while_codex_runs() {
        assert!(!should_probe_codex_state_directories(true, true));
        assert!(!should_probe_codex_state_directories(false, false));
        assert!(should_probe_codex_state_directories(true, false));
    }

    #[test]
    fn mixed_auth_status_accepts_chatgpt_and_rejects_api_key_login() {
        let chatgpt = classify_codex_login_status(true, "Logged in using ChatGPT");
        assert!(chatgpt.ready);
        assert_eq!(chatgpt.method, "chatgpt");

        let api = classify_codex_login_status(true, "Logged in using an API key");
        assert!(!api.ready);
        assert_eq!(api.method, "apiKey");
        assert!(api.message.contains("纯 API"));
    }

    #[test]
    fn mixed_auth_status_fails_closed_for_unknown_or_signed_out_output() {
        let unknown = classify_codex_login_status(true, "Authentication status unavailable");
        assert!(!unknown.ready);
        assert_eq!(unknown.method, "unknown");

        let signed_out = classify_codex_login_status(false, "Not logged in");
        assert!(!signed_out.ready);
        assert_eq!(signed_out.method, "signedOut");
    }

    #[test]
    fn mixed_auth_status_requires_an_explicit_positive_chatgpt_marker() {
        for (success, output) in [
            (true, "ChatGPT authentication status unavailable"),
            (true, "Not logged in to ChatGPT"),
            (false, "Failed to inspect ChatGPT login"),
            (false, "API key lookup failed"),
        ] {
            let status = classify_codex_login_status(success, output);
            assert!(!status.ready, "must not accept: {output}");
        }
    }

    #[test]
    fn codex_login_output_is_bounded_and_never_returned_in_status() {
        let secret = "sk-sensitive-test-only";
        let oversized = format!("{secret}{}", "x".repeat(32 * 1024));
        let captured = bounded_codex_login_output(oversized.as_bytes(), oversized.as_bytes());
        assert!(captured.len() <= 16 * 1024);

        let status = classify_codex_login_status(false, &captured);
        assert!(!status.message.contains(secret));
        assert!(!status.method.contains(secret));
    }

    #[test]
    fn keyring_login_probe_uses_an_isolated_non_secret_codex_home() {
        let probe = TemporaryMixedAuthProbeHome::create("keyring").unwrap();
        let path = probe.path().to_path_buf();
        let config = fs::read_to_string(path.join("config.toml")).unwrap();

        assert_eq!(config, "cli_auth_credentials_store = \"keyring\"\n");
        assert!(!path.join("auth.json").exists());
        drop(probe);
        assert!(!path.exists());
    }

    #[test]
    fn mirror_key_validation_cache_is_key_bound_trimmed_and_short_lived() {
        let discovery = codex_plus_core::mirror_access::MirrorModelDiscovery {
            models: vec![codex_plus_core::mirror_access::MirrorModel {
                id: "gpt-test".to_string(),
                display_name: "GPT Test".to_string(),
                context_window: Some(128_000),
                context_source: "test".to_string(),
            }],
            default_model: "gpt-test".to_string(),
        };
        let now = Instant::now();
        let mut cache = Vec::new();
        remember_mirror_key_validation(
            &mut cache,
            "  sk-current  ",
            discovery.clone(),
            "gpt-test".to_string(),
            200,
            "https://example.invalid/v1/responses".to_string(),
            now,
        );

        let cached =
            cached_mirror_key_validation(&mut cache, "sk-current", now + Duration::from_secs(30))
                .expect("the same trimmed key should reuse its recent validation");
        assert_eq!(cached.discovery, discovery);
        assert!(
            cached_mirror_key_validation(&mut cache, "sk-other", now).is_none(),
            "another key must never inherit validation"
        );
        assert!(
            cached_mirror_key_validation(
                &mut cache,
                "sk-current",
                now + MIRROR_KEY_VALIDATION_TTL + Duration::from_millis(1),
            )
            .is_none(),
            "expired validation must trigger a fresh pre-write probe"
        );
    }

    #[test]
    fn mirror_enable_pause_is_degraded_and_keeps_recovery_context() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let state_dir = temp.path().join("state");
        let probes = vec![json!({ "group": "CodexPro", "httpStatus": 200 })];

        assert!(
            pause_mirror_enable_for_process_count(0, "会话修复", &home, &state_dir, &[], &probes,)
                .is_none()
        );
        let result =
            pause_mirror_enable_for_process_count(2, "会话修复", &home, &state_dir, &[], &probes)
                .expect("running Codex must pause the remaining enable steps");

        assert_eq!(result.status, "degraded");
        assert_eq!(result.payload["codexRunning"], true);
        assert_eq!(result.payload["codexProcessCount"], 2);
        assert_eq!(result.payload["pausedStage"], "会话修复");
        assert_eq!(result.payload["sessionSync"], Value::Null);
        assert_eq!(result.payload["responseProbes"], Value::Array(probes));
        assert!(result.message.contains("未在 Codex 运行时自动回滚"));
    }

    #[test]
    fn mirror_enable_reuses_recent_validation_and_never_sends_a_postwrite_probe() {
        let source = include_str!("commands.rs");
        let start = source.find("pub async fn enable_mirror_access(").unwrap();
        let end = source[start..]
            .find("\nfn probe_writable_directory(")
            .map(|offset| start + offset)
            .unwrap();
        let enable_source = &source[start..end];
        let probe_positions = enable_source
            .match_indices("probe_mirror_profile_stream(")
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let write_position = enable_source
            .find("enable_grouped_access_transaction(")
            .unwrap();
        let replacement_write_position = enable_source
            .find("enable_grouped_access_transaction_replacing_groups(")
            .unwrap();
        let auth_positions = enable_source
            .match_indices("inspect_mixed_chatgpt_auth().await")
            .map(|(index, _)| index)
            .collect::<Vec<_>>();

        assert_eq!(probe_positions.len(), 1);
        assert!(probe_positions[0] < write_position);
        assert_eq!(auth_positions.len(), 2);
        assert!(auth_positions[1] < replacement_write_position);
        assert!(enable_source.contains("replace_existing_groups: Option<bool>"));
        assert!(enable_source.contains("recent_mirror_key_validation("));
        assert!(enable_source.contains("\"networkRequestSent\": false"));
        assert!(!enable_source.contains("落盘后真实流式 /responses 复验失败"));
        assert!(
            enable_source
                .matches("pause_mirror_enable_if_codex_started(")
                .count()
                >= 5
        );
    }

    #[test]
    fn restart_waits_for_launcher_cleanup_without_force_terminating_it() {
        let source = include_str!("commands.rs");
        let start = source.find("pub async fn restart_codex_plus(").unwrap();
        let end = source[start..]
            .find("\nasync fn spawn_codex_plus_launch(")
            .map(|offset| start + offset)
            .unwrap();
        let restart_source = &source[start..end];

        assert!(restart_source.contains("launch_operation_mutex().try_lock()"));
        assert!(restart_source.contains("wait_for_launcher_processes_to_exit()"));
        assert!(!restart_source.contains("stop_launcher_processes"));
        assert!(!restart_source.contains("terminate_process"));
    }

    #[test]
    fn repeated_launch_message_explains_that_existing_window_is_reused() {
        let existing = LaunchStatus {
            status: "running_degraded".to_string(),
            message: "Existing Codex window activated without changing its files; mirror enhancements were not re-injected or verified.".to_string(),
            started_at_ms: 1,
            debug_port: Some(9229),
            helper_port: Some(9230),
            codex_app: None,
        };
        let first_launch = LaunchStatus {
            message: "Codex launched; mirror+ enhancements are still waiting for the page bridge."
                .to_string(),
            ..existing.clone()
        };

        assert!(degraded_launch_message(&existing).contains("没有启动第二个实例"));
        assert!(degraded_launch_message(&first_launch).contains("无需再次点击启动"));
    }

    #[test]
    fn normal_launch_waits_for_previous_launcher_cleanup() {
        let source = include_str!("commands.rs");
        let start = source.find("async fn spawn_codex_plus_launch(").unwrap();
        let end = source[start..]
            .find("\nfn validate_settings_before_launch(")
            .map(|offset| start + offset)
            .unwrap();
        let launch_source = &source[start..end];

        assert!(launch_source.contains("find_codex_processes().is_empty()"));
        assert!(launch_source.contains("wait_for_launcher_processes_to_exit()"));
        assert!(!launch_source.contains("stop_launcher_processes"));
        assert!(!launch_source.contains("terminate_process"));
    }

    #[test]
    fn mirror_preflight_deduplicates_storage_paths_by_volume() {
        #[cfg(windows)]
        let paths = vec![
            PathBuf::from(r"C:\Users\test\.codex"),
            PathBuf::from(r"c:\Users\test\AppData\Local"),
            PathBuf::from(r"D:\Portable\Codex"),
        ];
        #[cfg(not(windows))]
        let paths = vec![PathBuf::from("/tmp/codex"), PathBuf::from("/var/tmp/codex")];

        let deduplicated = codex_plus_core::mirror_access::storage_paths_by_volume(paths);

        #[cfg(windows)]
        assert_eq!(deduplicated.len(), 2);
        #[cfg(not(windows))]
        assert_eq!(deduplicated.len(), 1);
    }

    #[test]
    fn backend_version_returns_structured_payload() {
        let result = backend_version();

        assert_eq!(result.status, "ok");
        assert!(!result.payload.version.is_empty());
    }

    #[test]
    fn startup_options_returns_structured_payload() {
        let result = startup_options();

        assert_eq!(result.status, "ok");
    }

    #[test]
    fn startup_options_honors_show_update_environment() {
        unsafe {
            std::env::set_var("CODEX_PLUS_SHOW_UPDATE", "1");
        }

        let result = startup_options();

        unsafe {
            std::env::remove_var("CODEX_PLUS_SHOW_UPDATE");
        }

        assert_eq!(result.status, "ok");
        assert!(result.payload.show_update);
    }

    #[test]
    fn startup_options_honors_show_update_argument() {
        assert!(should_show_update(
            ["mirror-x-codex-manager.exe", "--show-update"],
            None
        ));
    }

    #[test]
    fn overview_contains_expected_operational_fields() {
        let result = tauri::async_runtime::block_on(load_overview());

        assert_eq!(result.status, "ok");
        assert!(!result.payload.current_version.is_empty());
        assert!(
            result.payload.codex_version.is_none()
                || result
                    .payload
                    .codex_version
                    .as_deref()
                    .is_some_and(|version| !version.is_empty())
        );
        assert!(matches!(
            result.payload.codex_app.status.as_str(),
            "found" | "missing"
        ));
        assert!(matches!(
            result.payload.silent_shortcut.status.as_str(),
            "installed" | "missing"
        ));
    }

    #[test]
    fn update_install_requires_release_payload() {
        let result = tauri::async_runtime::block_on(perform_update(None));

        assert_eq!(result.status, "failed");
        assert!(result.message.contains("请先检查更新"));
    }

    #[test]
    fn watcher_state_returns_disabled_flag_path() {
        let result = load_watcher_state();

        assert_eq!(result.status, "ok");
        assert!(result.payload.disabled_flag.contains("watcher.disabled"));
    }

    #[test]
    fn missing_logs_return_failed_status() {
        let result = read_latest_logs(LogRequest { lines: 25 });

        if result.payload.text.is_empty() {
            assert_eq!(result.status, "failed");
        }
    }

    #[test]
    fn relay_payload_does_not_expose_token_text() {
        let payload = relay_payload(
            codex_plus_core::relay_config::RelayStatus {
                authenticated: true,
                auth_source: "registry.json".to_string(),
                account_label: Some("user@example.test".to_string()),
                config_path: "config.toml".to_string(),
                configured: true,
                requires_openai_auth: true,
                has_bearer_token: true,
                state_unreadable: false,
                state_error: None,
            },
            None,
        );
        let text = serde_json::to_string(&payload).unwrap();

        assert!(!text.contains("sk-"));
        assert!(text.contains("hasBearerToken"));
        assert!(text.contains("stateUnreadable"));
    }

    #[test]
    fn provider_doctor_recommendation_prioritizes_actionable_failures() {
        let recommendation = provider_doctor_recommendation(&[
            ProviderDoctorCheck {
                id: "models".to_string(),
                title: "模型列表".to_string(),
                status: "failed".to_string(),
                detail: "上游不支持 /v1/models".to_string(),
            },
            ProviderDoctorCheck {
                id: "request".to_string(),
                title: "真实请求".to_string(),
                status: "failed".to_string(),
                detail: "HTTP 404".to_string(),
            },
        ]);

        assert!(recommendation.contains("/v1/models"));
    }

    #[test]
    fn provider_doctor_recommendation_reports_model_warning() {
        let recommendation = provider_doctor_recommendation(&[
            ProviderDoctorCheck {
                id: "config".to_string(),
                title: "配置完整性".to_string(),
                status: "ok".to_string(),
                detail: "https://example.test/v1 / Responses API".to_string(),
            },
            ProviderDoctorCheck {
                id: "models".to_string(),
                title: "模型列表".to_string(),
                status: "warning".to_string(),
                detail: "未看到测试模型".to_string(),
            },
            ProviderDoctorCheck {
                id: "request".to_string(),
                title: "真实请求".to_string(),
                status: "ok".to_string(),
                detail: "HTTP 200".to_string(),
            },
        ]);

        assert!(recommendation.contains("测试模型"));
    }

    #[test]
    fn aggregate_relay_injection_writes_local_proxy_without_chatgpt_auth() {
        let temp = tempfile::tempdir().unwrap();

        let result = apply_aggregate_relay_injection_to_home(temp.path()).unwrap();
        let status = codex_plus_core::relay_config::relay_status_from_home(temp.path());
        let config = std::fs::read_to_string(temp.path().join("config.toml")).unwrap();

        assert!(result.configured);
        assert!(!status.authenticated);
        assert!(config.contains(r#"base_url = "http://127.0.0.1:57321/v1""#));
        assert!(config.contains(r#"experimental_bearer_token = "codex-plus-aggregate""#));
    }

    #[test]
    fn relay_files_payload_reads_config_and_auth_contents() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("config.toml"),
            "model_provider = \"custom\"\n",
        )
        .unwrap();
        std::fs::write(
            temp.path().join("auth.json"),
            "{\"OPENAI_API_KEY\":\"sk-test\"}\n",
        )
        .unwrap();

        let payload = relay_files_payload_from_home(temp.path()).unwrap();

        assert!(payload.config_path.ends_with("config.toml"));
        assert!(payload.auth_path.ends_with("auth.json"));
        assert_eq!(payload.config_contents, "model_provider = \"custom\"\n");
        assert_eq!(payload.auth_contents, "{\"OPENAI_API_KEY\":\"sk-test\"}\n");
    }

    #[test]
    fn env_conflict_commands_ignore_codex_home_and_remove_openai_vars() {
        let test_openai_name = "OPENAI_CODEX_PLUS_ENV_CONFLICT_TEST";
        let previous_openai = std::env::var_os(test_openai_name);
        let previous_codex_home = std::env::var_os("CODEX_HOME");
        let temp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var(test_openai_name, "sk-test");
            std::env::set_var("CODEX_HOME", temp.path());
        }

        let check = check_env_conflicts();
        assert_eq!(check.status, "ok");
        assert!(
            check
                .payload
                .conflicts
                .iter()
                .any(|item| item.name == test_openai_name)
        );
        assert!(
            !check
                .payload
                .conflicts
                .iter()
                .any(|item| item.name == "CODEX_HOME")
        );

        codex_plus_core::env_conflicts::remove_process_env_conflicts_for_tests(
            &[test_openai_name.to_string(), "CODEX_HOME".to_string()],
            codex_plus_core::paths::default_app_state_dir().join("test-backups"),
        )
        .unwrap();
        assert!(std::env::var_os(test_openai_name).is_none());
        assert_eq!(
            std::env::var_os("CODEX_HOME"),
            Some(temp.path().as_os_str().to_os_string())
        );

        unsafe {
            match previous_openai {
                Some(value) => std::env::set_var(test_openai_name, value),
                None => std::env::remove_var(test_openai_name),
            }
            match previous_codex_home {
                Some(value) => std::env::set_var("CODEX_HOME", value),
                None => std::env::remove_var("CODEX_HOME"),
            }
        }
    }

    #[test]
    fn delete_local_session_falls_back_when_requested_db_no_longer_contains_thread() {
        let temp = tempfile::tempdir().unwrap();
        let previous_codex_home = std::env::var_os("CODEX_HOME");
        let codex_home = temp.path().join("codex-home");
        let sqlite_dir = codex_home.join("sqlite");
        std::fs::create_dir_all(&sqlite_dir).unwrap();
        let stale_db = sqlite_dir.join("codex-dev.db");
        let active_db = sqlite_dir.join("state_5.sqlite");
        let rollout_path = codex_home.join("rollout.jsonl");
        std::fs::write(&rollout_path, "{\"type\":\"message\"}\n").unwrap();
        let stale = rusqlite::Connection::open(&stale_db).unwrap();
        stale
            .execute(
                "CREATE TABLE threads (id TEXT PRIMARY KEY, rollout_path TEXT, title TEXT)",
                [],
            )
            .unwrap();
        drop(stale);
        let active = rusqlite::Connection::open(&active_db).unwrap();
        active
            .execute(
                "CREATE TABLE threads (id TEXT PRIMARY KEY, rollout_path TEXT, title TEXT)",
                [],
            )
            .unwrap();
        active
            .execute(
                "INSERT INTO threads VALUES ('t1', ?1, 'Active Thread')",
                [rollout_path.to_string_lossy().to_string()],
            )
            .unwrap();
        drop(active);

        unsafe {
            std::env::set_var("CODEX_HOME", &codex_home);
        }
        let result = delete_local_session(DeleteLocalSessionRequest {
            session_id: "t1".to_string(),
            title: "Active Thread".to_string(),
            db_path: Some(stale_db.to_string_lossy().to_string()),
        });
        unsafe {
            if let Some(value) = previous_codex_home {
                std::env::set_var("CODEX_HOME", value);
            } else {
                std::env::remove_var("CODEX_HOME");
            }
        }

        assert_eq!(result.status, "ok");
        assert_eq!(
            result.payload.status,
            codex_plus_core::models::DeleteStatus::LocalDeleted
        );
        let active = rusqlite::Connection::open(&active_db).unwrap();
        assert_eq!(
            active
                .query_row("SELECT COUNT(*) FROM threads WHERE id = 't1'", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
    }

    #[test]
    fn list_local_sessions_deduplicates_threads_across_current_and_legacy_dbs() {
        let temp = tempfile::tempdir().unwrap();
        let previous_codex_home = std::env::var_os("CODEX_HOME");
        let codex_home = temp.path().join("codex-home");
        let sqlite_dir = codex_home.join("sqlite");
        std::fs::create_dir_all(&sqlite_dir).unwrap();
        let current_db = sqlite_dir.join("state_5.sqlite");
        let legacy_db = codex_home.join("state_5.sqlite");
        create_minimal_thread_db(&current_db, "t1", "Current Copy", 100);
        create_minimal_thread_db(&legacy_db, "t1", "Legacy Copy", 200);

        unsafe {
            std::env::set_var("CODEX_HOME", &codex_home);
        }
        let result = list_local_sessions(None);
        restore_codex_home(previous_codex_home);

        assert_eq!(result.status, "ok");
        assert_eq!(result.payload.sessions.len(), 1);
        assert_eq!(result.payload.sessions[0].id, "t1");
        assert_eq!(result.payload.sessions[0].title, "Legacy Copy");
        assert_eq!(
            result.payload.sessions[0].db_path,
            legacy_db.to_string_lossy()
        );
    }

    #[test]
    fn delete_local_session_removes_duplicate_threads_from_all_candidate_dbs() {
        let temp = tempfile::tempdir().unwrap();
        let previous_codex_home = std::env::var_os("CODEX_HOME");
        let codex_home = temp.path().join("codex-home");
        let sqlite_dir = codex_home.join("sqlite");
        std::fs::create_dir_all(&sqlite_dir).unwrap();
        let current_db = sqlite_dir.join("state_5.sqlite");
        let legacy_db = codex_home.join("state_5.sqlite");
        create_minimal_thread_db(&current_db, "t1", "Current Copy", 100);
        create_minimal_thread_db(&legacy_db, "t1", "Legacy Copy", 200);

        unsafe {
            std::env::set_var("CODEX_HOME", &codex_home);
        }
        let result = delete_local_session(DeleteLocalSessionRequest {
            session_id: "t1".to_string(),
            title: "Legacy Copy".to_string(),
            db_path: Some(legacy_db.to_string_lossy().to_string()),
        });
        restore_codex_home(previous_codex_home);

        assert_eq!(result.status, "ok");
        assert_eq!(thread_count(&current_db, "t1"), 0);
        assert_eq!(thread_count(&legacy_db, "t1"), 0);
    }

    fn create_minimal_thread_db(path: &Path, id: &str, title: &str, updated_at_ms: i64) {
        let db = rusqlite::Connection::open(path).unwrap();
        db.execute(
            "CREATE TABLE threads (id TEXT PRIMARY KEY, rollout_path TEXT, title TEXT, updated_at_ms INTEGER)",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO threads VALUES (?1, '', ?2, ?3)",
            (id, title, updated_at_ms),
        )
        .unwrap();
    }

    fn thread_count(path: &Path, id: &str) -> i64 {
        let db = rusqlite::Connection::open(path).unwrap();
        db.query_row("SELECT COUNT(*) FROM threads WHERE id = ?1", [id], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap()
    }

    fn restore_codex_home(previous: Option<std::ffi::OsString>) {
        unsafe {
            if let Some(value) = previous {
                std::env::set_var("CODEX_HOME", value);
            } else {
                std::env::remove_var("CODEX_HOME");
            }
        }
    }

    #[test]
    fn apply_relay_profile_to_home_with_switch_rules_preserves_custom_provider_id() {
        let temp = tempfile::tempdir().unwrap();
        let profile = RelayProfile {
            relay_mode: codex_plus_core::settings::RelayMode::PureApi,
            protocol: codex_plus_core::settings::RelayProtocol::Responses,
            config_contents: "model_provider = \"ai\"\nmodel = \"gpt-image-2\"\n\n[model_providers.ai]\nname = \"ai\"\nwire_api = \"responses\"\nrequires_openai_auth = true\nbase_url = \"https://ahg.codes\"\n"
                .to_string(),
            auth_contents: "{}\n".to_string(),
            ..RelayProfile::default()
        };

        codex_plus_core::relay_config::apply_relay_profile_to_home_with_switch_rules(
            temp.path(),
            &profile,
            "",
        )
        .unwrap();

        let applied = std::fs::read_to_string(temp.path().join("config.toml")).unwrap();
        assert!(applied.contains("model_provider = \"ai\""));
        assert!(applied.contains("[model_providers.ai]"));
        assert!(!applied.contains("[model_providers.custom]"));
    }

    #[test]
    fn save_relay_file_in_home_only_allows_known_files() {
        let temp = tempfile::tempdir().unwrap();

        save_relay_file_in_home(temp.path(), "config", "model = \"gpt-5\"\n").unwrap();
        save_relay_file_in_home(temp.path(), "auth", "{}\n").unwrap();

        assert_eq!(
            std::fs::read_to_string(temp.path().join("config.toml")).unwrap(),
            "model = \"gpt-5\"\n"
        );
        assert_eq!(
            std::fs::read_to_string(temp.path().join("auth.json")).unwrap(),
            "{}\n"
        );
        assert!(save_relay_file_in_home(temp.path(), "../bad", "").is_err());
    }

    #[test]
    fn save_relay_file_in_home_rejects_invalid_content_without_overwriting_live_files() {
        let temp = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("config.toml");
        let auth_path = temp.path().join("auth.json");
        let original_config = b"model = \"gpt-5\"\n";
        let original_auth = br#"{"auth_mode":"chatgpt"}"#;
        std::fs::write(&config_path, original_config).unwrap();
        std::fs::write(&auth_path, original_auth).unwrap();

        assert!(save_relay_file_in_home(temp.path(), "config", "model = [").is_err());
        assert_eq!(std::fs::read(&config_path).unwrap(), original_config);
        assert_eq!(std::fs::read(&auth_path).unwrap(), original_auth);

        assert!(save_relay_file_in_home(temp.path(), "auth", "{bad json").is_err());
        assert_eq!(std::fs::read(&config_path).unwrap(), original_config);
        assert_eq!(std::fs::read(&auth_path).unwrap(), original_auth);
    }

    #[test]
    fn normalize_settings_before_save_preserves_profile_context_until_manual_extract() {
        let settings = BackendSettings {
            relay_common_config_contents: "[mcp_servers.context7]\ncommand = \"npx\"\n".to_string(),
            relay_profiles: vec![RelayProfile {
                use_common_config: false,
                config_contents: "model = \"gpt-5\"\n\n[mcp_servers.context7]\ncommand = \"npx\"\n"
                    .to_string(),
                ..RelayProfile::default()
            }],
            ..BackendSettings::default()
        };

        let normalized = normalize_settings_before_save(settings);

        assert!(
            normalized.relay_profiles[0]
                .config_contents
                .contains("model = \"gpt-5\"")
        );
        assert!(
            normalized.relay_profiles[0]
                .config_contents
                .contains("[mcp_servers.context7]")
        );
        assert!(
            normalized
                .relay_context_config_contents
                .contains("[mcp_servers.context7]")
        );
        assert!(
            !normalized
                .relay_common_config_contents
                .contains("[mcp_servers")
        );
    }

    #[test]
    fn reset_image_overlay_settings_preserves_supplier_settings() {
        let _lock = crate::settings_path_test_lock().lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let settings_path = temp.path().join("settings.json");
        let previous = codex_plus_core::paths::set_settings_path_for_tests(Some(settings_path));

        let settings = BackendSettings {
            codex_app_image_overlay_enabled: true,
            codex_app_image_overlay_path: "C:\\Users\\me\\Pictures\\overlay.png".to_string(),
            codex_app_image_overlay_opacity: 42,
            codex_app_image_overlay_fit_mode: "fill".to_string(),
            active_relay_id: "supplier-a".to_string(),
            relay_profiles: vec![RelayProfile {
                id: "supplier-a".to_string(),
                name: "供应商 A".to_string(),
                relay_mode: codex_plus_core::settings::RelayMode::PureApi,
                api_key: "sk-test".to_string(),
                ..RelayProfile::default()
            }],
            ..BackendSettings::default()
        };
        SettingsStore::default().save(&settings).unwrap();

        let result = reset_image_overlay_settings();
        codex_plus_core::paths::set_settings_path_for_tests(previous);

        assert_eq!(result.status, "ok");
        assert!(!result.payload.settings.codex_app_image_overlay_enabled);
        assert_eq!(result.payload.settings.codex_app_image_overlay_path, "");
        assert_eq!(result.payload.settings.codex_app_image_overlay_opacity, 35);
        assert_eq!(
            result.payload.settings.codex_app_image_overlay_fit_mode,
            "fit"
        );
        assert_eq!(result.payload.settings.active_relay_id, "supplier-a");
        assert_eq!(result.payload.settings.relay_profiles.len(), 1);
        assert_eq!(result.payload.settings.relay_profiles[0].id, "supplier-a");
        assert_eq!(result.payload.settings.relay_profiles[0].api_key, "sk-test");
    }

    #[test]
    fn normalize_settings_before_save_drops_an_invalid_codex_app_path() {
        let settings = BackendSettings {
            codex_app_path: if cfg!(windows) {
                r"D:\Mirror X Codex\mirror-x-codex-manager.exe".to_string()
            } else {
                "/Applications/Mirror X Codex/mirror-x-codex-manager".to_string()
            },
            ..BackendSettings::default()
        };

        assert!(
            normalize_settings_before_save(settings)
                .codex_app_path
                .is_empty()
        );
    }

    #[test]
    fn normalize_settings_before_save_keeps_an_empty_codex_app_path_empty() {
        let settings = BackendSettings {
            codex_app_path: "   ".to_string(),
            ..BackendSettings::default()
        };

        assert!(
            normalize_settings_before_save(settings)
                .codex_app_path
                .is_empty()
        );
    }

    #[test]
    fn read_tail_does_not_load_a_large_log_prefix() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("mirror-x.log");
        let mut contents = String::from("prefix-should-not-appear\n");
        contents.push_str(&"x".repeat(MAX_LOG_TAIL_READ_BYTES as usize + 128));
        contents.push_str("\nlast-1\nlast-2\n");
        std::fs::write(&path, contents).unwrap();

        let result = read_tail(&path, 2).unwrap();
        assert_eq!(result, "last-1\nlast-2");
        assert!(!result.contains("prefix-should-not-appear"));
    }

    #[test]
    fn normalize_settings_before_save_preserves_official_profile_auth() {
        let settings = BackendSettings {
            relay_profiles: vec![RelayProfile {
                relay_mode: codex_plus_core::settings::RelayMode::Official,
                official_mix_api_key: false,
                auth_contents: r#"{"auth_mode":"chatgpt","tokens":{"access_token":"edited"}}"#
                    .to_string(),
                config_contents: "model_provider = \"custom\"\n".to_string(),
                ..RelayProfile::default()
            }],
            ..BackendSettings::default()
        };

        let normalized = normalize_settings_before_save(settings);

        let auth_json: serde_json::Value =
            serde_json::from_str(&normalized.relay_profiles[0].auth_contents).unwrap();
        assert_eq!(
            auth_json,
            serde_json::json!({
                "auth_mode": "chatgpt",
                "tokens": {
                    "access_token": "edited"
                }
            })
        );
        assert!(normalized.relay_profiles[0].config_contents.is_empty());
    }

    #[test]
    fn normalize_settings_before_save_strips_common_from_enabled_profile() {
        let settings = BackendSettings {
            relay_common_config_contents: r#"model_reasoning_effort = "high"

[features]
goals = true

[plugins."superpowers@openai-curated"]
enabled = true
"#
            .to_string(),
            relay_profiles: vec![RelayProfile {
                use_common_config: true,
                config_contents: r#"model = "gpt-5"
model_reasoning_effort = "high"

[features]
goals = true
model_reasoning_effort = "high"

[plugins."superpowers@openai-curated"]
enabled = true
"#
                .to_string(),
                ..RelayProfile::default()
            }],
            ..BackendSettings::default()
        };

        let normalized = normalize_settings_before_save(settings);
        let config = &normalized.relay_profiles[0].config_contents;

        assert!(config.contains("model = \"gpt-5\""));
        assert!(!config.contains("model_reasoning_effort"));
        assert!(!config.contains("[features]"));
        assert!(!config.contains("[plugins.\"superpowers@openai-curated\"]"));
    }

    #[test]
    fn normalize_settings_before_save_repairs_invalid_profile_common_duplication() {
        let settings = BackendSettings {
            relay_common_config_contents: r#"model_reasoning_effort = "high"

[marketplaces.openai-bundled]
last_updated = "2026-05-25T11:52:46Z"
"#
            .to_string(),
            relay_profiles: vec![RelayProfile {
                use_common_config: true,
                config_contents: r#"model = "gpt-5"
model_reasoning_effort = "high"

[marketplaces.openai-bundled]
last_updated = "2026-05-25T11:52:46Z"

[marketplaces.openai-bundled]
last_updated = "2026-05-25T11:52:46Z"
"#
                .to_string(),
                ..RelayProfile::default()
            }],
            ..BackendSettings::default()
        };

        let normalized = normalize_settings_before_save(settings);
        let config = &normalized.relay_profiles[0].config_contents;

        assert!(config.contains("model = \"gpt-5\""));
        assert!(!config.contains("model_reasoning_effort"));
        assert!(!config.contains("[marketplaces.openai-bundled]"));
    }

    #[test]
    fn normalize_settings_before_save_removes_model_catalog_from_common_config() {
        let settings = BackendSettings {
            relay_common_config_contents: r#"model_catalog_json = "C:\\Users\\Administrator\\.codex\\model-catalogs\\relay-a.json"
model_catalog_json = 'C:\Users\Administrator\.codex\model-catalogs\relay-b.json'
model_reasoning_effort = "high"
"#
            .to_string(),
            ..BackendSettings::default()
        };

        let normalized = normalize_settings_before_save(settings);

        assert!(
            !normalized
                .relay_common_config_contents
                .contains("model_catalog_json")
        );
        assert!(
            normalized
                .relay_common_config_contents
                .contains("model_reasoning_effort = \"high\"")
        );
    }

    #[test]
    fn context_entry_commands_update_settings_payload() {
        let settings = BackendSettings::default();
        let upsert = upsert_context_entry(ContextEntryRequest {
            settings: settings.clone(),
            kind: "mcp".to_string(),
            id: "context7".to_string(),
            toml_body: "command = \"npx\"\n".to_string(),
        });

        assert_eq!(upsert.status, "ok");
        assert!(
            upsert
                .payload
                .settings
                .relay_context_config_contents
                .contains("[mcp_servers.context7]")
        );

        let listed = list_context_entries(ContextSettingsRequest {
            settings: upsert.payload.settings.clone(),
        });
        assert_eq!(listed.payload.entries.mcp_servers[0].id, "context7");

        let deleted = delete_context_entry(ContextDeleteRequest {
            settings: upsert.payload.settings,
            kind: "mcp".to_string(),
            id: "context7".to_string(),
        });
        assert_eq!(deleted.status, "ok");
        assert!(
            !deleted
                .payload
                .settings
                .relay_context_config_contents
                .contains("[mcp_servers.context7]")
        );
    }

    #[test]
    fn ads_payload_keeps_version_and_ad_items() {
        let payload = ads_payload(json!({
            "version": 1,
            "ads": [{"id": "ad-1", "type": "normal", "title": "Ad"}]
        }));

        assert_eq!(payload.version, 1);
        assert_eq!(payload.ads.len(), 1);
        assert_eq!(payload.ads[0]["id"], json!("ad-1"));
    }

    #[test]
    fn open_external_url_rejects_non_http_urls() {
        let result = open_external_url("file:///C:/Windows/win.ini".to_string());

        assert_eq!(result.status, "failed");
        assert!(result.message.contains("只允许打开 http 或 https 链接"));
    }
}
