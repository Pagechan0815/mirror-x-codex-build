#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    if std::env::args().any(|arg| arg == "--restore-before-uninstall") {
        let result = restore_before_uninstall();
        let (event, payload, exit_code) = match result {
            Ok(()) => (
                "manager.uninstall_restore_completed",
                serde_json::json!({ "status": "completed" }),
                0,
            ),
            Err(error) => (
                "manager.uninstall_restore_failed",
                serde_json::json!({ "status": "failed", "error": error.to_string() }),
                2,
            ),
        };
        let _ = codex_plus_core::diagnostic_log::append_diagnostic_log(event, payload);
        std::process::exit(exit_code);
    }

    for arg in std::env::args() {
        if arg.starts_with("mirrorplus://") || arg.starts_with("codexplusplus://") {
            match codex_plus_core::provider_import::save_pending_provider_import_from_url(&arg) {
                Ok(request) => {
                    let _ = codex_plus_core::diagnostic_log::append_diagnostic_log(
                        "manager.provider_import_url.pending",
                        serde_json::json!({
                            "name": request.name,
                            "baseUrl": request.base_url
                        }),
                    );
                    focus_existing_manager_window();
                }
                Err(error) => {
                    let _ = codex_plus_core::diagnostic_log::append_diagnostic_log(
                        "manager.provider_import_url.failed",
                        serde_json::json!({
                            "error": error.to_string()
                        }),
                    );
                }
            }
        }
    }
    if std::env::args().any(|arg| arg == "--show-update") {
        unsafe {
            std::env::set_var("CODEX_PLUS_SHOW_UPDATE", "1");
        }
    }
    codex_plus_manager_lib::run();
}

fn restore_before_uninstall() -> anyhow::Result<()> {
    use anyhow::{Context, bail};

    codex_plus_core::codex_home::validate_codex_home_environment()
        .context("CODEX_HOME 无法安全访问")?;
    codex_plus_core::codex_sqlite::validate_codex_sqlite_home_environment()
        .context("CODEX_SQLITE_HOME 无法安全访问")?;

    let codex_processes = codex_plus_core::watcher::find_codex_processes();
    if !codex_processes.is_empty() {
        bail!(
            "Codex 仍在运行（{} 个进程），未修改配置；请完全退出 Codex 后重试卸载",
            codex_processes.len()
        );
    }

    let home = codex_plus_core::relay_config::default_codex_home_dir();
    let state_dir = codex_plus_core::paths::default_app_state_dir();
    let settings_path = codex_plus_core::paths::default_settings_path();
    codex_plus_core::codex_app_state::capture_app_state_snapshot(&home)
        .context("无法创建 Codex 界面状态恢复快照，卸载前未修改任何 Codex 文件")?;
    let access = codex_plus_core::mirror_access::try_access_status(&home, &state_dir)
        .context("无法读取接管状态，卸载前未修改任何 Codex 文件")?;

    let should_restore_access = access.baseline_exists
        && (access.active
            || access.phase == "restore_failed"
            || access.session_sync_status != "synced");
    let original_provider = if should_restore_access {
        Some(
            codex_plus_core::mirror_access::restore_access(&home, &state_dir, &settings_path)
                .context("无法恢复接入前的 Codex 配置")?
                .original_provider,
        )
    } else {
        None
    };

    codex_plus_core::imagegen_skill::restore_baseline(&home, &state_dir)
        .context("无法恢复接入前的生图 Skill 或 Image Key")?;

    if let Some(original_provider) = original_provider {
        let sync = codex_plus_data::provider_sync::run_provider_sync_with_target(
            Some(&home),
            Some(&original_provider),
        );
        let synced = matches!(sync.status, codex_plus_data::ProviderSyncStatus::Synced);
        let message = if synced {
            format!(
                "卸载前恢复完成：会话 Provider 已恢复为 {}。",
                original_provider
            )
        } else {
            format!("卸载前会话恢复未完成：{}", sync.message)
        };
        codex_plus_core::mirror_access::record_session_sync(&home, &state_dir, synced, &message)
            .context("无法记录卸载前恢复状态")?;
        if !synced {
            bail!("{message}");
        }
        codex_plus_core::codex_app_state::sync_app_state_after_provider_switch_nonfatal(
            &home,
            "manager.uninstall_restore.after",
        );
    }

    Ok(())
}

fn focus_existing_manager_window() {
    codex_plus_manager_lib::focus_existing_manager_window();
}
