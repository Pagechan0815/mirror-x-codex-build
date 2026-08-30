#[cfg(windows)]
#[test]
fn manager_binary_uses_windows_gui_subsystem_in_debug_and_release() {
    let main_rs = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/main.rs"))
        .expect("read manager main.rs");

    assert!(
        main_rs.contains("#![cfg_attr(windows, windows_subsystem = \"windows\")]"),
        "manager binary should not allocate a console window on Windows"
    );
}

#[test]
fn manager_release_binary_uses_embedded_frontend_assets() {
    let cargo_toml = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .expect("read manager Cargo.toml");

    assert!(
        cargo_toml.contains("custom-protocol"),
        "release manager binary should use Tauri custom protocol instead of devUrl localhost"
    );
}

#[test]
fn manager_uses_single_instance_guard_before_starting_tauri() {
    let lib_rs = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"))
        .expect("read manager lib.rs");

    assert!(lib_rs.contains("acquire_single_instance_guard()"));
    assert!(lib_rs.contains("manager_guard_port"));
    assert!(lib_rs.contains("manager.already_running"));
    assert!(lib_rs.contains("focus_existing_manager_window();"));
    assert!(lib_rs.contains("windows_activate_process_window"));
}

#[test]
fn manager_main_window_uses_default_window_icon_explicitly() {
    let lib_rs = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"))
        .expect("read manager lib.rs");

    assert!(lib_rs.contains("main_window_builder"));
    assert!(lib_rs.contains("app.default_window_icon().cloned()"));
    assert!(lib_rs.contains("main_window_builder = main_window_builder.icon(icon)?"));
}

#[test]
fn manager_close_minimizes_to_tray_without_confirmation() {
    let lib_rs = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"))
        .expect("read manager lib.rs");
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let app_tsx = manifest_dir.parent().unwrap().join("src/App.tsx");
    let app_tsx = std::fs::read_to_string(&app_tsx).expect("read manager App.tsx");

    assert!(!lib_rs.contains("MessageDialogButtons"));
    assert!(!lib_rs.contains(".dialog()"));
    assert!(!lib_rs.contains("manager://close-requested"));
    assert!(lib_rs.contains("let _ = close_event_window.hide();"));
    assert!(!app_tsx.contains("CloseConfirmDialog"));
    assert!(app_tsx.contains("manager_exit_app"));
    assert!(app_tsx.contains("manager_hide_to_tray"));
}

#[test]
fn manager_queues_mirrorplus_provider_urls_for_confirmation_on_startup() {
    let main_rs = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/main.rs"))
        .expect("read manager main.rs");

    assert!(main_rs.contains("mirrorplus://"));
    assert!(main_rs.contains("codexplusplus://"));
    assert!(main_rs.contains("provider_import::save_pending_provider_import_from_url"));
    assert!(!main_rs.contains("provider_import::import_provider_from_url"));
    assert!(main_rs.contains("manager.provider_import_url.pending"));
}

#[test]
fn launcher_binary_embeds_codex_icon_resource() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let launcher_build = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .unwrap()
        .join("codex-plus-launcher/build.rs");
    let build_rs = std::fs::read_to_string(&launcher_build).expect("read launcher build.rs");

    assert!(build_rs.contains("WindowsResource"));
    assert!(build_rs.contains("icons/icon.ico"));
}

#[test]
fn windows_binaries_and_installer_run_as_current_user() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let manager_build =
        std::fs::read_to_string(manifest_dir.join("build.rs")).expect("read manager build.rs");
    let windows_manifest = std::fs::read_to_string(manifest_dir.join("windows-app-manifest.xml"))
        .expect("read windows app manifest");
    let launcher_build = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .unwrap()
        .join("codex-plus-launcher/build.rs");
    let launcher_build = std::fs::read_to_string(&launcher_build).expect("read launcher build.rs");
    let windows_installer = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .and_then(std::path::Path::parent)
        .unwrap()
        .join("scripts/installer/windows/MirrorXCodex.nsi");
    let windows_installer =
        std::fs::read_to_string(&windows_installer).expect("read windows installer");

    assert!(manager_build.contains("windows-app-manifest.xml"));
    assert!(launcher_build.contains("windows-app-manifest.xml"));
    assert!(windows_manifest.contains("asInvoker"));
    assert!(windows_manifest.contains("Microsoft.Windows.Common-Controls"));
    assert!(windows_installer.contains("RequestExecutionLevel user"));
    assert!(windows_installer.contains(
        "CreateShortcut \"$DESKTOP\\mirror x codex.lnk\" \"$INSTDIR\\mirror-x-codex.exe\""
    ));
    assert!(windows_installer.contains(
        "CreateShortcut \"$DESKTOP\\mirror x codex 管理器.lnk\" \"$INSTDIR\\mirror-x-codex-manager.exe\""
    ));
}

#[test]
fn windows_installer_stages_and_rolls_back_executables_before_overwrite() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let installer_path = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .and_then(std::path::Path::parent)
        .unwrap()
        .join("scripts/installer/windows/MirrorXCodex.nsi");
    let installer = std::fs::read_to_string(&installer_path).expect("read windows installer");
    let install = installer
        .split_once("Section \"Install\"")
        .expect("installer should have install section")
        .1
        .split_once("Section \"Uninstall\"")
        .expect("installer should have uninstall section")
        .0;

    let stage = install
        .find(r#"File /oname=mirror-x-codex.exe"#)
        .expect("new binaries should be staged first");
    let backup = install
        .find(r#"CopyFiles /SILENT "$INSTDIR\mirror-x-codex.exe" "$BackupDir""#)
        .expect("old binary should be backed up");
    let remove_old = install
        .find(r#"Delete "$INSTDIR\mirror-x-codex.exe""#)
        .expect("old binary should only be removed after backup completes");
    let activate = install
        .find(r#"Rename "$StageDir\mirror-x-codex.exe" "$INSTDIR\mirror-x-codex.exe""#)
        .expect("staged binary should be activated");

    assert!(stage < backup && backup < remove_old && remove_old < activate);
    assert!(install.contains(r#"$BackupDir\transaction.backing-up"#));
    assert!(install.contains(r#"$BackupDir\transaction.pending"#));
    assert!(install.contains(
        r#"Rename "$BackupDir\transaction.backing-up" "$BackupDir\transaction.pending""#
    ));
    assert!(install.contains("Call RollbackUpgrade"));
    assert!(install.contains("磁盘空间不足"));
    assert!(!installer.contains("RMDir /r"));

    let rollback = installer
        .split_once("Function RollbackUpgrade")
        .expect("installer should define rollback")
        .1
        .split_once("FunctionEnd")
        .expect("rollback function should end")
        .0;
    assert!(rollback.contains(r#"CopyFiles /SILENT "$BackupDir\mirror-x-codex.exe" "$INSTDIR""#));
    assert!(
        !rollback
            .contains(r#"Rename "$BackupDir\mirror-x-codex.exe" "$INSTDIR\mirror-x-codex.exe""#)
    );
}

#[test]
fn windows_installer_refuses_to_reuse_a_partial_backup_directory() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let installer_path = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .and_then(std::path::Path::parent)
        .unwrap()
        .join("scripts/installer/windows/MirrorXCodex.nsi");
    let installer = std::fs::read_to_string(&installer_path).expect("read windows installer");
    let cleanup = installer
        .split_once("Function CleanupBackup")
        .expect("installer should verify backup cleanup")
        .1
        .split_once("FunctionEnd")
        .expect("cleanup function should end")
        .0;
    let install = installer
        .split_once("Section \"Install\"")
        .expect("installer should have install section")
        .1
        .split_once("Section \"Uninstall\"")
        .expect("installer should have uninstall section")
        .0;

    assert!(cleanup.contains(r#"IfFileExists "$BackupDir\*.*" cleanup_backup_failed"#));
    let cleanup_failure_label = cleanup
        .find("cleanup_backup_failed:")
        .expect("cleanup should expose a failure label");
    assert!(cleanup[cleanup_failure_label..].contains("SetErrors"));
    let pending_delete = cleanup
        .find(r#"Delete "$BackupDir\transaction.pending""#)
        .expect("cleanup should remove the transaction marker");
    let pending_guard = cleanup
        .find(r#"IfFileExists "$BackupDir\transaction.pending" cleanup_backup_failed"#)
        .expect("cleanup should stop if the transaction marker is locked");
    let payload_delete = cleanup
        .find(r#"Delete "$BackupDir\mirror-x-codex.exe""#)
        .expect("cleanup should remove backup payloads");
    assert!(pending_delete < pending_guard && pending_guard < payload_delete);

    let cleanup_call = install
        .find("Call CleanupBackup")
        .expect("installer should clean stale backups");
    let create_transaction = install
        .find(r#"FileOpen $0 "$BackupDir\transaction.backing-up" w"#)
        .expect("installer should create a transaction marker");
    assert!(cleanup_call < create_transaction);
    assert!(install[cleanup_call..create_transaction].contains("IfErrors backup_cleanup_failed"));

    let cleanup_failure = install
        .split_once("backup_cleanup_failed:")
        .expect("installer should have a dedicated stale-backup failure path")
        .1
        .split_once("backup_failed:")
        .expect("stale-backup failure path should end before rollback path")
        .0;
    assert!(!cleanup_failure.contains("RollbackUpgrade"));
    assert!(cleanup_failure.contains("Call CleanupStage"));
}

#[test]
fn windows_uninstaller_restores_codex_before_deleting_owned_binaries() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let installer_path = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .and_then(std::path::Path::parent)
        .unwrap()
        .join("scripts/installer/windows/MirrorXCodex.nsi");
    let installer = std::fs::read_to_string(&installer_path).expect("read windows installer");
    let uninstall = installer
        .split_once("Section \"Uninstall\"")
        .expect("installer should have uninstall section")
        .1;
    let restore = uninstall
        .find("--restore-before-uninstall")
        .expect("uninstaller should invoke safe restore");
    let delete_binary = uninstall
        .find(r#"Delete "$INSTDIR\mirror-x-codex.exe""#)
        .expect("uninstaller should delete its owned launcher");

    assert!(restore < delete_binary);
    assert!(uninstall.contains(r#"StrCmp $0 "0" restore_ok"#));
    assert!(uninstall.contains(
        r#"DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "MirrorPlusWatcher""#
    ));
    assert!(uninstall.contains(r#"Delete "$SMSTARTUP\MirrorPlusWatcher.lnk""#));
    assert!(uninstall.contains(r#"DeleteRegKey HKCU "Software\Classes\mirrorplus""#));
    assert!(!installer.contains(r#"$HOME\.codex"#));
    assert!(!installer.contains("CODEX_HOME"));
}

#[test]
fn windows_uninstaller_keeps_restore_manager_until_other_binary_deletes_succeed() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let installer_path = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .and_then(std::path::Path::parent)
        .unwrap()
        .join("scripts/installer/windows/MirrorXCodex.nsi");
    let installer = std::fs::read_to_string(&installer_path).expect("read windows installer");
    let uninstall = installer
        .split_once("Section \"Uninstall\"")
        .expect("installer should have uninstall section")
        .1;

    let launcher = uninstall
        .find(r#"Delete "$INSTDIR\mirror-x-codex.exe""#)
        .expect("uninstaller should delete launcher");
    let imagegen = uninstall
        .find(r#"Delete "$INSTDIR\mirror-x-imagegen.exe""#)
        .expect("uninstaller should delete image helper");
    let legacy_launcher = uninstall
        .find(r#"Delete "$INSTDIR\codex-plus-plus.exe""#)
        .expect("uninstaller should delete legacy launcher");
    let legacy_manager = uninstall
        .find(r#"Delete "$INSTDIR\codex-plus-plus-manager.exe""#)
        .expect("uninstaller should delete legacy manager");
    let restore_manager = uninstall
        .find(r#"Delete "$INSTDIR\mirror-x-codex-manager.exe""#)
        .expect("uninstaller should delete current manager");

    assert!(launcher < imagegen);
    assert!(imagegen < legacy_launcher);
    assert!(legacy_launcher < legacy_manager);
    assert!(legacy_manager < restore_manager);

    let guarded_deletes = &uninstall[launcher..restore_manager];
    assert!(
        guarded_deletes
            .matches("IfErrors uninstall_files_failed")
            .count()
            >= 6,
        "binary, shortcut, and transaction cleanup must abort before deleting the restore manager"
    );
    assert!(guarded_deletes.contains(r#"IfFileExists "$BackupDir\*.*" uninstall_files_failed"#));
    let manager_guard = uninstall[restore_manager..]
        .find("IfErrors uninstall_files_failed")
        .expect("current manager deletion should be checked");
    assert!(
        manager_guard < 100,
        "manager deletion guard must be immediate"
    );
}

#[test]
fn windows_installer_removes_all_known_legacy_uninstall_entries() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let installer_path = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .and_then(std::path::Path::parent)
        .unwrap()
        .join("scripts/installer/windows/MirrorXCodex.nsi");
    let installer = std::fs::read_to_string(&installer_path).expect("read windows installer");

    assert_eq!(
        installer
            .matches(
                r#"DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Codex++""#
            )
            .count(),
        2,
        "install and uninstall paths should both remove the oldest Codex++ entry"
    );
}

#[test]
fn windows_installer_registers_quoted_silent_uninstall_and_ci_checks_makensis() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let repository = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .and_then(std::path::Path::parent)
        .unwrap();
    let installer =
        std::fs::read_to_string(repository.join("scripts/installer/windows/MirrorXCodex.nsi"))
            .expect("read windows installer");

    assert!(installer.contains(r#""UninstallString" '$\"$INSTDIR\uninstall.exe$\"'"#));
    assert!(installer.contains(r#""QuietUninstallString" '$\"$INSTDIR\uninstall.exe$\" /S'"#));

    for workflow in [
        ".github/workflows/pr-build.yml",
        ".github/workflows/release-assets.yml",
    ] {
        let workflow =
            std::fs::read_to_string(repository.join(workflow)).expect("read build workflow");
        assert!(workflow.contains("if ($LASTEXITCODE -ne 0)"));
        assert!(workflow.contains("makensis reported success but did not create"));
        assert!(workflow.contains("RUSTFLAGS: \"-C target-feature=+crt-static\""));
    }
}

#[test]
fn windows_entrypoints_register_mirrorplus_url_protocol() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let windows_install = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .and_then(std::path::Path::parent)
        .unwrap()
        .join("crates/codex-plus-core/src/install/windows.rs");
    let windows_install =
        std::fs::read_to_string(&windows_install).expect("read windows install source");

    assert!(windows_install.contains("Software\\Classes\\mirrorplus"));
    assert!(windows_install.contains("URL Protocol"));
    assert!(windows_install.contains("%1"));
}

#[test]
fn manager_launch_button_spawns_silent_launcher_binary() {
    let commands_rs =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/commands.rs"))
            .expect("read manager commands.rs");

    assert!(commands_rs.contains("SILENT_BINARY"));
    assert!(commands_rs.contains("std::process::Command::new"));
    assert!(!commands_rs.contains("launch_and_inject_with_hooks(options"));
}

#[test]
fn macos_packager_hides_silent_launcher_but_not_manager() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let packager = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .and_then(std::path::Path::parent)
        .unwrap()
        .join("scripts/installer/macos/package-dmg.sh");
    let script = std::fs::read_to_string(&packager).expect("read macOS packager");

    assert!(script.contains("<key>LSUIElement</key>"));
    assert!(script.contains("ARCH=\"${2:-$(uname -m)}\""));
    assert!(script.contains("BINARY_DIR=\"${BINARY_DIR:-$ROOT/target/release}\""));
    assert!(script.contains("mirror-x-codex-${VERSION}-macos-${ARCH}.dmg"));
    assert!(script.contains(
        "create_app \"mirror x codex\" \"mirror-x-codex\" \"$BINARY_DIR/mirror-x-codex\" \"club.jingziai.mirrorplus\" \"true\""
    ));
    assert!(script.contains(
        "create_app \"mirror x codex 管理器\" \"mirror-x-codex-manager\" \"$BINARY_DIR/mirror-x-codex-manager\" \"club.jingziai.mirrorplus.manager\" \"false\""
    ));
}

#[test]
fn github_release_workflow_builds_separate_macos_x64_and_arm64_dmgs() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let workflow = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .and_then(std::path::Path::parent)
        .unwrap()
        .join(".github/workflows/release-assets.yml");
    let workflow = std::fs::read_to_string(&workflow).expect("read release assets workflow");

    assert!(workflow.contains("macos-15-intel"));
    assert!(workflow.contains("x86_64-apple-darwin"));
    assert!(workflow.contains("macos-14"));
    assert!(workflow.contains("aarch64-apple-darwin"));
    assert!(workflow.contains("package-dmg.sh \"$VERSION\" \"${{ matrix.arch }}\""));
    assert!(workflow.contains("target/${{ matrix.target }}/release"));
}

#[test]
fn github_release_workflow_uploads_static_latest_json() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let workflow = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .and_then(std::path::Path::parent)
        .unwrap()
        .join(".github/workflows/release-assets.yml");
    let workflow = std::fs::read_to_string(&workflow).expect("read release assets workflow");

    assert!(workflow.contains("latest-json:"));
    assert!(workflow.contains("latest.json"));
    assert!(workflow.contains("gh release upload \"$TAG\" latest.json --clobber"));
}

#[test]
fn relay_standalone_manifest_matches_workspace_version() {
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .and_then(std::path::Path::parent)
        .expect("workspace root");
    let workspace =
        std::fs::read_to_string(workspace_root.join("Cargo.toml")).expect("workspace Cargo.toml");
    let standalone = std::fs::read_to_string(
        workspace_root.join("apps/codex-plus-mobile-relay/deploy/Cargo.standalone.toml"),
    )
    .expect("standalone relay Cargo.toml");
    let workspace_version = workspace
        .lines()
        .find_map(|line| line.trim().strip_prefix("version = "))
        .expect("workspace package version");
    let standalone_version = standalone
        .lines()
        .find_map(|line| line.trim().strip_prefix("version = "))
        .expect("standalone relay version");
    assert_eq!(
        standalone_version, workspace_version,
        "server relay health version must match the Windows release version"
    );
}

#[test]
fn relay_settings_keeps_profile_config_and_auth_files_isolated() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let app_tsx = manifest_dir.parent().unwrap().join("src/App.tsx");
    let app_tsx = std::fs::read_to_string(&app_tsx).expect("read manager App.tsx");
    let commands_rs = manifest_dir.join("src/commands.rs");
    let commands_rs = std::fs::read_to_string(&commands_rs).expect("read manager commands.rs");

    assert!(app_tsx.contains("snapshotActiveRelayFilesBeforeSwitch"));
    assert!(app_tsx.contains("backfill_relay_profile_from_live"));
    assert!(app_tsx.contains("relayProfileSwitchValidation(selectedBeforeSave)"));
    assert!(app_tsx.contains("缺少独立 config.toml"));
    assert!(app_tsx.contains("const command = relayProfileSwitchCommand(selectedAfterSave)"));
    assert!(app_tsx.contains("function relayProfileSwitchCommand"));
    assert!(app_tsx.contains("return \"apply_pure_api_injection\""));
    assert!(app_tsx.contains("return \"apply_relay_injection\""));
    assert!(app_tsx.contains("const createNewAggregateProfile = () =>"));
    assert!(app_tsx.contains("onClick={createNewAggregateProfile}"));
    assert!(app_tsx.contains("已打开聚合供应商详情"));
    assert!(!commands_rs.contains("缺少独立 auth.json"));
    assert!(commands_rs.contains("backfill_relay_profile_from_live"));
    assert!(commands_rs.contains("apply_relay_profile_to_home_with_switch_rules"));
}

#[test]
fn relay_context_management_is_global_not_supplier_scoped() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let app_tsx = manifest_dir.parent().unwrap().join("src/App.tsx");
    let app_tsx = std::fs::read_to_string(&app_tsx).expect("read manager App.tsx");
    let styles = manifest_dir.parent().unwrap().join("src/styles.css");
    let styles = std::fs::read_to_string(&styles).expect("read manager styles.css");

    assert!(app_tsx.contains("作为全局配置独立管理"));
    assert!(
        app_tsx.contains("label: t(\"工具与插件\")") || app_tsx.contains("label: \"工具与插件\"")
    );
    assert!(
        app_tsx.contains("title={t(\"Codex 工具与插件\")}")
            || app_tsx.contains("title=\"Codex 工具与插件\"")
    );
    assert!(!app_tsx.contains("label: \"上下文配置\""));
    assert!(!app_tsx.contains("title=\"上下文配置\""));
    assert!(!app_tsx.contains("<strong>Codex 上下文</strong>"));
    assert!(app_tsx.contains("id: \"context\""));
    assert!(app_tsx.contains("function ContextScreen"));
    assert!(app_tsx.contains("route === \"context\""));
    assert!(app_tsx.contains("if (next === \"context\")"));
    assert!(app_tsx.contains("selectedContextConfigToml(entries)"));
    assert!(app_tsx.contains("toggleContextEntryEnabled"));
    assert!(app_tsx.contains("relayFiles={relayFiles}"));
    assert!(app_tsx.contains("read_live_context_entries"));
    assert!(app_tsx.contains("sync_live_context_entries"));
    assert!(app_tsx.contains("refreshLiveContextEntries"));
    assert!(app_tsx.contains("syncLiveContextEntries(next, true)"));
    assert!(app_tsx.contains("function contextEntriesWithLiveEntries"));
    assert!(app_tsx.contains("liveByKind"));
    assert!(app_tsx.contains("mergeLiveContextEntries"));
    assert!(app_tsx.contains("withLiveEntryState"));
    assert!(app_tsx.contains("contextEnabledSwitch"));
    assert!(!app_tsx.contains("entry.enabled ? \"已启用\" : \"已禁用\""));
    assert!(!app_tsx.contains("空配置体"));
    assert!(app_tsx.contains("relay-context-delete"));
    assert!(!app_tsx.contains("切换供应商时只合并勾选项"));
    assert!(!app_tsx.contains("未勾选的条目不会写入"));
    assert!(!app_tsx.contains("className=\"context-switch\""));
    assert!(!styles.contains(".context-switch {"));
    assert!(styles.contains(".context-enabled-switch"));
    assert!(styles.contains(".context-switch-track"));
    assert!(styles.contains(".context-switch-thumb"));
    assert!(!styles.contains(".relay-context-row code"));
    assert!(styles.contains(".relay-context-delete"));
}

#[test]
fn manager_window_and_relay_detail_header_stay_usable() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let app_tsx = manifest_dir.parent().unwrap().join("src/App.tsx");
    let app_tsx = std::fs::read_to_string(&app_tsx).expect("read manager App.tsx");
    let styles = manifest_dir.parent().unwrap().join("src/styles.css");
    let styles = std::fs::read_to_string(&styles).expect("read manager styles.css");
    let lib_rs =
        std::fs::read_to_string(manifest_dir.join("src/lib.rs")).expect("read manager lib.rs");
    let tauri_conf =
        std::fs::read_to_string(manifest_dir.join("tauri.conf.json")).expect("read tauri config");

    assert!(app_tsx.contains("relay-detail-sticky"));
    assert!(!app_tsx.contains("CardHead title=\"供应商详情\""));
    assert!(styles.contains(".relay-detail-sticky"));
    assert!(styles.contains("position: sticky"));
    assert!(styles.contains("top: 0"));
    assert!(styles.contains("margin: 0"));
    assert!(lib_rs.contains(".inner_size(880.0, 700.0)"));
    assert!(lib_rs.contains(".min_inner_size(640.0, 600.0)"));
    assert!(tauri_conf.contains("\"width\": 880"));
    assert!(tauri_conf.contains("\"height\": 700"));
    assert!(tauri_conf.contains("\"minWidth\": 640"));
    assert!(tauri_conf.contains("\"minHeight\": 600"));
}

#[test]
fn manager_defaults_to_simple_mirror_access_workflow() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let frontend = manifest_dir.parent().unwrap().join("src");
    let main = std::fs::read_to_string(frontend.join("main.tsx")).expect("read main.tsx");
    let simple =
        std::fs::read_to_string(frontend.join("SimpleApp.tsx")).expect("read SimpleApp.tsx");
    let commands =
        std::fs::read_to_string(manifest_dir.join("src/commands.rs")).expect("read commands.rs");
    let lib = std::fs::read_to_string(manifest_dir.join("src/lib.rs")).expect("read lib.rs");

    assert!(main.contains("import(\"./SimpleApp\")"));
    assert!(main.contains("render(<SimpleApp />)"));
    assert!(simple.contains("API Key"));
    assert!(simple.contains("混合 API"));
    assert!(simple.contains("纯 API"));
    assert!(simple.contains("恢复使用前状态"));
    assert!(simple.contains("选择此 Key 负责的模型"));
    assert!(simple.contains("CodexPro Key"));
    assert!(simple.contains("企业GPT专线（极稳）"));
    assert!(!simple.contains("账号自动填入"));
    assert!(!simple.contains("Claude Key"));
    assert!(simple.contains("keyGroups"));
    assert!(simple.contains("selectedModelIds"));
    assert!(simple.contains("defaultModel"));
    assert!(simple.contains("完全退出 Codex"));
    assert!(simple.contains("打开 Codex"));
    assert!(simple.contains("前往镜子AI获取 Key"));
    assert!(simple.contains("https://api.jingziai.club/pricing"));
    assert!(simple.contains("open_external_url"));
    for command in [
        "get_mirror_access_status",
        "validate_mirror_key",
        "enable_mirror_access",
        "repair_mirror_sessions",
        "restore_pre_mirror_state",
    ] {
        assert!(commands.contains(&format!("fn {command}")));
        assert!(lib.contains(&format!("commands::{command}")));
    }
    assert!(!commands.contains("fn load_mirror_account_keys"));
    assert!(!lib.contains("commands::load_mirror_account_keys"));
}

#[test]
fn old_codex_recovery_exposes_official_update_command() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let lib =
        std::fs::read_to_string(manifest_dir.join("src/lib.rs")).expect("read manager lib.rs");
    let setup =
        std::fs::read_to_string(manifest_dir.join("src/codex_setup.rs")).expect("read codex setup");
    let simple = std::fs::read_to_string(manifest_dir.parent().unwrap().join("src/SimpleApp.tsx"))
        .expect("read simple app");

    assert!(lib.contains("codex_setup::update_codex_desktop"));
    assert!(setup.contains("winget_update_args"));
    assert!(setup.contains("CODEX_STORE_PRODUCT_ID"));
    assert!(simple.contains("operation === \"update-codex\""));
    assert!(simple.contains("正在更新 Codex"));
}

#[test]
fn relay_preview_deduplicates_root_keys_when_merging_common_config() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let app_tsx = manifest_dir.parent().unwrap().join("src/App.tsx");
    let app_tsx = std::fs::read_to_string(&app_tsx).expect("read manager App.tsx");

    assert!(app_tsx.contains("dedupeTomlRootLines"));
    assert!(app_tsx.contains("rootSeen.add(key)"));
    assert!(app_tsx.contains("joinTomlSectionsRootFirst"));
}

#[test]
fn provider_presets_include_jingziai() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let presets = manifest_dir.parent().unwrap().join("src/presets.ts");
    let presets = std::fs::read_to_string(&presets).expect("read manager presets.ts");

    assert!(presets.contains("id: \"jingziai\""));
    assert!(presets.contains("name: \"mirror x codex\""));
    assert!(presets.contains("category: \"aggregator\""));
    assert!(presets.contains("baseUrl: \"https://api.jingziai.club/v1\""));
}

#[test]
fn manager_no_longer_exposes_mobile_control() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let app_tsx = manifest_dir.parent().unwrap().join("src/App.tsx");
    let app_tsx = std::fs::read_to_string(&app_tsx).expect("read manager App.tsx");

    assert!(!app_tsx.contains("mobileControl"));
    assert!(!app_tsx.contains("手机控制"));
    assert!(!app_tsx.contains("mobileRelayServers"));
    assert!(!app_tsx.contains("MobileControlScreen"));
}

#[test]
fn manager_simple_app_exposes_mobile_control_without_legacy_route() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let app_tsx = manifest_dir.parent().unwrap().join("src/App.tsx");
    let app_tsx = std::fs::read_to_string(&app_tsx).expect("read manager App.tsx");
    let simple_app_tsx = manifest_dir.parent().unwrap().join("src/SimpleApp.tsx");
    let simple_app_tsx =
        std::fs::read_to_string(&simple_app_tsx).expect("read manager SimpleApp.tsx");
    let mobile_panel_tsx = manifest_dir
        .parent()
        .unwrap()
        .join("src/MobileControlPanel.tsx");
    let mobile_panel_tsx =
        std::fs::read_to_string(&mobile_panel_tsx).expect("read manager MobileControlPanel.tsx");

    assert!(!app_tsx.contains("mobileControl"));
    assert!(!app_tsx.contains("mobileRelayServers"));
    assert!(!app_tsx.contains("MobileControlScreen"));

    assert!(simple_app_tsx.contains("import { MobileControlPanel }"));
    assert!(simple_app_tsx.contains("<MobileControlPanel onNotice={setNotice} />"));

    assert!(mobile_panel_tsx.contains("get_mobile_control_status"));
    assert!(mobile_panel_tsx.contains("enable_mobile_control"));
    assert!(mobile_panel_tsx.contains("disable_mobile_control"));
    assert!(mobile_panel_tsx.contains("generate_mobile_qr_code"));
    assert!(mobile_panel_tsx.contains("#mx=preview"));
}

#[test]
fn manager_ui_no_longer_exposes_command_wrapper_or_startup_marketplace_prompt() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let app_tsx = manifest_dir.parent().unwrap().join("src/App.tsx");
    let app_tsx = std::fs::read_to_string(&app_tsx).expect("read manager App.tsx");

    assert!(!app_tsx.contains("启用 Codex 命令包装器"));
    assert!(!app_tsx.contains("修复后端"));
    assert!(!app_tsx.contains("repairBackend"));
    assert!(!app_tsx.contains("await checkPluginMarketplacePrompt()"));
}

#[test]
fn manager_update_install_keeps_visible_progress_bar() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let app_tsx = manifest_dir.parent().unwrap().join("src/App.tsx");
    let app_tsx = std::fs::read_to_string(&app_tsx).expect("read manager App.tsx");

    assert!(app_tsx.contains("下载并运行安装包"));
    assert!(app_tsx.contains("updateInstallProgress"));
    assert!(app_tsx.contains("安装包更新进度"));
    assert!(app_tsx.contains("completedTitle={t(\"上次更新结果\")}"));
    assert!(app_tsx.contains("progress={updateInstallProgress}"));
}
