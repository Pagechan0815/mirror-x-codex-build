use codex_plus_core::install::{
    InstallOptions, SILENT_BINARY, app_bundle_names, build_macos_app_bundle,
    build_windows_entrypoint_plan, companion_binary_path_from_exe, default_install_root_strategy,
    shortcut_names,
};

#[test]
fn windows_entrypoint_plan_contains_silent_and_manager_entrypoints() {
    let options = InstallOptions {
        install_root: Some("C:/Users/A/Desktop".into()),
        launcher_path: Some("C:/Tools/mirror-x-codex.exe".into()),
        manager_path: Some("C:/Tools/mirror-x-codex-manager.exe".into()),
        remove_owned_data: false,
    };

    let plan = build_windows_entrypoint_plan(&options);

    assert!(plan.silent_shortcut.ends_with("mirror x codex.lnk"));
    assert!(plan.manager_shortcut.ends_with("mirror x codex 管理器.lnk"));
    assert_eq!(plan.launcher_path, "C:/Tools/mirror-x-codex.exe");
    assert_eq!(plan.manager_path, "C:/Tools/mirror-x-codex-manager.exe");
    assert_eq!(plan.silent_icon_path, "C:/Tools/mirror-x-codex.exe");
    assert_eq!(
        plan.manager_icon_path,
        "C:/Tools/mirror-x-codex-manager.exe"
    );
    assert_eq!(plan.uninstall_key, "MirrorXCodex");
    assert_eq!(plan.legacy_uninstall_key, "MirrorPlus");
    assert_eq!(
        plan.uninstaller_path.replace('\\', "/"),
        "C:/Tools/uninstall.exe"
    );
    assert_eq!(
        plan.uninstall_command.replace('\\', "/"),
        "\"C:/Tools/uninstall.exe\""
    );
    assert_eq!(
        plan.quiet_uninstall_command.replace('\\', "/"),
        "\"C:/Tools/uninstall.exe\" /S"
    );
    assert_ne!(
        plan.uninstall_command,
        "\"C:/Tools/mirror-x-codex-manager.exe\""
    );
}

#[test]
fn windows_entrypoint_plan_can_request_owned_data_removal_without_shell_script() {
    let options = InstallOptions {
        install_root: Some("C:/Users/A/Desktop".into()),
        launcher_path: None,
        manager_path: None,
        remove_owned_data: true,
    };

    let plan = build_windows_entrypoint_plan(&options);

    assert!(plan.silent_shortcut.ends_with("mirror x codex.lnk"));
    assert!(plan.manager_shortcut.ends_with("mirror x codex 管理器.lnk"));
    assert!(plan.remove_owned_data);
}

#[test]
fn windows_entrypoint_repair_uses_current_uninstaller_contract_without_stranding_app() {
    let source = std::fs::read_to_string("src/install/windows.rs")
        .expect("read Windows entrypoint implementation");
    assert!(source.contains(r#"Uninstall\MirrorXCodex"#));
    assert!(source.contains("Path::new(&plan.uninstaller_path).is_file()"));

    let uninstall = source
        .split_once("pub fn uninstall_shortcuts")
        .expect("uninstall_shortcuts should exist")
        .1
        .split_once("#[cfg(not(windows))]")
        .expect("Windows uninstall implementation should end")
        .0;
    assert!(!uninstall.contains("delete_current_user_key(UNINSTALL_SUBKEY)"));
    assert!(uninstall.contains("delete_current_user_key(LEGACY_UNINSTALL_SUBKEY)"));
    assert!(uninstall.contains("delete_current_user_key(OLDEST_UNINSTALL_SUBKEY)"));
}

#[test]
fn macos_bundle_metadata_contains_silent_and_manager_apps() {
    let options = InstallOptions {
        install_root: Some("/Applications".into()),
        launcher_path: Some("/opt/mirror-x-codex/mirror-x-codex".into()),
        manager_path: Some("/opt/mirror-x-codex/mirror-x-codex-manager".into()),
        remove_owned_data: false,
    };

    let silent = build_macos_app_bundle(&options, false);
    let manager = build_macos_app_bundle(&options, true);

    assert!(silent.app_path.ends_with("mirror x codex.app"));
    assert!(manager.app_path.ends_with("mirror x codex 管理器.app"));
    assert!(
        silent
            .info_plist
            .contains("<string>mirror x codex</string>")
    );
    assert!(
        manager
            .info_plist
            .contains("<string>mirror x codex 管理器</string>")
    );
    assert_eq!(silent.binary_target_name.as_deref(), Some("mirror-x-codex"));
    assert_eq!(
        manager.binary_target_name.as_deref(),
        Some("mirror-x-codex-manager")
    );
    assert!(silent.launch_script.is_empty());
    assert!(manager.launch_script.is_empty());
    assert!(
        silent
            .info_plist
            .contains("<key>LSUIElement</key>\n  <true/>")
    );
    assert!(
        manager
            .info_plist
            .contains("<key>LSUIElement</key>\n  <false/>")
    );
    assert!(!silent.info_plist.contains("<string>mirrorplus</string>"));
    assert!(manager.info_plist.contains("<string>mirrorplus</string>"));
    assert!(
        manager
            .info_plist
            .contains("<string>codexplusplus</string>")
    );
    assert!(manager.info_plist.contains("mirror-x-codex.icns"));
}

#[test]
fn installer_exports_expected_two_entrypoint_names() {
    assert_eq!(
        shortcut_names(),
        ("mirror x codex.lnk", "mirror x codex 管理器.lnk")
    );
    assert_eq!(
        app_bundle_names(),
        ("mirror x codex.app", "mirror x codex 管理器.app")
    );
}

#[test]
fn macos_dmg_includes_applications_shortcut_for_drag_install() {
    let script = std::fs::read_to_string("../../scripts/installer/macos/package-dmg.sh")
        .expect("read macOS DMG packaging script");

    assert!(script.contains("ln -s /Applications \"$STAGE/Applications\""));
    assert!(script.contains("<string>mirrorplus</string>"));
    assert!(script.contains("for attempt in 1 2 3; do"));

    let verifier = std::fs::read_to_string("../../scripts/installer/macos/verify-dmg.sh")
        .expect("read macOS DMG verifier");
    assert!(verifier.contains("Print :LSUIElement"));
    assert!(verifier.contains("Print :CFBundleURLTypes:0:CFBundleURLSchemes"));
    assert!(verifier.contains("Contents/Resources/mirror-x-codex.icns"));
    assert!(verifier.contains("stat -f '%z'"));
}

#[test]
fn macos_entrypoint_repair_fails_closed_and_writes_complete_bundles() {
    let source = std::fs::read_to_string("src/install/macos.rs")
        .expect("read macOS entrypoint implementation");

    assert!(source.contains("validate_binary_source(source)?"));
    assert!(source.contains("validate_binary_source(&target)?"));
    assert!(source.contains("macOS bundle is missing its binary source"));
    assert!(source.contains("contents.join(\"PkgInfo\")"));
    assert!(source.contains("contents.join(\"Resources\")"));
    assert!(source.contains("metadata.len() < 1024"));
    assert!(source.contains("header == *b\"#!\""));
}

#[test]
fn companion_binary_path_resolves_macos_silent_app_next_to_manager_app() {
    let manager_exe = std::path::Path::new(
        "/Applications/mirror x codex 管理器.app/Contents/MacOS/mirror-x-codex-manager",
    );

    let companion = companion_binary_path_from_exe(manager_exe, SILENT_BINARY);

    assert_eq!(
        companion,
        std::path::PathBuf::from("/Applications/mirror x codex.app/Contents/MacOS/mirror-x-codex")
    );
    assert_ne!(
        companion,
        std::path::PathBuf::from(
            "/Applications/mirror x codex 管理器.app/Contents/MacOS/mirror-x-codex"
        )
    );
}

#[test]
fn companion_binary_path_resolves_macos_manager_app_next_to_silent_app() {
    let silent_exe =
        std::path::Path::new("/Applications/mirror x codex.app/Contents/MacOS/mirror-x-codex");

    let companion =
        companion_binary_path_from_exe(silent_exe, codex_plus_core::install::MANAGER_BINARY);

    assert_eq!(
        companion,
        std::path::PathBuf::from(
            "/Applications/mirror x codex 管理器.app/Contents/MacOS/mirror-x-codex-manager"
        )
    );
}

#[test]
fn macos_bundle_does_not_wrap_the_bundle_executable_in_itself() {
    let options = InstallOptions {
        install_root: Some("/Applications".into()),
        launcher_path: Some(
            "/Applications/mirror x codex.app/Contents/MacOS/mirror-x-codex".into(),
        ),
        manager_path: Some(
            "/Applications/mirror x codex 管理器.app/Contents/MacOS/mirror-x-codex-manager".into(),
        ),
        remove_owned_data: false,
    };

    let silent = build_macos_app_bundle(&options, false);
    let manager = build_macos_app_bundle(&options, true);

    assert_eq!(
        silent.binary_source,
        Some(std::path::PathBuf::from(
            "/Applications/mirror x codex.app/Contents/MacOS/mirror-x-codex"
        ))
    );
    assert_eq!(
        manager.binary_source,
        Some(std::path::PathBuf::from(
            "/Applications/mirror x codex 管理器.app/Contents/MacOS/mirror-x-codex-manager"
        ))
    );
    assert!(silent.launch_script.is_empty());
    assert!(manager.launch_script.is_empty());
}

#[test]
fn windows_default_install_root_uses_known_folder_before_userprofile_desktop() {
    let strategy = default_install_root_strategy();

    if cfg!(windows) {
        assert_eq!(strategy, "windows-known-folder");
    } else if cfg!(target_os = "macos") {
        assert_eq!(strategy, "macos-applications");
    } else {
        assert_eq!(strategy, "user-dirs-desktop");
    }
}

#[test]
fn windows_nsis_keeps_utf8_bom_for_unicode_installer_text() {
    let bytes = std::fs::read("../../scripts/installer/windows/MirrorXCodex.nsi")
        .expect("read Windows NSIS installer");

    assert!(
        bytes.starts_with(&[0xEF, 0xBB, 0xBF]),
        "makensis otherwise parses the Chinese messages as the active code page"
    );
    assert!(String::from_utf8(bytes).is_ok());
}

#[test]
fn windows_nsis_webview2_bootstrap_is_https_signed_and_recoverable() {
    let source = std::fs::read_to_string("../../scripts/installer/windows/MirrorXCodex.nsi")
        .expect("read Windows NSIS installer");

    assert!(source.contains("{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"));
    assert!(source.contains(r#"ReadRegStr $0 HKLM "SOFTWARE\Microsoft\EdgeUpdate\Clients\${WEBVIEW2_RUNTIME_CLIENT_ID}" "pv""#));
    assert!(source.contains(r#"ReadRegStr $0 HKCU "Software\Microsoft\EdgeUpdate\Clients\${WEBVIEW2_RUNTIME_CLIENT_ID}" "pv""#));
    assert!(source.contains(r#"StrCmp $0 "0.0.0.0""#));

    assert!(!source.contains("NSISdl::download"));
    assert!(source.contains(r#"$SYSDIR\curl.exe" --fail --location"#));
    assert!(source.contains("--connect-timeout 15 --max-time 120"));
    assert!(source.contains("Invoke-WebRequest -UseBasicParsing"));
    assert!(source.contains("-TimeoutSec 120"));
    assert!(source.contains("Get-AuthenticodeSignature"));
    assert!(source.contains("CN=Microsoft Corporation"));
    assert!(source.contains("https://go.microsoft.com/fwlink/p/?LinkId=2124703"));
    assert!(source.contains(
        "https://developer.microsoft.com/microsoft-edge/webview2#download-the-webview2-runtime"
    ));
    assert!(source.contains(r#"ExecWait '"$0" /silent /install' $1"#));
}

#[test]
fn windows_nsis_checks_dependencies_and_process_exit_before_overwrite() {
    let source = std::fs::read_to_string("../../scripts/installer/windows/MirrorXCodex.nsi")
        .expect("read Windows NSIS installer");
    let install = source
        .split_once("Section \"Install\"")
        .expect("Windows install section should exist")
        .1
        .split_once("SectionEnd")
        .expect("Windows install section should end")
        .0;

    let prerequisite = install
        .find("Call EnsureWebView2")
        .expect("WebView2 prerequisite check should run");
    let stop = install
        .find("Call StopProductProcess")
        .expect("running product should be stopped");
    let stage = install
        .find("CreateDirectory \"$StageDir\"")
        .expect("new binaries should be staged before overwrite");
    assert!(prerequisite < stop && stop < stage);

    let stop_function = source
        .split_once("Function ${PREFIX}StopProductProcess")
        .expect("stop-process function should exist")
        .1
        .split_once("FunctionEnd")
        .expect("stop-process function should end")
        .0;
    assert!(stop_function.contains(r#""$SYSDIR\taskkill.exe" /IM "$0""#));
    assert!(!stop_function.contains(" /F"));
    assert!(stop_function.contains("Sleep 500"));
    assert!(stop_function.contains("IntCmp $2 10"));
    assert!(stop_function.contains(r#"StrCmp $1 "128" process_stop_confirmed"#));
}
