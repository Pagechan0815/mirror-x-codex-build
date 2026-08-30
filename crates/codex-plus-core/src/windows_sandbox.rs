use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::time::{Duration, Instant, timeout};

const MAX_MARKER_BYTES: u64 = 1024 * 1024;
const MAX_CONFIG_BYTES: u64 = 16 * 1024 * 1024;
const MAX_STATE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_PROCESS_ERROR_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowsSandboxDiagnostic {
    pub platform_supported: bool,
    pub codex_home: String,
    pub config_exists: bool,
    pub config_readable: bool,
    pub config_valid: bool,
    pub config_contains_nul: bool,
    pub sandbox_dir_exists: bool,
    pub marker_exists: bool,
    pub marker_readable: bool,
    pub marker_valid: bool,
    pub marker_version: Option<u64>,
    pub sandbox_users_exists: bool,
    pub sandbox_users_readable: bool,
    pub sandbox_users_valid: bool,
    pub sandbox_users_version: Option<u64>,
    pub deny_read_state_exists: bool,
    pub deny_read_state_readable: bool,
    pub deny_read_state_valid: bool,
    pub deny_read_state_contains_nul: bool,
    pub sandbox_log_exists: bool,
    pub latest_log_modified_at_ms: Option<u64>,
    pub directory_readable: bool,
    pub configured_mode: Option<String>,
    pub free_space_bytes: Option<u64>,
    pub official_readiness: Option<String>,
    pub official_check_error: Option<String>,
    pub codex_cli_path: Option<String>,
    pub codex_cli_source: Option<String>,
    pub codex_cli_user_agent: Option<String>,
    pub app_server_codex_home: Option<String>,
    pub full_access_configured: bool,
    pub full_access_check_error: Option<String>,
    pub update_action: Option<String>,
    pub status: String,
    pub blocking: bool,
    pub message: String,
    pub recommended_action: String,
}

pub fn diagnose_default() -> WindowsSandboxDiagnostic {
    diagnose(
        &crate::codex_home::default_codex_home_dir(),
        cfg!(target_os = "windows"),
    )
}

pub fn diagnose(home: &Path, platform_supported: bool) -> WindowsSandboxDiagnostic {
    let sandbox_dir = home.join(".sandbox");
    let marker_path = sandbox_dir.join("setup_marker.json");
    let sandbox_users_path = home.join(".sandbox-secrets").join("sandbox_users.json");
    let deny_read_state_path = sandbox_dir.join("deny_read_acl_state.json");
    let config = inspect_toml_file(&home.join("config.toml"), MAX_CONFIG_BYTES);
    let marker = inspect_versioned_json_file(
        &marker_path,
        MAX_MARKER_BYTES,
        VersionedJsonKind::SetupMarker,
    );
    let sandbox_users = inspect_versioned_json_file(
        &sandbox_users_path,
        MAX_STATE_BYTES,
        VersionedJsonKind::SandboxUsers,
    );
    let deny_read_state = inspect_deny_read_state_file(&deny_read_state_path, MAX_STATE_BYTES);
    let configured_mode = config
        .value
        .as_ref()
        .and_then(configured_sandbox_mode_from_value);
    let free_space_bytes =
        nearest_existing_path(home).and_then(|path| fs2::available_space(path).ok());

    let mut result = WindowsSandboxDiagnostic {
        platform_supported,
        codex_home: home.to_string_lossy().to_string(),
        config_exists: config.exists,
        config_readable: config.readable,
        config_valid: config.valid,
        config_contains_nul: config.contains_nul,
        sandbox_dir_exists: sandbox_dir.is_dir(),
        marker_exists: marker.exists,
        marker_readable: marker.readable,
        marker_valid: marker.valid,
        marker_version: marker.version,
        sandbox_users_exists: sandbox_users.exists,
        sandbox_users_readable: sandbox_users.readable,
        sandbox_users_valid: sandbox_users.valid,
        sandbox_users_version: sandbox_users.version,
        deny_read_state_exists: deny_read_state.exists,
        deny_read_state_readable: deny_read_state.readable,
        deny_read_state_valid: deny_read_state.valid,
        deny_read_state_contains_nul: deny_read_state.contains_nul,
        sandbox_log_exists: false,
        latest_log_modified_at_ms: None,
        directory_readable: false,
        configured_mode,
        free_space_bytes,
        official_readiness: None,
        official_check_error: None,
        codex_cli_path: None,
        codex_cli_source: None,
        codex_cli_user_agent: None,
        app_server_codex_home: None,
        full_access_configured: false,
        full_access_check_error: None,
        update_action: None,
        status: "unknown".to_string(),
        blocking: true,
        message: String::new(),
        recommended_action: String::new(),
    };

    if !platform_supported {
        result.status = "unsupported_platform".to_string();
        result.blocking = false;
        result.message = "当前系统不使用 Windows 原生执行环境。".to_string();
        result.recommended_action = "无需执行 Windows Sandbox 设置。".to_string();
        return result;
    }

    if config.exists && (!config.readable || !config.valid) {
        result.status = "config_invalid".to_string();
        result.message = if config.contains_nul {
            "Codex config.toml 含 NUL 字节，真实 Codex 无法可靠加载配置。"
        } else if !config.readable {
            "Codex config.toml 存在，但当前交互用户无法完整读取。"
        } else {
            "Codex config.toml 不是有效 TOML；官方桌面端可能把它误显示为 Windows 设置失败。"
        }
        .to_string();
        result.recommended_action =
            "不要删除整个 .codex；先备份并仅修复 config.toml，再重新检测。".to_string();
        return result;
    }

    if deny_read_state.exists && (!deny_read_state.readable || !deny_read_state.valid) {
        result.status = "acl_state_invalid".to_string();
        result.message = if deny_read_state.contains_nul {
            "deny_read_acl_state.json 含 NUL 字节，文件访问 ACL 状态已损坏。"
        } else if !deny_read_state.readable {
            "deny_read_acl_state.json 存在，但当前交互用户无法读取。"
        } else {
            "deny_read_acl_state.json 不是 Codex 可接受的状态结构。"
        }
        .to_string();
        result.recommended_action = "保留 ACL 现状，先备份并仅隔离已确认损坏的 deny_read_acl_state.json；不要递归 takeown/icacls 或删除 .sandbox。".to_string();
        return result;
    }

    if !home.is_dir() || !result.sandbox_dir_exists {
        result.status = "not_configured".to_string();
        result.message = "尚未发现 Codex Windows 执行环境的初始化目录。".to_string();
        result.recommended_action =
            "从本工具打开 Codex，按提示完成 Windows 设置；不要选择“继续使用受限访问”。".to_string();
        return result;
    }

    let entries = match fs::read_dir(&sandbox_dir) {
        Ok(entries) => entries,
        Err(_) => {
            result.status = "permission_blocked".to_string();
            result.message = "Windows 执行环境目录存在，但当前用户无法读取。".to_string();
            result.recommended_action =
                "完全退出 Codex 后检查该目录权限，再回到本工具重新检测。".to_string();
            return result;
        }
    };
    result.directory_readable = true;

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == "sandbox.log" || (name.starts_with("sandbox.") && name.ends_with(".log")) {
            result.sandbox_log_exists = true;
            let modified_at = entry
                .metadata()
                .ok()
                .and_then(|metadata| metadata.modified().ok())
                .and_then(system_time_ms);
            result.latest_log_modified_at_ms = match (result.latest_log_modified_at_ms, modified_at)
            {
                (Some(current), Some(candidate)) => Some(current.max(candidate)),
                (None, value) => value,
                (value, None) => value,
            };
        }
    }

    if !result.marker_exists {
        result.status = if result.sandbox_log_exists {
            "partial_setup"
        } else {
            "not_configured"
        }
        .to_string();
        result.message = if result.sandbox_log_exists {
            "检测到 Windows 执行日志，但初始化标记缺失，设置可能未完整完成。"
        } else {
            "Windows 执行环境尚未完成初始化。"
        }
        .to_string();
        result.recommended_action =
            "完全退出 Codex 后重新打开并完成 Windows 设置；不要删除 .sandbox 或直接修改 ACL。"
                .to_string();
        return result;
    }

    if !result.marker_valid {
        result.status = "sandbox_state_invalid".to_string();
        result.message = if !result.marker_readable {
            "初始化标记存在，但当前交互用户无法读取。"
        } else {
            "初始化标记不是有效的版本化 JSON，Windows 设置可能已损坏。"
        }
        .to_string();
        result.recommended_action =
            "点击更新时会先备份关键状态，再由同一 Codex CLI 的官方 setup 重建；不要手动删除整个 .sandbox。"
                .to_string();
        return result;
    }

    if result.sandbox_users_exists
        && (!result.sandbox_users_readable || !result.sandbox_users_valid)
    {
        result.status = "sandbox_state_invalid".to_string();
        result.message = "Sandbox 用户状态无法读取或结构已损坏。".to_string();
        result.recommended_action =
            "不要查看或复制其中的凭据；点击更新后会先创建受控备份，再由官方 setup 重建。"
                .to_string();
        return result;
    }

    if !result.sandbox_log_exists {
        result.status = "setup_unverified".to_string();
        result.message =
            "Windows 执行环境已初始化，但尚未发现成功启动过文件执行环境的记录。".to_string();
        result.recommended_action =
            "打开 Codex 完成 Windows 设置并执行一次文件操作，然后返回重新检测。".to_string();
        return result;
    }

    if free_space_bytes
        .is_some_and(|bytes| bytes < crate::mirror_access::MIN_CODEX_RUNTIME_FREE_SPACE_BYTES)
    {
        result.status = "storage_low".to_string();
        result.message = "Windows 执行环境已初始化，但相关磁盘剩余空间不足。".to_string();
        result.recommended_action = "释放空间后重新检测，避免会话读取或工具执行中断。".to_string();
        return result;
    }

    result.status = "ready_hint".to_string();
    result.blocking = false;
    result.message = "Windows 执行环境的本地初始化文件完整。".to_string();
    result.recommended_action =
        "打开 Codex 后仍会由官方 readiness 检查做最终确认；若再次提示设置失败，请返回重新检测。"
            .to_string();
    result
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowsSandboxEnableResult {
    pub ready: bool,
    pub mode: Option<String>,
    pub fallback_used: bool,
    pub readiness_before: String,
    pub readiness_after: String,
    pub elevated_error: Option<String>,
    pub recovery_backup_path: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CodexCliTarget {
    executable: PathBuf,
    source: String,
    identity: ExecutableIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExecutableIdentity {
    len: u64,
    modified_at_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AppServerIdentity {
    codex_home: PathBuf,
    user_agent: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OfficialReadinessResult {
    status: Option<String>,
    identity: AppServerIdentity,
    requirements: SandboxRequirements,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SandboxRequirements {
    allowed_modes: Vec<String>,
    full_access_policy: FullAccessPolicy,
    default_permissions: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FullAccessPolicy {
    Allowed,
    Blocked(String),
}

#[derive(Debug, thiserror::Error)]
#[error("Codex App Server 不支持 {method} RPC：{message}")]
struct UnsupportedAppServerRpc {
    method: String,
    message: String,
}

pub async fn diagnose_default_with_official() -> WindowsSandboxDiagnostic {
    let mut diagnostic = diagnose_default();
    if !diagnostic.platform_supported {
        return diagnostic;
    }

    let home_resolution = crate::codex_home::resolve_codex_home();
    if let Some(issue) = home_resolution.issue {
        diagnostic.status = "codex_home_invalid".to_string();
        diagnostic.blocking = true;
        diagnostic.official_check_error = Some(redact_error(&issue));
        diagnostic.message = "管理器与真实 Codex 无法确定同一个 CODEX_HOME。".to_string();
        diagnostic.recommended_action =
            "修正 CODEX_HOME 后重新检测；不要让管理员进程与普通用户进程使用不同目录。".to_string();
        return diagnostic;
    }

    match crate::codex_app_state::validate_local_full_access_state(&home_resolution.path) {
        Ok(()) => diagnostic.full_access_configured = true,
        Err(error) => {
            diagnostic.full_access_check_error = Some(redact_error(&format!("{error:#}")));
        }
    }

    let target = match resolve_sandbox_codex_cli(None) {
        Ok(target) => target,
        Err(error) => {
            diagnostic.status = "codex_cli_unavailable".to_string();
            diagnostic.blocking = true;
            diagnostic.official_check_error = Some(redact_error(&format!("{error:#}")));
            diagnostic.message =
                "未找到即将启动的 Codex App 所携带的真实 CLI，无法可信检查 Sandbox。".to_string();
            diagnostic.recommended_action =
                "重新选择实际 Codex App 路径或修正 CODEX_CLI_PATH；不会改用 Manager runtime 代替。"
                    .to_string();
            return diagnostic;
        }
    };
    diagnostic.codex_cli_path = Some(target.executable.to_string_lossy().to_string());
    diagnostic.codex_cli_source = Some(target.source.clone());
    let preserve_local_failure = local_failure_has_priority(&diagnostic.status);

    match official_readiness_with_target(&target, &home_resolution.path).await {
        Ok(result) => {
            diagnostic.codex_cli_user_agent = result.identity.user_agent;
            diagnostic.app_server_codex_home =
                Some(result.identity.codex_home.to_string_lossy().to_string());
            if apply_policy_block(&mut diagnostic, &result.requirements) {
                return diagnostic;
            }
            let Some(status) = result.status else {
                diagnostic.status = "check_failed".to_string();
                diagnostic.blocking = true;
                diagnostic.message = "真实 Codex CLI 未返回 Windows 执行环境状态。".to_string();
                diagnostic.recommended_action = "请更新 Codex 后重新检测。".to_string();
                return diagnostic;
            };
            diagnostic.official_readiness = Some(status.clone());
            match status.as_str() {
                "ready" => {
                    if !preserve_local_failure {
                        if diagnostic.full_access_configured {
                            diagnostic.status = "ready".to_string();
                            diagnostic.blocking = false;
                            diagnostic.message =
                                "真实 Codex CLI 已确认 Windows 文件执行环境和 Full access 均可用。"
                                    .to_string();
                            diagnostic.recommended_action =
                                "可以正常启动 Codex 并创建、修改文件。".to_string();
                        } else {
                            diagnostic.status = "full_access_not_configured".to_string();
                            diagnostic.blocking = true;
                            diagnostic.message =
                                "Windows Sandbox 已就绪，但 Codex 本地权限仍不是 Full access。"
                                    .to_string();
                            diagnostic.recommended_action =
                                "点击“启用完整文件能力”；工具会先备份界面状态，再写入并回读 Full access。"
                                    .to_string();
                        }
                    }
                }
                "notConfigured" => {
                    if !preserve_local_failure {
                        diagnostic.status = "not_configured".to_string();
                        diagnostic.blocking = true;
                        diagnostic.message =
                            "真实 Codex CLI 确认 Windows 文件执行环境尚未配置。".to_string();
                        diagnostic.recommended_action =
                            "点击“启用完整文件能力”完成官方 Sandbox 初始化。".to_string();
                    }
                }
                "updateRequired" => {
                    if !preserve_local_failure {
                        diagnostic.status = "update_required".to_string();
                        diagnostic.update_action = Some("sandbox_environment".to_string());
                        diagnostic.blocking = true;
                        diagnostic.message =
                            "真实 Codex CLI 要求更新 Windows 文件执行环境。".to_string();
                        diagnostic.recommended_action =
                            "点击“更新文件执行环境”重新执行官方初始化。".to_string();
                    }
                }
                _ => {
                    if !preserve_local_failure {
                        diagnostic.status = "check_failed".to_string();
                        diagnostic.blocking = true;
                        diagnostic.message =
                            "真实 Codex CLI 返回了无法识别的 Windows 执行环境状态。".to_string();
                        diagnostic.recommended_action = "请更新 Codex 后重新检测。".to_string();
                    }
                }
            }
        }
        Err(error) => {
            apply_official_check_error(&mut diagnostic, &error, preserve_local_failure);
        }
    }
    diagnostic
}

pub async fn official_readiness() -> anyhow::Result<String> {
    crate::codex_home::validate_codex_home_environment()?;
    let home = crate::codex_home::default_codex_home_dir();
    let target = resolve_sandbox_codex_cli(None)?;
    let result = official_readiness_with_target(&target, &home).await?;
    if let FullAccessPolicy::Blocked(reason) = result.requirements.full_access_policy {
        anyhow::bail!("当前设备的组织策略禁止 Codex Full access：{reason}");
    }
    result
        .status
        .context("Codex 未返回 Windows Sandbox readiness")
}

pub fn resolve_codex_cli_for_app(app_dir: Option<&Path>) -> anyhow::Result<PathBuf> {
    resolve_sandbox_codex_cli(app_dir).map(|target| target.executable)
}

fn resolve_sandbox_codex_cli(app_dir: Option<&Path>) -> anyhow::Result<CodexCliTarget> {
    let env_override = std::env::var_os("CODEX_CLI_PATH");
    resolve_sandbox_codex_cli_with(app_dir, env_override)
}

fn resolve_sandbox_codex_cli_with(
    app_dir: Option<&Path>,
    env_override: Option<OsString>,
) -> anyhow::Result<CodexCliTarget> {
    let (executable, source) = if let Some(raw) = env_override {
        let path = PathBuf::from(raw);
        if path.as_os_str().to_string_lossy().trim().is_empty() {
            anyhow::bail!("CODEX_CLI_PATH 已设置但为空；不会静默改用其他 CLI");
        }
        if !path.is_absolute() {
            anyhow::bail!("CODEX_CLI_PATH 必须是绝对路径：{}", path.display());
        }
        if !path.is_file() {
            anyhow::bail!("CODEX_CLI_PATH 指向的文件不存在：{}", path.display());
        }
        #[cfg(windows)]
        if !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("codex.exe"))
        {
            anyhow::bail!(
                "CODEX_CLI_PATH 必须指向真实 codex.exe，不能指向 Manager runtime 或命令包装器：{}",
                path.display()
            );
        }
        (path, "CODEX_CLI_PATH".to_string())
    } else {
        let resolved_app_dir = match app_dir {
            Some(path) => crate::app_paths::resolve_codex_app_dir(Some(path)),
            None => {
                let settings = crate::settings::SettingsStore::default()
                    .load()
                    .context("无法读取 Manager 设置以确定将启动的 Codex App")?;
                crate::app_paths::resolve_codex_app_dir_with_saved(
                    None,
                    Some(settings.codex_app_path.as_str()),
                )
            }
        }
        .context("未找到可启动的 Codex App；不会使用 Manager runtime 代替")?;
        let bundled_cli = crate::app_paths::find_bundled_codex_cli(&resolved_app_dir)
            .with_context(|| {
                format!(
                    "Codex App 缺少可读取的包内 CLI（预期位于 {} 的 resources/Resources 下）",
                    resolved_app_dir.display()
                )
            })?;
        #[cfg(windows)]
        let executable = crate::mobile_relay_host::materialize_codex_cli_for_app(&resolved_app_dir)
            .context("无法创建与所选 Codex 完全一致的可执行 CLI 私有副本")?;
        #[cfg(not(windows))]
        let executable = bundled_cli.clone();
        let source = if executable == bundled_cli {
            "codex_app_bundle"
        } else {
            "codex_app_bundle_private_cache"
        };
        (executable, source.to_string())
    };
    let identity = executable_identity(&executable)?;
    Ok(CodexCliTarget {
        executable,
        source,
        identity,
    })
}

fn executable_identity(path: &Path) -> anyhow::Result<ExecutableIdentity> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("无法读取真实 Codex CLI 元数据：{}", path.display()))?;
    if !metadata.is_file() {
        anyhow::bail!("真实 Codex CLI 不是普通文件：{}", path.display());
    }
    Ok(ExecutableIdentity {
        len: metadata.len(),
        modified_at_ms: metadata.modified().ok().and_then(system_time_ms),
    })
}

fn ensure_executable_unchanged(target: &CodexCliTarget) -> anyhow::Result<()> {
    let current = executable_identity(&target.executable)?;
    if current != target.identity {
        anyhow::bail!(
            "Codex CLI 在 Sandbox 初始化期间发生变化；已停止复检，请在 Codex 更新完成后重试"
        );
    }
    Ok(())
}

async fn official_readiness_with_target(
    target: &CodexCliTarget,
    expected_home: &Path,
) -> anyhow::Result<OfficialReadinessResult> {
    ensure_executable_unchanged(target)?;
    let mut server = SandboxAppServer::start(&target.executable).await?;
    let identity = server.initialize(expected_home).await?;
    let requirements = server.requirements().await?;
    let status = if matches!(requirements.full_access_policy, FullAccessPolicy::Allowed) {
        Some(server.readiness().await?)
    } else {
        None
    };
    Ok(OfficialReadinessResult {
        status,
        identity,
        requirements,
    })
}

pub async fn ensure_full_file_access(
    cwd: Option<&Path>,
) -> anyhow::Result<WindowsSandboxEnableResult> {
    ensure_full_file_access_for_app(None, cwd).await
}

pub async fn ensure_full_file_access_for_app(
    app_dir: Option<&Path>,
    cwd: Option<&Path>,
) -> anyhow::Result<WindowsSandboxEnableResult> {
    if !cfg!(target_os = "windows") {
        anyhow::bail!("Windows Sandbox 仅适用于 Windows");
    }
    crate::codex_home::validate_codex_home_environment()?;
    let home = crate::codex_home::default_codex_home_dir();
    validate_local_state_for_setup(&home)?;
    crate::mirror_access::validate_existing_config(&home)?;
    let config_path = home.join("config.toml");
    let target = resolve_sandbox_codex_cli(app_dir)?;

    let mut server = SandboxAppServer::start(&target.executable).await?;
    server.initialize(&home).await?;
    let requirements = server.requirements().await?;
    if let FullAccessPolicy::Blocked(reason) = &requirements.full_access_policy {
        anyhow::bail!("当前设备的组织策略禁止 Codex Full access：{reason}");
    }
    let allowed_modes = requirements.allowed_modes;
    if allowed_modes.is_empty() {
        anyhow::bail!("当前管理策略不允许任何 Windows Sandbox 实现");
    }
    crate::codex_app_state::capture_app_state_snapshot(&home)
        .context("无法创建 Codex 界面状态恢复快照")?;
    let readiness_before = server.readiness().await?;
    if readiness_before == "ready" {
        crate::codex_app_state::set_preferred_agent_mode_auto(&home)?;
        return Ok(WindowsSandboxEnableResult {
            ready: true,
            mode: configured_sandbox_mode(&config_path),
            fallback_used: false,
            readiness_after: readiness_before.clone(),
            readiness_before,
            elevated_error: None,
            recovery_backup_path: None,
            message: "Windows 文件执行能力已经可用，已清除受限访问偏好。".to_string(),
        });
    }

    let recovery_backup_path = create_sandbox_setup_backup(&home)?;
    let recovery_context = setup_backup_context(recovery_backup_path.as_deref());

    let cwd = cwd
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok());
    let mut selected_mode = None;
    let mut elevated_error = None;
    if allowed_modes.iter().any(|mode| mode == "elevated") {
        match server.setup("elevated", cwd.as_deref()).await {
            Ok(()) => selected_mode = Some("elevated".to_string()),
            Err(error) => elevated_error = Some(redact_error(&error.to_string())),
        }
    }
    if selected_mode.is_none() && allowed_modes.iter().any(|mode| mode == "unelevated") {
        ensure_executable_unchanged(&target)?;
        server = SandboxAppServer::start(&target.executable).await?;
        server.initialize(&home).await?;
        server
            .setup("unelevated", cwd.as_deref())
            .await
            .with_context(|| recovery_context.clone())?;
        selected_mode = Some("unelevated".to_string());
    }
    let Some(mode) = selected_mode else {
        anyhow::bail!(
            "Windows Sandbox 初始化失败，且管理策略不允许 unelevated 回退：{}；{}",
            elevated_error.as_deref().unwrap_or("未知错误"),
            recovery_context
        );
    };

    drop(server);

    ensure_executable_unchanged(&target)?;
    let mut verifier = SandboxAppServer::start(&target.executable).await?;
    verifier.initialize(&home).await.with_context(|| {
        format!("Sandbox 已完成，但真实 Codex CLI 无法读取初始化后的配置；{recovery_context}")
    })?;
    let readiness_after = verifier.readiness().await?;
    let ready = readiness_after == "ready";
    if !ready {
        anyhow::bail!(
            "Sandbox 初始化完成，但同一真实 Codex CLI 复检状态为 {readiness_after}；{recovery_context}"
        );
    }
    let persisted_mode = configured_sandbox_mode(&config_path);
    if persisted_mode.as_deref() != Some(mode.as_str()) {
        anyhow::bail!(
            "Sandbox setupCompleted 已成功，但 config.toml 回读模式不一致（期望 {mode}，实际 {}）；{recovery_context}",
            persisted_mode.as_deref().unwrap_or("未写入"),
        );
    }
    crate::codex_app_state::set_preferred_agent_mode_auto(&home)
        .with_context(|| format!("Sandbox 已完成，但无法清除受限访问偏好；{recovery_context}"))?;
    Ok(WindowsSandboxEnableResult {
        ready,
        fallback_used: mode == "unelevated" && elevated_error.is_some(),
        mode: Some(mode.clone()),
        readiness_before,
        readiness_after,
        elevated_error,
        recovery_backup_path: recovery_backup_path.map(|path| path.to_string_lossy().to_string()),
        message: if mode == "elevated" {
            "完整文件能力已启用，当前使用官方 elevated Windows Sandbox。".to_string()
        } else {
            "完整文件能力已启用，当前使用官方 unelevated Windows Sandbox。".to_string()
        },
    })
}

fn create_sandbox_setup_backup(home: &Path) -> anyhow::Result<Option<PathBuf>> {
    const FILES: &[(&str, &str)] = &[
        ("config.toml", "config.toml"),
        (".sandbox/setup_marker.json", "sandbox/setup_marker.json"),
        (
            ".sandbox/deny_read_acl_state.json",
            "sandbox/deny_read_acl_state.json",
        ),
        (".sandbox/setup_error.json", "sandbox/setup_error.json"),
        (
            ".sandbox-secrets/sandbox_users.json",
            "sandbox-secrets/sandbox_users.json",
        ),
    ];
    let existing = FILES
        .iter()
        .filter(|(source, _)| home.join(source).is_file())
        .collect::<Vec<_>>();
    if existing.is_empty() {
        return Ok(None);
    }

    let backup_root = home.join("backups_state").join("windows-sandbox");
    fs::create_dir_all(&backup_root).with_context(|| {
        format!(
            "无法创建 Windows Sandbox 恢复备份目录：{}",
            backup_root.display()
        )
    })?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let base_name = format!("{timestamp}-{}", std::process::id());
    let mut suffix = 0_u32;
    let backup_dir = loop {
        let name = if suffix == 0 {
            base_name.clone()
        } else {
            format!("{base_name}-{suffix}")
        };
        let candidate = backup_root.join(name);
        if !candidate.exists() {
            break candidate;
        }
        suffix = suffix.saturating_add(1);
    };
    let staging = backup_root.join(format!(
        ".{}.tmp",
        backup_dir.file_name().unwrap_or_default().to_string_lossy()
    ));
    fs::create_dir(&staging).with_context(|| {
        format!(
            "无法创建 Windows Sandbox 恢复备份暂存目录：{}",
            staging.display()
        )
    })?;

    let mut copied = Vec::with_capacity(existing.len());
    for (source, destination) in existing {
        let source_path = home.join(source);
        let destination_path = staging.join(destination);
        if let Some(parent) = destination_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let source_len = fs::metadata(&source_path)?.len();
        let copied_len = fs::copy(&source_path, &destination_path).with_context(|| {
            format!(
                "备份 Windows Sandbox 关键文件失败：{}",
                source_path.display()
            )
        })?;
        if copied_len != source_len {
            anyhow::bail!(
                "Windows Sandbox 关键文件备份长度不一致：{}",
                source_path.display()
            );
        }
        copied.push((*source).to_string());
    }
    let manifest = serde_json::to_vec_pretty(&json!({
        "version": 1,
        "createdAtMs": timestamp,
        "files": copied,
    }))?;
    fs::write(staging.join("manifest.json"), manifest)
        .context("无法写入 Windows Sandbox 恢复备份清单")?;
    fs::rename(&staging, &backup_dir).with_context(|| {
        format!(
            "无法提交 Windows Sandbox 恢复备份：{}",
            backup_dir.display()
        )
    })?;
    Ok(Some(backup_dir))
}

fn setup_backup_context(backup_path: Option<&Path>) -> String {
    match backup_path {
        Some(path) => format!("恢复备份已保留在 {}", path.display()),
        None => "初始化前没有需要备份的既有 Sandbox 状态".to_string(),
    }
}

struct SandboxAppServer {
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    stderr_capture: std::sync::Arc<tokio::sync::Mutex<Vec<u8>>>,
    stderr_task: tokio::task::JoinHandle<()>,
    next_id: u64,
}

impl SandboxAppServer {
    async fn start(executable: &Path) -> anyhow::Result<Self> {
        let mut command = Command::new(executable);
        command
            .arg("app-server")
            .arg("--strict-config")
            .arg("--stdio")
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        command.creation_flags(crate::windows_create_no_window());
        let mut child = command
            .spawn()
            .with_context(|| format!("无法启动真实 Codex App Server：{}", executable.display()))?;
        let stdin = child
            .stdin
            .take()
            .context("Codex App Server stdin 不可用")?;
        let stdout = child
            .stdout
            .take()
            .context("Codex App Server stdout 不可用")?;
        let stderr = child
            .stderr
            .take()
            .context("Codex App Server stderr 不可用")?;
        let stderr_capture = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let stderr_task = tokio::spawn(capture_process_stderr(
            stderr,
            std::sync::Arc::clone(&stderr_capture),
        ));
        Ok(Self {
            child,
            stdin,
            lines: BufReader::new(stdout).lines(),
            stderr_capture,
            stderr_task,
            next_id: 1,
        })
    }

    async fn initialize(&mut self, expected_home: &Path) -> anyhow::Result<AppServerIdentity> {
        let result = self
            .request(
                "initialize",
                json!({
                    "clientInfo": {
                        "name": "mirror-x-codex-manager",
                        "title": "Mirror X Codex Manager",
                        "version": crate::version::VERSION,
                    },
                    "capabilities": null,
                }),
                Duration::from_secs(15),
            )
            .await?;
        let codex_home = result
            .get("codexHome")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .context("真实 Codex App Server initialize 响应缺少 codexHome")?;
        if !paths_refer_to_same_location(&codex_home, expected_home) {
            anyhow::bail!(
                "真实 Codex App Server 使用的 CODEX_HOME 与管理器不一致（App Server: {}，Manager: {}）",
                codex_home.display(),
                expected_home.display()
            );
        }
        self.write(json!({ "method": "initialized" }))
            .await
            .context("无法确认 Codex App Server initialized 握手")?;
        Ok(AppServerIdentity {
            codex_home,
            user_agent: result
                .get("userAgent")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }

    async fn requirements(&mut self) -> anyhow::Result<SandboxRequirements> {
        let result = self
            .request(
                "configRequirements/read",
                Value::Null,
                Duration::from_secs(15),
            )
            .await?;
        parse_requirements(&result)
    }

    async fn readiness(&mut self) -> anyhow::Result<String> {
        self.request(
            "windowsSandbox/readiness",
            Value::Null,
            Duration::from_secs(20),
        )
        .await?
        .get("status")
        .and_then(Value::as_str)
        .map(str::to_string)
        .context("Codex 未返回 Windows Sandbox readiness")
    }

    async fn setup(&mut self, mode: &str, cwd: Option<&Path>) -> anyhow::Result<()> {
        let id = self.next_request_id();
        self.write(json!({
            "id": id,
            "method": "windowsSandbox/setupStart",
            "params": {
                "mode": mode,
                "cwd": cwd.map(|path| path.to_string_lossy().to_string()),
            }
        }))
        .await?;
        let deadline = Instant::now() + Duration::from_secs(300);
        let mut started = false;
        let mut pending_completion = None;
        loop {
            let message = self.read_until(deadline).await?;
            if message.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(error) = message.get("error") {
                    return Err(rpc_error("windowsSandbox/setupStart", error));
                }
                started = message
                    .get("result")
                    .and_then(|result| result.get("started"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if !started {
                    anyhow::bail!("Windows Sandbox setup 未启动");
                }
                if let Some(params) = pending_completion.take() {
                    return setup_completion_result(mode, &params);
                }
            }
            if message.get("method").and_then(Value::as_str)
                == Some("windowsSandbox/setupCompleted")
            {
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                if params.get("mode").and_then(Value::as_str) != Some(mode) {
                    continue;
                }
                if started {
                    return setup_completion_result(mode, &params);
                }
                pending_completion = Some(params);
            }
            if !started && Instant::now() >= deadline {
                anyhow::bail!("等待 Windows Sandbox setup 启动超时");
            }
        }
    }

    async fn request(
        &mut self,
        method: &str,
        params: Value,
        wait: Duration,
    ) -> anyhow::Result<Value> {
        let id = self.next_request_id();
        self.write(json!({ "id": id, "method": method, "params": params }))
            .await?;
        let deadline = Instant::now() + wait;
        loop {
            let message = self.read_until(deadline).await?;
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                return Err(rpc_error(method, error));
            }
            return message
                .get("result")
                .cloned()
                .context("App Server 响应缺少 result");
        }
    }

    async fn write(&mut self, value: Value) -> anyhow::Result<()> {
        let result = async {
            self.stdin.write_all(value.to_string().as_bytes()).await?;
            self.stdin.write_all(b"\n").await?;
            self.stdin.flush().await
        }
        .await;
        if let Err(error) = result {
            let detail = self.process_error_detail().await;
            return Err(error).context(format!(
                "无法写入真实 Codex App Server{}",
                detail
                    .as_deref()
                    .map(|value| format!("：{value}"))
                    .unwrap_or_default()
            ));
        }
        Ok(())
    }

    async fn read_until(&mut self, deadline: Instant) -> anyhow::Result<Value> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let line = timeout(remaining, self.lines.next_line())
            .await
            .context("等待 Codex App Server 响应超时")??;
        let Some(line) = line else {
            let detail = self.process_error_detail().await;
            anyhow::bail!(
                "真实 Codex App Server 已提前退出{}",
                detail
                    .as_deref()
                    .map(|value| format!("：{value}"))
                    .unwrap_or_default()
            );
        };
        serde_json::from_str(&line).context("Codex App Server 返回了无效 JSON")
    }

    async fn process_error_detail(&mut self) -> Option<String> {
        let _ = timeout(Duration::from_secs(1), self.child.wait()).await;
        let _ = timeout(Duration::from_secs(1), &mut self.stderr_task).await;
        let stderr = self.stderr_capture.lock().await.clone();
        sanitized_process_error(&stderr, &[])
    }

    fn next_request_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

fn parse_requirements(result: &Value) -> anyhow::Result<SandboxRequirements> {
    let requirements = match result.get("requirements") {
        None | Some(Value::Null) => None,
        Some(Value::Object(requirements)) => Some(requirements),
        Some(_) => anyhow::bail!("configRequirements/read 的 requirements 格式无效"),
    };

    let allowed_modes = match requirements
        .and_then(|requirements| requirements.get("allowedWindowsSandboxImplementations"))
    {
        None | Some(Value::Null) => vec!["elevated".to_string(), "unelevated".to_string()],
        Some(Value::Array(values)) => {
            if values.iter().any(|value| !value.is_string()) {
                anyhow::bail!("allowedWindowsSandboxImplementations 格式无效");
            }
            values
                .iter()
                .filter_map(Value::as_str)
                .filter(|mode| matches!(*mode, "elevated" | "unelevated"))
                .map(str::to_string)
                .collect()
        }
        Some(_) => anyhow::bail!("allowedWindowsSandboxImplementations 格式无效"),
    };

    let default_permissions =
        match requirements.and_then(|requirements| requirements.get("defaultPermissions")) {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) => Some(value.clone()),
            Some(_) => anyhow::bail!("defaultPermissions 格式无效"),
        };

    let mut policy_reasons = Vec::new();
    match requirements.and_then(|requirements| requirements.get("allowedPermissionProfiles")) {
        None | Some(Value::Null) => {}
        Some(Value::Object(profiles)) => {
            if profiles.values().any(|value| !value.is_boolean()) {
                anyhow::bail!("allowedPermissionProfiles 格式无效");
            }
            if profiles.get(":danger-full-access").and_then(Value::as_bool) != Some(true) {
                policy_reasons
                    .push("allowedPermissionProfiles 未允许 :danger-full-access".to_string());
            }
        }
        Some(_) => anyhow::bail!("allowedPermissionProfiles 格式无效"),
    }
    match requirements.and_then(|requirements| requirements.get("allowedSandboxModes")) {
        None | Some(Value::Null) => {}
        Some(Value::Array(modes)) => {
            if modes.iter().any(|value| !value.is_string()) {
                anyhow::bail!("allowedSandboxModes 格式无效");
            }
            if !modes
                .iter()
                .filter_map(Value::as_str)
                .any(|mode| mode == "danger-full-access")
            {
                policy_reasons.push("allowedSandboxModes 未允许 danger-full-access".to_string());
            }
        }
        Some(_) => anyhow::bail!("allowedSandboxModes 格式无效"),
    }
    let full_access_policy = if policy_reasons.is_empty() {
        FullAccessPolicy::Allowed
    } else {
        FullAccessPolicy::Blocked(policy_reasons.join("；"))
    };

    Ok(SandboxRequirements {
        allowed_modes,
        full_access_policy,
        default_permissions,
    })
}

#[cfg(test)]
fn parse_allowed_modes(result: &Value) -> anyhow::Result<Vec<String>> {
    Ok(parse_requirements(result)?.allowed_modes)
}

fn setup_completion_result(mode: &str, params: &Value) -> anyhow::Result<()> {
    if params.get("mode").and_then(Value::as_str) != Some(mode) {
        anyhow::bail!("Windows Sandbox setup 返回了不匹配的模式");
    }
    if params.get("success").and_then(Value::as_bool) == Some(true) {
        return Ok(());
    }
    anyhow::bail!(
        "{}",
        params
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("Windows Sandbox setup 失败")
    );
}

impl Drop for SandboxAppServer {
    fn drop(&mut self) {
        self.stderr_task.abort();
        let _ = self.child.start_kill();
    }
}

async fn capture_process_stderr(
    mut stderr: tokio::process::ChildStderr,
    capture: std::sync::Arc<tokio::sync::Mutex<Vec<u8>>>,
) {
    let mut buffer = [0_u8; 1024];
    loop {
        let Ok(read) = stderr.read(&mut buffer).await else {
            return;
        };
        if read == 0 {
            return;
        }
        let mut captured = capture.lock().await;
        let remaining = MAX_PROCESS_ERROR_BYTES.saturating_sub(captured.len());
        if remaining > 0 {
            captured.extend_from_slice(&buffer[..read.min(remaining)]);
        }
    }
}

fn rpc_error_message(error: &Value) -> String {
    error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("Codex App Server 请求失败")
        .to_string()
}

fn rpc_error(method: &str, error: &Value) -> anyhow::Error {
    let message = rpc_error_message(error);
    if rpc_method_unavailable(error) {
        return UnsupportedAppServerRpc {
            method: method.to_string(),
            message,
        }
        .into();
    }
    anyhow::anyhow!(message)
}

fn rpc_method_unavailable(error: &Value) -> bool {
    if error.get("code").and_then(Value::as_i64) == Some(-32601) {
        return true;
    }
    let message = rpc_error_message(error).to_ascii_lowercase();
    message.contains("method not found")
        || message.contains("unknown method")
        || (message.contains("method") && message.contains("not supported"))
}

fn is_unsupported_app_server_rpc(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.downcast_ref::<UnsupportedAppServerRpc>().is_some())
}

fn apply_policy_block(
    diagnostic: &mut WindowsSandboxDiagnostic,
    requirements: &SandboxRequirements,
) -> bool {
    let FullAccessPolicy::Blocked(reason) = &requirements.full_access_policy else {
        return false;
    };
    diagnostic.status = "policy_blocked".to_string();
    diagnostic.blocking = true;
    diagnostic.full_access_configured = false;
    diagnostic.message = "当前设备的组织策略禁止 Codex Full access。".to_string();
    diagnostic.recommended_action = if let Some(default_permissions) =
        requirements.default_permissions.as_deref()
    {
        format!(
            "请联系设备管理员调整 Codex requirements.toml（当前默认权限：{default_permissions}）：{reason}"
        )
    } else {
        format!("请联系设备管理员调整 Codex requirements.toml：{reason}")
    };
    true
}

fn apply_official_check_error(
    diagnostic: &mut WindowsSandboxDiagnostic,
    error: &anyhow::Error,
    preserve_local_failure: bool,
) {
    if is_unsupported_app_server_rpc(error) {
        diagnostic.status = "update_required".to_string();
        diagnostic.update_action = Some("codex_app".to_string());
        diagnostic.blocking = true;
        diagnostic.message = "当前 Codex 版本缺少完整文件能力所需的官方接口。".to_string();
        diagnostic.recommended_action =
            "请先更新 Codex；更新失败时会打开 Microsoft Store 官方页面。".to_string();
    } else if !preserve_local_failure {
        diagnostic.status = "check_failed".to_string();
        diagnostic.blocking = true;
        diagnostic.message = "真实 Codex CLI 未能完成 Windows 执行环境检查。".to_string();
        diagnostic.recommended_action = "先按错误修复 config.toml、Codex App 路径或 CODEX_HOME，再重新检查；不会以独立 runtime 的结果冒充成功。".to_string();
    }
    diagnostic.official_check_error = Some(redact_error(&error.to_string()));
}

fn redact_error(error: &str) -> String {
    let home = crate::codex_home::default_codex_home_dir()
        .to_string_lossy()
        .to_string();
    error
        .replace(&home, "%CODEX_HOME%")
        .chars()
        .take(600)
        .collect()
}

fn sanitized_process_error(stderr: &[u8], stdout: &[u8]) -> Option<String> {
    let rendered = [stderr, stdout]
        .into_iter()
        .flat_map(|bytes| {
            String::from_utf8_lossy(bytes)
                .lines()
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            ![
                "api_key",
                "apikey",
                "authorization",
                "bearer",
                "password",
                "secret",
                "token",
            ]
            .iter()
            .any(|keyword| lower.contains(keyword))
        })
        .take(8)
        .collect::<Vec<_>>()
        .join(" | ");
    if rendered.is_empty() {
        None
    } else {
        Some(redact_error(&rendered))
    }
}

fn local_failure_has_priority(status: &str) -> bool {
    matches!(
        status,
        "config_invalid"
            | "acl_state_invalid"
            | "permission_blocked"
            | "storage_low"
            | "codex_home_invalid"
            | "sandbox_state_invalid"
    )
}

fn validate_local_state_for_setup(home: &Path) -> anyhow::Result<()> {
    let diagnostic = diagnose(home, true);
    if matches!(
        diagnostic.status.as_str(),
        "config_invalid"
            | "acl_state_invalid"
            | "permission_blocked"
            | "storage_low"
            | "codex_home_invalid"
    ) {
        anyhow::bail!("{} {}", diagnostic.message, diagnostic.recommended_action);
    }
    Ok(())
}

#[derive(Debug, Default)]
struct RawFileInspection {
    exists: bool,
    readable: bool,
    contains_nul: bool,
    bytes: Option<Vec<u8>>,
}

#[derive(Debug, Default)]
struct TomlFileInspection {
    exists: bool,
    readable: bool,
    valid: bool,
    contains_nul: bool,
    value: Option<toml::Value>,
}

#[derive(Debug, Default)]
struct VersionedJsonFileInspection {
    exists: bool,
    readable: bool,
    valid: bool,
    version: Option<u64>,
}

#[derive(Debug, Clone, Copy)]
enum VersionedJsonKind {
    SetupMarker,
    SandboxUsers,
}

#[derive(Debug, Default)]
struct JsonFileInspection {
    exists: bool,
    readable: bool,
    valid: bool,
    contains_nul: bool,
}

fn inspect_raw_file(path: &Path, max_bytes: u64) -> RawFileInspection {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return RawFileInspection::default();
        }
        Err(_) => {
            return RawFileInspection {
                exists: true,
                ..Default::default()
            };
        }
    };
    if !metadata.is_file() || metadata.len() > max_bytes {
        return RawFileInspection {
            exists: true,
            readable: metadata.is_file(),
            ..Default::default()
        };
    }
    match fs::read(path) {
        Ok(bytes) => RawFileInspection {
            exists: true,
            readable: true,
            contains_nul: bytes.contains(&0),
            bytes: Some(bytes),
        },
        Err(_) => RawFileInspection {
            exists: true,
            ..Default::default()
        },
    }
}

fn inspect_toml_file(path: &Path, max_bytes: u64) -> TomlFileInspection {
    let raw = inspect_raw_file(path, max_bytes);
    let value = raw
        .bytes
        .as_deref()
        .filter(|_| !raw.contains_nul)
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .and_then(|text| toml::from_str::<toml::Value>(text).ok());
    TomlFileInspection {
        exists: raw.exists,
        readable: raw.readable,
        valid: value.is_some(),
        contains_nul: raw.contains_nul,
        value,
    }
}

fn inspect_versioned_json_file(
    path: &Path,
    max_bytes: u64,
    kind: VersionedJsonKind,
) -> VersionedJsonFileInspection {
    let raw = inspect_raw_file(path, max_bytes);
    let value = raw
        .bytes
        .as_deref()
        .filter(|_| !raw.contains_nul)
        .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok());
    let version = value
        .as_ref()
        .and_then(Value::as_object)
        .and_then(|object| object.get("version"))
        .and_then(Value::as_u64);
    let structure_valid = value.as_ref().is_some_and(|value| match kind {
        VersionedJsonKind::SetupMarker => {
            value.get("offline_username").is_some_and(Value::is_string)
                && value.get("online_username").is_some_and(Value::is_string)
        }
        VersionedJsonKind::SandboxUsers => ["offline", "online"].into_iter().all(|key| {
            value.get(key).is_some_and(|record| {
                record.get("username").is_some_and(Value::is_string)
                    && record.get("password").is_some_and(Value::is_string)
            })
        }),
    });
    VersionedJsonFileInspection {
        exists: raw.exists,
        readable: raw.readable,
        valid: version.is_some() && structure_valid,
        version,
    }
}

fn inspect_deny_read_state_file(path: &Path, max_bytes: u64) -> JsonFileInspection {
    let raw = inspect_raw_file(path, max_bytes);
    let valid = raw
        .bytes
        .as_deref()
        .filter(|_| !raw.contains_nul)
        .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok())
        .and_then(|value| value.as_object().cloned())
        .and_then(|object| object.get("principals").cloned())
        .and_then(|principals| principals.as_object().cloned())
        .is_some_and(|principals| {
            principals.values().all(|paths| {
                paths
                    .as_array()
                    .is_some_and(|paths| paths.iter().all(Value::is_string))
            })
        });
    JsonFileInspection {
        exists: raw.exists,
        readable: raw.readable,
        valid,
        contains_nul: raw.contains_nul,
    }
}

fn paths_refer_to_same_location(left: &Path, right: &Path) -> bool {
    path_comparison_key(left) == path_comparison_key(right)
}

fn path_comparison_key(path: &Path) -> String {
    let normalized = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut rendered = normalized.to_string_lossy().replace('\\', "/");
    if let Some(without_prefix) = rendered.strip_prefix("//?/") {
        rendered = without_prefix.to_string();
    }
    while rendered.len() > 3 && rendered.ends_with('/') {
        rendered.pop();
    }
    if cfg!(windows) {
        rendered.make_ascii_lowercase();
    }
    rendered
}

fn configured_sandbox_mode(config_path: &Path) -> Option<String> {
    let inspection = inspect_toml_file(config_path, MAX_CONFIG_BYTES);
    configured_sandbox_mode_from_value(inspection.value.as_ref()?)
}

fn configured_sandbox_mode_from_value(value: &toml::Value) -> Option<String> {
    value
        .get("windows")?
        .get("sandbox")?
        .as_str()
        .filter(|mode| matches!(*mode, "elevated" | "unelevated"))
        .map(str::to_string)
}

fn nearest_existing_path(path: &Path) -> Option<&Path> {
    path.ancestors().find(|candidate| candidate.exists())
}

fn system_time_ms(value: SystemTime) -> Option<u64> {
    value
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_not_configured_without_sandbox_directory() {
        let temp = tempfile::tempdir().unwrap();
        let result = diagnose(temp.path(), true);
        assert_eq!(result.status, "not_configured");
        assert!(result.blocking);
        assert!(!result.full_access_configured);
        assert_eq!(result.full_access_check_error, None);
    }

    #[test]
    fn reports_partial_setup_when_logs_exist_without_marker() {
        let temp = tempfile::tempdir().unwrap();
        let sandbox = temp.path().join(".sandbox");
        fs::create_dir(&sandbox).unwrap();
        fs::write(sandbox.join("sandbox.2026-08-22.log"), b"ignored").unwrap();
        let result = diagnose(temp.path(), true);
        assert_eq!(result.status, "partial_setup");
        assert!(result.sandbox_log_exists);
    }

    #[test]
    fn rejects_invalid_marker_without_exposing_contents() {
        let temp = tempfile::tempdir().unwrap();
        let sandbox = temp.path().join(".sandbox");
        fs::create_dir(&sandbox).unwrap();
        fs::write(sandbox.join("setup_marker.json"), b"not-json").unwrap();
        let result = diagnose(temp.path(), true);
        assert_eq!(result.status, "sandbox_state_invalid");
        assert!(result.marker_readable);
        assert!(!result.marker_valid);
    }

    #[test]
    fn malformed_config_is_reported_before_sandbox_setup_state() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("config.toml"), b"model = [\n").unwrap();

        let result = diagnose(temp.path(), true);

        assert_eq!(result.status, "config_invalid");
        assert!(result.config_exists);
        assert!(result.config_readable);
        assert!(!result.config_valid);
        assert!(!result.message.contains("model"));
    }

    #[test]
    fn nul_filled_config_is_reported_without_parsing_contents() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("config.toml"), [0_u8; 32]).unwrap();

        let result = diagnose(temp.path(), true);

        assert_eq!(result.status, "config_invalid");
        assert!(result.config_contains_nul);
        assert!(result.message.contains("NUL"));
    }

    #[test]
    fn malformed_deny_read_state_blocks_false_ready_result() {
        let temp = tempfile::tempdir().unwrap();
        let sandbox = temp.path().join(".sandbox");
        fs::create_dir(&sandbox).unwrap();
        fs::write(sandbox.join("deny_read_acl_state.json"), b"{broken").unwrap();

        let result = diagnose(temp.path(), true);

        assert_eq!(result.status, "acl_state_invalid");
        assert!(result.deny_read_state_exists);
        assert!(result.deny_read_state_readable);
        assert!(!result.deny_read_state_valid);
    }

    #[test]
    fn deny_read_state_requires_official_principals_shape() {
        let temp = tempfile::tempdir().unwrap();
        let sandbox = temp.path().join(".sandbox");
        fs::create_dir(&sandbox).unwrap();
        fs::write(sandbox.join("deny_read_acl_state.json"), br#"{}"#).unwrap();
        let missing = diagnose(temp.path(), true);
        assert_eq!(missing.status, "acl_state_invalid");

        fs::write(
            sandbox.join("deny_read_acl_state.json"),
            br#"{"principals":{"S-1-5-21":["D:/private"]}}"#,
        )
        .unwrap();
        let valid = diagnose(temp.path(), true);
        assert!(valid.deny_read_state_valid);
        assert_eq!(valid.status, "not_configured");
    }

    #[test]
    fn valid_marker_is_ready_and_reads_explicit_mode() {
        let temp = tempfile::tempdir().unwrap();
        let sandbox = temp.path().join(".sandbox");
        fs::create_dir(&sandbox).unwrap();
        fs::write(
            sandbox.join("setup_marker.json"),
            br#"{"version":1,"offline_username":"offline","online_username":"online"}"#,
        )
        .unwrap();
        fs::write(sandbox.join("sandbox.log"), b"ignored").unwrap();
        fs::write(
            temp.path().join("config.toml"),
            "[windows]\nsandbox = \"unelevated\"\n",
        )
        .unwrap();
        let result = diagnose(temp.path(), true);
        assert_eq!(result.status, "ready_hint");
        assert!(!result.blocking);
        assert_eq!(result.configured_mode.as_deref(), Some("unelevated"));
    }

    #[test]
    fn valid_marker_without_execution_log_remains_unverified() {
        let temp = tempfile::tempdir().unwrap();
        let sandbox = temp.path().join(".sandbox");
        fs::create_dir(&sandbox).unwrap();
        fs::write(
            sandbox.join("setup_marker.json"),
            br#"{"version":1,"offline_username":"offline","online_username":"online"}"#,
        )
        .unwrap();
        let result = diagnose(temp.path(), true);
        assert_eq!(result.status, "setup_unverified");
        assert!(result.blocking);
    }

    #[test]
    fn sandbox_users_requires_both_credential_records_without_exposing_them() {
        let temp = tempfile::tempdir().unwrap();
        let sandbox = temp.path().join(".sandbox");
        fs::create_dir(&sandbox).unwrap();
        fs::create_dir(temp.path().join(".sandbox-secrets")).unwrap();
        fs::write(
            sandbox.join("setup_marker.json"),
            br#"{"version":1,"offline_username":"offline","online_username":"online"}"#,
        )
        .unwrap();
        fs::write(
            temp.path()
                .join(".sandbox-secrets")
                .join("sandbox_users.json"),
            br#"{"version":1,"offline":{"username":"hidden"}}"#,
        )
        .unwrap();

        let result = diagnose(temp.path(), true);

        assert_eq!(result.status, "sandbox_state_invalid");
        assert!(!result.sandbox_users_valid);
        assert!(!result.message.contains("hidden"));
    }

    #[test]
    fn non_windows_platform_is_not_blocking() {
        let temp = tempfile::tempdir().unwrap();
        let result = diagnose(temp.path(), false);
        assert_eq!(result.status, "unsupported_platform");
        assert!(!result.blocking);
    }

    #[test]
    fn missing_or_null_requirements_allow_both_official_modes() {
        let missing = json!({});
        let null = json!({"requirements": {"allowedWindowsSandboxImplementations": null}});
        let expected = vec!["elevated".to_string(), "unelevated".to_string()];
        assert_eq!(parse_allowed_modes(&missing).unwrap(), expected);
        assert_eq!(parse_allowed_modes(&null).unwrap(), expected);
    }

    #[test]
    fn empty_allowed_modes_blocks_setup() {
        let result = json!({"requirements": {"allowedWindowsSandboxImplementations": []}});
        assert!(parse_allowed_modes(&result).unwrap().is_empty());
    }

    #[test]
    fn elevated_only_policy_is_preserved() {
        let result = json!({"requirements": {
            "allowedWindowsSandboxImplementations": ["elevated"]
        }});
        assert_eq!(parse_allowed_modes(&result).unwrap(), vec!["elevated"]);
    }

    #[test]
    fn unelevated_only_policy_is_preserved() {
        let result = json!({"requirements": {
            "allowedWindowsSandboxImplementations": ["unelevated"]
        }});
        assert_eq!(parse_allowed_modes(&result).unwrap(), vec!["unelevated"]);
    }

    #[test]
    fn requirements_without_permission_policy_allow_full_access() {
        let requirements = parse_requirements(&json!({"requirements": {
            "defaultPermissions": ":read-only"
        }}))
        .unwrap();

        assert_eq!(requirements.full_access_policy, FullAccessPolicy::Allowed);
        assert_eq!(
            requirements.default_permissions.as_deref(),
            Some(":read-only")
        );
    }

    #[test]
    fn permission_profile_policy_must_explicitly_allow_full_access() {
        let blocked = parse_requirements(&json!({"requirements": {
            "allowedPermissionProfiles": {":read-only": true, ":danger-full-access": false}
        }}))
        .unwrap();
        assert!(matches!(
            blocked.full_access_policy,
            FullAccessPolicy::Blocked(reason) if reason.contains("allowedPermissionProfiles")
        ));

        let allowed = parse_requirements(&json!({"requirements": {
            "allowedPermissionProfiles": {":danger-full-access": true}
        }}))
        .unwrap();
        assert_eq!(allowed.full_access_policy, FullAccessPolicy::Allowed);
    }

    #[test]
    fn sandbox_mode_policy_must_allow_danger_full_access() {
        let blocked = parse_requirements(&json!({"requirements": {
            "allowedSandboxModes": ["read-only", "workspace-write"]
        }}))
        .unwrap();
        assert!(matches!(
            blocked.full_access_policy,
            FullAccessPolicy::Blocked(reason) if reason.contains("danger-full-access")
        ));

        let allowed = parse_requirements(&json!({"requirements": {
            "allowedSandboxModes": ["workspace-write", "danger-full-access"]
        }}))
        .unwrap();
        assert_eq!(allowed.full_access_policy, FullAccessPolicy::Allowed);
    }

    #[test]
    fn policy_block_cannot_be_overridden_by_local_full_access_marker() {
        let temp = tempfile::tempdir().unwrap();
        let mut diagnostic = diagnose(temp.path(), true);
        diagnostic.full_access_configured = true;
        let requirements = parse_requirements(&json!({"requirements": {
            "allowedPermissionProfiles": {":danger-full-access": false},
            "allowedSandboxModes": ["workspace-write"],
            "defaultPermissions": ":workspace"
        }}))
        .unwrap();

        assert!(apply_policy_block(&mut diagnostic, &requirements));
        assert_eq!(diagnostic.status, "policy_blocked");
        assert!(diagnostic.blocking);
        assert!(!diagnostic.full_access_configured);
        assert!(diagnostic.recommended_action.contains(":workspace"));
    }

    #[test]
    fn missing_rpc_maps_to_codex_update_required() {
        let temp = tempfile::tempdir().unwrap();
        let mut diagnostic = diagnose(temp.path(), true);
        diagnostic.status = "storage_low".to_string();
        let error = rpc_error(
            "configRequirements/read",
            &json!({"code": -32601, "message": "Method not found"}),
        );

        apply_official_check_error(&mut diagnostic, &error, true);

        assert_eq!(diagnostic.status, "update_required");
        assert_eq!(diagnostic.update_action.as_deref(), Some("codex_app"));
        assert!(diagnostic.blocking);
    }

    #[test]
    fn unsupported_method_text_is_classified_but_normal_rpc_failure_is_not() {
        assert!(rpc_method_unavailable(
            &json!({"message": "Unknown method windowsSandbox/readiness"})
        ));
        assert!(!rpc_method_unavailable(
            &json!({"code": -32000, "message": "sandbox setup is not supported by policy"})
        ));
    }

    #[test]
    fn sandbox_cli_resolution_uses_app_bundle_not_manager_runtime() {
        let temp = tempfile::tempdir().unwrap();
        let app_dir = temp.path().join("codex-app");
        fs::create_dir_all(app_dir.join("resources")).unwrap();
        fs::write(app_dir.join("Codex.exe"), b"desktop").unwrap();
        let bundled_cli = app_dir.join("resources").join("codex.exe");
        fs::write(&bundled_cli, b"actual-cli").unwrap();

        let target = resolve_sandbox_codex_cli_with(Some(&app_dir), None).unwrap();

        assert_eq!(target.executable, bundled_cli);
        assert_eq!(target.source, "codex_app_bundle");
        assert!(!target.executable.to_string_lossy().contains("mobile-host"));
    }

    #[test]
    fn explicit_codex_cli_path_is_pinned_when_valid() {
        let temp = tempfile::tempdir().unwrap();
        let cli = temp.path().join("codex.exe");
        fs::write(&cli, b"actual-cli").unwrap();

        let target =
            resolve_sandbox_codex_cli_with(None, Some(cli.clone().into_os_string())).unwrap();

        assert_eq!(target.executable, cli);
        assert_eq!(target.source, "CODEX_CLI_PATH");
    }

    #[cfg(windows)]
    #[test]
    fn explicit_manager_runtime_is_rejected_for_sandbox_setup() {
        let temp = tempfile::tempdir().unwrap();
        let runtime = temp.path().join("codex-mobile-host.exe");
        fs::write(&runtime, b"copied-cli").unwrap();

        let error = resolve_sandbox_codex_cli_with(None, Some(runtime.into_os_string()))
            .expect_err("renamed Manager runtime must not impersonate the desktop CLI");

        assert!(error.to_string().contains("真实 codex.exe"));
    }

    #[test]
    fn strict_config_error_filter_hides_credential_lines() {
        let stderr = b"unknown configuration field `future`\napi_key = super-secret\n";

        let rendered = sanitized_process_error(stderr, &[]).unwrap();

        assert!(rendered.contains("unknown configuration field"));
        assert!(!rendered.contains("super-secret"));
        assert!(!rendered.contains("api_key"));
    }

    #[test]
    fn sandbox_setup_backup_preserves_only_critical_state_bytes() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join(".sandbox-secrets")).unwrap();
        fs::create_dir_all(temp.path().join(".sandbox")).unwrap();
        fs::write(temp.path().join("config.toml"), b"[windows]\n").unwrap();
        fs::write(
            temp.path().join(".sandbox").join("setup_marker.json"),
            b"marker-bytes",
        )
        .unwrap();
        fs::write(
            temp.path()
                .join(".sandbox-secrets")
                .join("sandbox_users.json"),
            b"encrypted-user-state",
        )
        .unwrap();
        fs::write(
            temp.path().join(".sandbox").join("sandbox.log"),
            b"large-log-is-not-recovery-state",
        )
        .unwrap();

        let backup = create_sandbox_setup_backup(temp.path()).unwrap().unwrap();

        assert_eq!(
            fs::read(backup.join("config.toml")).unwrap(),
            b"[windows]\n"
        );
        assert_eq!(
            fs::read(backup.join("sandbox").join("setup_marker.json")).unwrap(),
            b"marker-bytes"
        );
        assert_eq!(
            fs::read(backup.join("sandbox-secrets").join("sandbox_users.json")).unwrap(),
            b"encrypted-user-state"
        );
        assert!(!backup.join("sandbox").join("sandbox.log").exists());
        assert!(backup.join("manifest.json").is_file());
    }
}
