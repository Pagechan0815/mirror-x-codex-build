use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::Context;
use codex_plus_core::settings::SettingsStore;
use serde_json::{Value, json};

use crate::commands::CommandResult;

const CODEX_STORE_PRODUCT_ID: &str = "9PLM9XGG6VKS";
const CODEX_STORE_URI: &str = "ms-windows-store://pdp/?ProductId=9PLM9XGG6VKS";
const CODEX_STORE_WEB_URL: &str = "https://apps.microsoft.com/detail/9PLM9XGG6VKS";
const CODEX_INSTALL_WORKING_SPACE_BYTES: u64 = 512 * 1024 * 1024;

#[tauri::command]
pub fn set_codex_app_path(app_path: String) -> CommandResult<Value> {
    match persist_codex_app_path(Path::new(app_path.trim())) {
        Ok(app_dir) => success(
            "Codex 路径已验证并保存。",
            json!({
                "codexAppPath": app_dir,
                "codexVersion": codex_plus_core::app_paths::codex_app_version(&app_dir),
            }),
        ),
        Err(error) => failure(
            &format!("Codex 路径不可用：{error}"),
            json!({ "codexAppPath": null, "codexVersion": null }),
        ),
    }
}

#[tauri::command]
pub async fn install_codex_desktop() -> CommandResult<Value> {
    tauri::async_runtime::spawn_blocking(install_codex_desktop_blocking)
        .await
        .unwrap_or_else(|error| {
            failure(
                &format!("Codex 安装任务异常结束：{error}"),
                json!({ "codexAppPath": null, "storeOpened": false }),
            )
        })
}

#[tauri::command]
pub async fn update_codex_desktop() -> CommandResult<Value> {
    tauri::async_runtime::spawn_blocking(update_codex_desktop_blocking)
        .await
        .unwrap_or_else(|error| {
            failure(
                &format!("Codex 更新任务异常结束：{error}"),
                json!({ "codexAppPath": null, "storeOpened": false }),
            )
        })
}

#[tauri::command]
pub fn open_codex_store() -> CommandResult<Value> {
    if open_store().is_ok() {
        return success(
            "已打开 Microsoft Store。安装完成后返回本工具重新检测。",
            json!({ "storeOpened": true, "downloadPageOpened": false }),
        );
    }
    match open_store_web() {
        Ok(()) => success(
            "Store 协议不可用，已打开 Microsoft 官方安装页。安装完成后返回本工具重新检测。",
            json!({
                "storeOpened": false,
                "downloadPageOpened": true,
                "officialDownloadUrl": CODEX_STORE_WEB_URL,
            }),
        ),
        Err(error) => failure(
            &format!("无法打开 Microsoft Store 或官方安装页：{error}"),
            json!({
                "storeOpened": false,
                "downloadPageOpened": false,
                "officialDownloadUrl": CODEX_STORE_WEB_URL,
            }),
        ),
    }
}

fn install_codex_desktop_blocking() -> CommandResult<Value> {
    if let Some(app_dir) = discover_codex_app_dir() {
        return match persist_codex_app_path(&app_dir) {
            Ok(app_dir) => success(
                "已检测到 Codex，无需重复安装。",
                json!({
                    "codexAppPath": app_dir,
                    "codexVersion": codex_plus_core::app_paths::codex_app_version(&app_dir),
                    "storeOpened": false,
                }),
            ),
            Err(error) => failure(
                &format!("已检测到 Codex，但无法保存应用路径：{error}。未修改原 settings.json。"),
                json!({ "codexAppPath": app_dir, "storeOpened": false }),
            ),
        };
    }

    #[cfg(windows)]
    {
        if let Err(error) = ensure_codex_install_headroom() {
            return failure(
                &format!(
                    "Codex 安装前磁盘检查未通过：{error} 未启动 winget 或 Microsoft Store，请先释放空间后重试。"
                ),
                json!({
                    "codexAppPath": null,
                    "storeOpened": false,
                    "installationBlocked": true,
                }),
            );
        }
        let mut command = Command::new("winget.exe");
        command.args([
            "install",
            "--id",
            CODEX_STORE_PRODUCT_ID,
            "--source",
            "msstore",
            "--exact",
            "--accept-package-agreements",
            "--accept-source-agreements",
            "--disable-interactivity",
        ]);
        use std::os::windows::process::CommandExt;
        command.creation_flags(codex_plus_core::windows_create_no_window());
        match command.status() {
            Ok(status) if status.success() => {
                if let Some(app_dir) = wait_for_codex_discovery(Duration::from_secs(90)) {
                    return match persist_codex_app_path(&app_dir) {
                        Ok(app_dir) => success(
                            "Codex 已安装并通过可执行文件检查。",
                            json!({
                                "codexAppPath": app_dir,
                                "codexVersion": codex_plus_core::app_paths::codex_app_version(&app_dir),
                                "storeOpened": false,
                            }),
                        ),
                        Err(error) => failure(
                            &format!("Codex 已安装，但保存应用路径失败：{error}"),
                            json!({ "codexAppPath": app_dir, "storeOpened": false }),
                        ),
                    };
                }
                return registration_pending_result();
            }
            Ok(_) | Err(_) => {
                let store_opened = open_store().is_ok();
                let download_page_opened = !store_opened && open_store_web().is_ok();
                return CommandResult {
                    status: "degraded".to_string(),
                    message: if store_opened {
                        "自动安装未完成，已改为打开 Microsoft Store。安装完成后返回本工具重新检测。"
                            .to_string()
                    } else if download_page_opened {
                        "自动安装和 Store 协议未完成，已打开 Microsoft 官方安装页。安装完成后返回本工具重新检测。"
                            .to_string()
                    } else {
                        "自动安装未完成，Microsoft Store 与官方安装页也无法打开。请检查网络策略后重试，或手动安装后选择 ChatGPT.exe / Codex.exe。"
                            .to_string()
                    },
                    payload: json!({
                        "codexAppPath": null,
                        "storeOpened": store_opened,
                        "downloadPageOpened": download_page_opened,
                        "officialDownloadUrl": CODEX_STORE_WEB_URL,
                    }),
                };
            }
        }
    }

    #[cfg(not(windows))]
    failure(
        "当前平台不支持 Microsoft Store 安装，请先安装 Codex 后手动选择应用路径。",
        json!({ "codexAppPath": null, "storeOpened": false }),
    )
}

fn update_codex_desktop_blocking() -> CommandResult<Value> {
    if !codex_plus_core::watcher::find_codex_processes().is_empty() {
        return failure(
            "请先在 Codex 中停止任务并完全退出；为保护会话和程序文件，本工具不会在 Codex 运行时更新。",
            json!({ "codexAppPath": null, "storeOpened": false, "codexRunning": true }),
        );
    }
    let Some(app_dir_before) = discover_codex_app_dir() else {
        return failure(
            "未检测到可更新的 Codex。请先使用“自动安装 Codex”或手动选择 ChatGPT.exe / Codex.exe。",
            json!({ "codexAppPath": null, "storeOpened": false, "codexInstalled": false }),
        );
    };
    let version_before = codex_plus_core::app_paths::codex_app_version(&app_dir_before);

    #[cfg(windows)]
    {
        if let Err(error) = ensure_codex_install_headroom() {
            return failure(
                &format!(
                    "Codex 更新前磁盘检查未通过：{error} 未启动 winget 或 Microsoft Store，请先释放空间后重试。"
                ),
                json!({
                    "codexAppPath": app_dir_before,
                    "codexVersionBefore": version_before,
                    "storeOpened": false,
                    "updateBlocked": true,
                }),
            );
        }
        let mut command = Command::new("winget.exe");
        command.args(winget_update_args());
        use std::os::windows::process::CommandExt;
        command.creation_flags(codex_plus_core::windows_create_no_window());
        match command.status() {
            Ok(status) if status.success() => {
                let latest = wait_for_codex_update_registration(
                    &app_dir_before,
                    version_before.as_deref(),
                    Duration::from_secs(30),
                );
                let app_dir = latest
                    .as_ref()
                    .map(|(path, _)| path.clone())
                    .unwrap_or_else(|| app_dir_before.clone());
                let registration_changed = latest
                    .as_ref()
                    .map(|(_, changed)| *changed)
                    .unwrap_or(false);
                return match persist_codex_app_path(&app_dir) {
                    Ok(app_dir) if registration_changed => success(
                        "Codex 已通过 winget 更新并重新检测到新版程序。请返回完整文件能力检查继续处理。",
                        json!({
                            "codexAppPath": app_dir,
                            "codexVersionBefore": version_before,
                            "codexVersion": codex_plus_core::app_paths::codex_app_version(&app_dir),
                            "storeOpened": false,
                            "registrationPending": false,
                        }),
                    ),
                    Ok(app_dir) => CommandResult {
                        status: "degraded".to_string(),
                        message: "winget 已完成更新，但 Windows 尚未暴露新的 Codex 包版本。请等待系统完成注册后点击“重新检测”；不会重复修改 Codex 配置。".to_string(),
                        payload: json!({
                            "codexAppPath": app_dir,
                            "codexVersionBefore": version_before,
                            "codexVersion": codex_plus_core::app_paths::codex_app_version(&app_dir),
                            "storeOpened": false,
                            "registrationPending": true,
                            "retryable": true,
                        }),
                    },
                    Err(error) => failure(
                        &format!("Codex 已更新，但无法安全保存新版应用路径：{error}"),
                        json!({
                            "codexAppPath": app_dir,
                            "codexVersionBefore": version_before,
                            "storeOpened": false,
                        }),
                    ),
                };
            }
            Ok(_) | Err(_) => return update_store_fallback_result(&app_dir_before, version_before),
        }
    }

    #[cfg(not(windows))]
    failure(
        "当前平台不支持 Microsoft Store 更新，请使用系统官方更新方式。",
        json!({
            "codexAppPath": app_dir_before,
            "codexVersionBefore": version_before,
            "storeOpened": false,
        }),
    )
}

fn winget_update_args() -> [&'static str; 9] {
    [
        "upgrade",
        "--id",
        CODEX_STORE_PRODUCT_ID,
        "--source",
        "msstore",
        "--exact",
        "--accept-package-agreements",
        "--accept-source-agreements",
        "--disable-interactivity",
    ]
}

fn wait_for_codex_update_registration(
    previous_app_dir: &Path,
    previous_version: Option<&str>,
    timeout: Duration,
) -> Option<(PathBuf, bool)> {
    let deadline = Instant::now() + timeout;
    loop {
        let latest = codex_plus_core::app_paths::resolve_codex_app_dir(None)
            .filter(|path| is_complete_codex_app(path));
        if let Some(app_dir) = latest {
            let version = codex_plus_core::app_paths::codex_app_version(&app_dir);
            let changed = app_dir != previous_app_dir
                || version
                    .as_deref()
                    .zip(previous_version)
                    .is_some_and(|(current, previous)| current != previous);
            if changed || Instant::now() >= deadline {
                return Some((app_dir, changed));
            }
        } else if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn update_store_fallback_result(
    app_dir: &Path,
    version_before: Option<String>,
) -> CommandResult<Value> {
    let store_opened = open_store().is_ok();
    let download_page_opened = !store_opened && open_store_web().is_ok();
    CommandResult {
        status: "degraded".to_string(),
        message: if store_opened {
            "winget 未完成更新，已打开 Microsoft Store 的 Codex 官方页面。完成更新后返回本工具重新检测。".to_string()
        } else if download_page_opened {
            "winget 和 Store 协议均未完成更新，已打开 Microsoft 官方安装页。完成更新后返回本工具重新检测。".to_string()
        } else {
            format!(
                "无法通过 winget、Microsoft Store 协议或官方网页启动更新。请检查 Windows Package Manager、网络和组织策略，或手动打开 {CODEX_STORE_WEB_URL}。"
            )
        },
        payload: json!({
            "codexAppPath": app_dir,
            "codexVersionBefore": version_before,
            "storeOpened": store_opened,
            "downloadPageOpened": download_page_opened,
            "officialDownloadUrl": CODEX_STORE_WEB_URL,
            "retryable": true,
        }),
    }
}

fn ensure_codex_install_headroom() -> anyhow::Result<()> {
    let home = codex_plus_core::codex_home::resolve_codex_home();
    if let Some(issue) = home.issue {
        anyhow::bail!(issue);
    }
    let sqlite_home = codex_plus_core::codex_sqlite::resolve_codex_sqlite_home();
    if let Some(issue) = sqlite_home.issue {
        anyhow::bail!(issue);
    }
    let state_dir = codex_plus_core::paths::default_app_state_dir();
    let mut paths = codex_plus_core::mirror_access::codex_runtime_storage_paths(
        &home.path,
        &state_dir,
        sqlite_home.path.as_deref(),
        None,
    );
    paths.extend(
        ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"]
            .into_iter()
            .filter_map(std::env::var_os)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from),
    );
    ensure_install_storage_headroom_for_paths(paths)
}

fn ensure_install_storage_headroom_for_paths(
    paths: impl IntoIterator<Item = PathBuf>,
) -> anyhow::Result<()> {
    for path in codex_plus_core::mirror_access::storage_paths_by_volume(paths) {
        codex_plus_core::mirror_access::ensure_storage_headroom(
            &path,
            CODEX_INSTALL_WORKING_SPACE_BYTES,
            codex_plus_core::mirror_access::MIN_CODEX_RUNTIME_FREE_SPACE_BYTES,
        )?;
    }
    Ok(())
}

fn persist_codex_app_path(path: &Path) -> anyhow::Result<PathBuf> {
    let app_dir = codex_plus_core::app_paths::normalize_codex_app_path(path).ok_or_else(|| {
        anyhow::anyhow!("请选择 ChatGPT.exe、Codex.exe 或包含它们的 Codex 应用目录")
    })?;
    if !codex_plus_core::app_paths::is_codex_app_launchable(&app_dir) {
        let executable = codex_plus_core::app_paths::build_codex_executable(&app_dir);
        anyhow::bail!(
            "未找到可启动的 Codex 应用（检查路径 {}）",
            executable.display()
        );
    }
    #[cfg(windows)]
    codex_plus_core::windows_sandbox::resolve_codex_cli_for_app(Some(&app_dir)).with_context(
        || {
            format!(
                "已找到桌面程序，但缺少可用于严格配置与 Sandbox 初始化的真实 Codex CLI：{}",
                app_dir.display()
            )
        },
    )?;
    let store = SettingsStore::default();
    let settings_snapshot = match std::fs::read(store.path()) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            anyhow::bail!("读取现有 settings.json 原始快照失败：{error}；未保存 Codex 路径")
        }
    };
    store.load().map_err(|error| {
        anyhow::anyhow!(
            "读取现有设置失败：{error:#}；已停止保存 Codex 路径，原 settings.json 保持不变"
        )
    })?;
    store.update(json!({
        "codexAppPath": app_dir.to_string_lossy().to_string(),
    }))?;
    let verification = store.load().and_then(|saved| {
        if Path::new(&saved.codex_app_path) == app_dir {
            Ok(())
        } else {
            anyhow::bail!("路径写入后回读不一致")
        }
    });
    if let Err(error) = verification {
        return match restore_settings_snapshot(store.path(), settings_snapshot.as_deref()) {
            Ok(()) => Err(anyhow::anyhow!(
                "{error:#}；已恢复保存前的 settings.json，未使用该路径启动 Codex"
            )),
            Err(rollback_error) => Err(anyhow::anyhow!(
                "{error:#}，且恢复保存前 settings.json 失败：{rollback_error:#}；已停止启动，请先修复 Manager 设置"
            )),
        };
    }
    Ok(app_dir)
}

fn restore_settings_snapshot(path: &Path, snapshot: Option<&[u8]>) -> anyhow::Result<()> {
    if let Some(bytes) = snapshot {
        return codex_plus_core::settings::atomic_write(path, bytes);
    }
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn discover_codex_app_dir() -> Option<PathBuf> {
    let settings = SettingsStore::default().load().ok();
    codex_plus_core::app_paths::resolve_codex_app_dir_with_saved(
        None,
        settings
            .as_ref()
            .map(|settings| settings.codex_app_path.as_str()),
    )
    .filter(|path| is_complete_codex_app(path))
}

fn is_complete_codex_app(app_dir: &Path) -> bool {
    codex_plus_core::app_paths::is_codex_app_launchable(app_dir)
        && (!cfg!(windows) || codex_plus_core::app_paths::find_bundled_codex_cli(app_dir).is_some())
}

fn wait_for_codex_discovery(timeout: Duration) -> Option<PathBuf> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(path) = discover_codex_app_dir() {
            return Some(path);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn open_store() -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        codex_plus_core::windows_open_url(CODEX_STORE_URI)
    }
    #[cfg(not(windows))]
    {
        anyhow::bail!("Microsoft Store 仅适用于 Windows")
    }
}

fn open_store_web() -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        codex_plus_core::windows_open_url(CODEX_STORE_WEB_URL)
    }
    #[cfg(not(windows))]
    {
        anyhow::bail!("Codex 官方 Windows 安装页仅适用于 Windows")
    }
}

fn success(message: &str, payload: Value) -> CommandResult<Value> {
    CommandResult {
        status: "ok".to_string(),
        message: message.to_string(),
        payload,
    }
}

fn failure(message: &str, payload: Value) -> CommandResult<Value> {
    CommandResult {
        status: "failed".to_string(),
        message: message.to_string(),
        payload,
    }
}

fn registration_pending_result() -> CommandResult<Value> {
    CommandResult {
        status: "degraded".to_string(),
        message: "winget 已完成，但 90 秒内 Codex 的 AppX 注册仍在等待系统完成。可直接点击“重新检测”（会实时查询注册状态，不会重复安装），或手动选择 ChatGPT.exe / Codex.exe。".to_string(),
        payload: json!({
            "codexAppPath": null,
            "storeOpened": false,
            "registrationPending": true,
            "retryable": true,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_codex_app(app_dir: &Path, executable_name: &str) {
        std::fs::create_dir_all(app_dir.join("resources")).unwrap();
        std::fs::write(app_dir.join(executable_name), b"test").unwrap();
        std::fs::write(app_dir.join("resources").join("codex.exe"), b"test-cli").unwrap();
    }

    #[test]
    fn manual_path_must_resolve_to_real_codex_executable() {
        let _lock = crate::settings_path_test_lock().lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let settings_path = temp.path().join("settings.json");
        let previous = codex_plus_core::paths::set_settings_path_for_tests(Some(settings_path));
        let app_dir = temp.path().join("portable-codex");
        create_test_codex_app(&app_dir, "Codex.exe");

        let saved = persist_codex_app_path(&app_dir.join("Codex.exe")).unwrap();
        assert_eq!(saved, app_dir);
        assert_eq!(
            SettingsStore::default().load().unwrap().codex_app_path,
            app_dir.to_string_lossy()
        );
        codex_plus_core::paths::set_settings_path_for_tests(previous);
    }

    #[test]
    fn manual_path_accepts_current_chatgpt_executable_name() {
        let _lock = crate::settings_path_test_lock().lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let settings_path = temp.path().join("settings.json");
        let previous = codex_plus_core::paths::set_settings_path_for_tests(Some(settings_path));
        let app_dir = temp.path().join("current-codex");
        create_test_codex_app(&app_dir, "ChatGPT.exe");

        let saved = persist_codex_app_path(&app_dir.join("ChatGPT.exe")).unwrap();
        assert_eq!(saved, app_dir);
        codex_plus_core::paths::set_settings_path_for_tests(previous);
    }

    #[test]
    fn manual_path_rejects_arbitrary_file() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("not-codex.exe");
        std::fs::write(&file, b"test").unwrap();
        assert!(persist_codex_app_path(&file).is_err());
    }

    #[test]
    fn installation_discovery_requires_the_desktop_and_bundled_cli_on_windows() {
        let temp = tempfile::tempdir().unwrap();
        let app_dir = temp.path().join("codex");
        create_test_codex_app(&app_dir, "Codex.exe");
        assert!(is_complete_codex_app(&app_dir));

        std::fs::remove_file(app_dir.join("resources").join("codex.exe")).unwrap();
        if cfg!(windows) {
            assert!(!is_complete_codex_app(&app_dir));
        } else {
            assert!(is_complete_codex_app(&app_dir));
        }
    }

    #[test]
    fn manual_path_does_not_replace_corrupt_settings() {
        let _lock = crate::settings_path_test_lock().lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let settings_path = temp.path().join("settings.json");
        let corrupt = b"{not valid settings";
        std::fs::write(&settings_path, corrupt).unwrap();
        let previous =
            codex_plus_core::paths::set_settings_path_for_tests(Some(settings_path.clone()));
        let app_dir = temp.path().join("portable-codex");
        create_test_codex_app(&app_dir, "Codex.exe");

        let error = persist_codex_app_path(&app_dir).expect_err("corrupt settings must block save");
        codex_plus_core::paths::set_settings_path_for_tests(previous);

        assert!(error.to_string().contains("读取现有设置失败"));
        assert_eq!(std::fs::read(&settings_path).unwrap(), corrupt);
    }

    #[test]
    fn manual_path_update_preserves_unknown_future_settings_fields() {
        let _lock = crate::settings_path_test_lock().lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let settings_path = temp.path().join("settings.json");
        let mut initial =
            serde_json::to_value(codex_plus_core::settings::BackendSettings::default()).unwrap();
        initial["futureField"] = json!({ "nested": true });
        std::fs::write(&settings_path, serde_json::to_vec_pretty(&initial).unwrap()).unwrap();
        let previous =
            codex_plus_core::paths::set_settings_path_for_tests(Some(settings_path.clone()));
        let app_dir = temp.path().join("portable-codex");
        create_test_codex_app(&app_dir, "ChatGPT.exe");

        persist_codex_app_path(&app_dir).unwrap();
        let saved: Value = serde_json::from_slice(&std::fs::read(&settings_path).unwrap()).unwrap();
        codex_plus_core::paths::set_settings_path_for_tests(previous);

        assert_eq!(saved["futureField"], json!({ "nested": true }));
        assert_eq!(saved["codexAppPath"], app_dir.to_string_lossy().as_ref());
    }

    #[cfg(windows)]
    #[test]
    fn manual_path_rejects_desktop_without_real_bundled_cli() {
        let temp = tempfile::tempdir().unwrap();
        let app_dir = temp.path().join("incomplete-codex");
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::write(app_dir.join("Codex.exe"), b"test").unwrap();

        let error = persist_codex_app_path(&app_dir).unwrap_err();

        assert!(error.to_string().contains("真实 Codex CLI"));
    }

    #[test]
    fn settings_snapshot_restore_recovers_exact_bytes_or_absence() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("settings.json");
        std::fs::write(&path, b"changed").unwrap();

        restore_settings_snapshot(&path, Some(b"original bytes\r\n")).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"original bytes\r\n");
        restore_settings_snapshot(&path, None).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn completed_install_with_delayed_registration_is_retryable_degraded_state() {
        let result = registration_pending_result();

        assert_eq!(result.status, "degraded");
        assert_eq!(result.payload["registrationPending"], json!(true));
        assert_eq!(result.payload["retryable"], json!(true));
        assert_eq!(result.payload["storeOpened"], json!(false));
        assert!(result.message.contains("不会重复安装"));
    }

    #[test]
    fn install_headroom_check_uses_working_space_and_runtime_reserve() {
        let temp = tempfile::tempdir().unwrap();
        let error = codex_plus_core::mirror_access::ensure_storage_headroom(
            temp.path(),
            CODEX_INSTALL_WORKING_SPACE_BYTES,
            u64::MAX,
        )
        .unwrap_err();

        assert!(error.to_string().contains("剩余空间不足"));
        assert!(error.to_string().contains("未修改 Codex 配置"));
    }

    #[test]
    fn codex_update_uses_the_exact_official_store_package() {
        assert_eq!(
            winget_update_args(),
            [
                "upgrade",
                "--id",
                CODEX_STORE_PRODUCT_ID,
                "--source",
                "msstore",
                "--exact",
                "--accept-package-agreements",
                "--accept-source-agreements",
                "--disable-interactivity",
            ]
        );
        assert_eq!(CODEX_STORE_PRODUCT_ID, "9PLM9XGG6VKS");
    }
}
