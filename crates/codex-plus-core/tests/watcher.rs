use codex_plus_core::watcher::{
    build_spawn_launcher_command, build_watcher_install_plan, cdp_listening, codex_process_ids,
    disable_watcher_at, enable_watcher_at, filter_killable_launcher_processes,
    process_ids_still_running, watcher_disabled_flag,
};

#[cfg(windows)]
use codex_plus_core::watcher::{WindowsProcessInfo, find_codex_processes_from_snapshot};

#[test]
fn cdp_listening_returns_true_for_bound_loopback_port() {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();

    assert!(cdp_listening(port));
}

#[test]
fn cdp_listening_returns_true_for_bound_ipv6_loopback_port() {
    let listener = std::net::TcpListener::bind("[::1]:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    assert!(cdp_listening(port));
}

#[test]
fn cdp_listening_returns_false_for_closed_port() {
    let port = {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.local_addr().unwrap().port()
    };

    assert!(!cdp_listening(port));
}

#[test]
fn watcher_enable_and_disable_toggle_flag() {
    let dir = tempfile::tempdir().unwrap();
    let flag = watcher_disabled_flag(dir.path());

    disable_watcher_at(dir.path()).unwrap();
    assert!(flag.exists());

    enable_watcher_at(dir.path()).unwrap();
    assert!(!flag.exists());
}

#[test]
fn watcher_install_plan_registers_rust_launcher_at_logon() {
    let plan = build_watcher_install_plan("C:/Tools/mirror-x-codex.exe".into(), 9333);

    assert_eq!(plan.run_value_name, "MirrorPlusWatcher");
    assert_eq!(
        plan.run_value,
        "\"C:/Tools/mirror-x-codex.exe\" --debug-port 9333"
    );
    assert_eq!(plan.shortcut_name, "MirrorPlusWatcher.lnk");
    assert_eq!(plan.shortcut_target, "C:/Tools/mirror-x-codex.exe");
    assert_eq!(plan.shortcut_arguments, "--debug-port 9333");
}

#[test]
fn spawn_launcher_command_points_to_silent_binary_only() {
    let command = build_spawn_launcher_command("C:/Tools/mirror-x-codex.exe", 9444);

    assert_eq!(command[0], "C:/Tools/mirror-x-codex.exe");
    assert!(command.contains(&"--debug-port".to_string()));
    assert!(command.contains(&"9444".to_string()));
    assert!(!command.iter().any(|part| part.contains("manager")));
}

#[test]
fn codex_process_filter_keeps_only_windowsapps_codex_processes() {
    let processes = [
        (
            11,
            r"C:\Program Files\WindowsApps\OpenAI.Codex_1.0.0.0_x64__abc\app\Codex.exe",
        ),
        (12, r"C:\Tools\Codex.exe"),
        (
            13,
            r"C:\Program Files\WindowsApps\Other.App_1.0.0.0_x64__abc\app\Codex.exe",
        ),
        (
            14,
            r"C:\Program Files\WindowsApps\OpenAI.Codex_26.814.5517.0_x64__abc\app\ChatGPT.exe",
        ),
        (
            15,
            r"C:\Program Files\WindowsApps\Microsoft.ChatGPT_1.0.0.0_x64__abc\app\ChatGPT.exe",
        ),
        (
            16,
            r"C:\Program Files\WindowsApps\OpenAI.CodexBeta_26.900.1.0_x64__abc\app\ChatGPT.exe",
        ),
        (
            17,
            r"C:\Program Files\WindowsApps\OpenAI.ChatGPT-Desktop_26.900.1.0_x64__abc\app\ChatGPT.exe",
        ),
    ];

    assert_eq!(codex_process_ids(processes), vec![11, 14, 16, 17]);
}

#[test]
fn launcher_process_filter_protects_current_process_ancestry() {
    let processes = [
        (10, 0, "mirror-x-codex.exe"),
        (20, 10, "mirror-x-codex.exe"),
        (30, 20, "mirror-x-codex.exe"),
        (40, 10, "mirror-x-codex.exe"),
        (50, 10, "mirror-x-codex-manager.exe"),
    ];

    assert_eq!(filter_killable_launcher_processes(processes, 30), vec![40]);
}

#[test]
fn stop_wait_tracks_only_expected_process_ids() {
    assert_eq!(
        process_ids_still_running(&[10, 20, 30], [5, 20, 40, 30]),
        vec![20, 30]
    );
}

#[cfg(windows)]
#[test]
fn find_codex_processes_finds_local_install_with_capitial_c() {
    let processes = [WindowsProcessInfo {
        process_id: 42,
        parent_process_id: 0,
        exe_file: "Codex.exe".to_string(),
        executable_path: Some(std::path::PathBuf::from(
            r"D:\360Downloads\codexapp\app\Codex.exe",
        )),
    }];

    assert_eq!(find_codex_processes_from_snapshot(&processes), vec![42]);
}

#[cfg(windows)]
#[test]
fn find_codex_processes_ignores_lowercase_local_cli_binary() {
    let processes = [WindowsProcessInfo {
        process_id: 43,
        parent_process_id: 0,
        exe_file: "codex.exe".to_string(),
        executable_path: Some(std::path::PathBuf::from(
            r"D:\360Downloads\codexapp\app\codex.exe",
        )),
    }];

    assert!(find_codex_processes_from_snapshot(&processes).is_empty());
}

#[cfg(windows)]
#[test]
fn find_codex_processes_ignores_npm_cli_binary() {
    let processes = [WindowsProcessInfo {
        process_id: 44,
        parent_process_id: 0,
        exe_file: "codex.exe".to_string(),
        executable_path: Some(std::path::PathBuf::from(
            r"C:\Users\me\AppData\Roaming\npm\node_modules\@openai\codex\node_modules\@openai\codex-win32-x64\vendor\x86_64-pc-windows-msvc\bin\codex.exe",
        )),
    }];

    assert!(find_codex_processes_from_snapshot(&processes).is_empty());
}

#[cfg(windows)]
#[test]
fn find_codex_processes_ignores_packaged_resource_cli_binary() {
    let processes = [WindowsProcessInfo {
        process_id: 45,
        parent_process_id: 0,
        exe_file: "codex.exe".to_string(),
        executable_path: Some(std::path::PathBuf::from(
            r"C:\Program Files\WindowsApps\OpenAI.Codex_1.0.0.0_x64__abc\app\resources\codex.exe",
        )),
    }];

    assert!(find_codex_processes_from_snapshot(&processes).is_empty());
}

#[cfg(windows)]
#[test]
fn find_codex_processes_finds_current_packaged_chatgpt_shell() {
    let processes = [WindowsProcessInfo {
        process_id: 46,
        parent_process_id: 0,
        exe_file: "ChatGPT.exe".to_string(),
        executable_path: Some(std::path::PathBuf::from(
            r"C:\Program Files\WindowsApps\OpenAI.Codex_26.814.5517.0_x64__abc\app\ChatGPT.exe",
        )),
    }];

    assert_eq!(find_codex_processes_from_snapshot(&processes), vec![46]);
}

#[cfg(windows)]
#[test]
fn find_codex_processes_ignores_unrelated_packaged_chatgpt_shell() {
    let processes = [WindowsProcessInfo {
        process_id: 47,
        parent_process_id: 0,
        exe_file: "ChatGPT.exe".to_string(),
        executable_path: Some(std::path::PathBuf::from(
            r"C:\Program Files\WindowsApps\Microsoft.ChatGPT_1.0.0.0_x64__abc\app\ChatGPT.exe",
        )),
    }];

    assert!(find_codex_processes_from_snapshot(&processes).is_empty());
}

#[cfg(windows)]
#[test]
fn find_codex_processes_finds_supported_standalone_chatgpt_shells() {
    let processes = [
        WindowsProcessInfo {
            process_id: 48,
            parent_process_id: 0,
            exe_file: "ChatGPT.exe".to_string(),
            executable_path: Some(std::path::PathBuf::from(
                r"C:\Users\me\AppData\Local\OpenAI\Codex\bin\ChatGPT.exe",
            )),
        },
        WindowsProcessInfo {
            process_id: 49,
            parent_process_id: 0,
            exe_file: "ChatGPT.exe".to_string(),
            executable_path: Some(std::path::PathBuf::from(
                r"C:\Users\me\AppData\Local\Programs\ChatGPT\ChatGPT.exe",
            )),
        },
    ];

    assert_eq!(find_codex_processes_from_snapshot(&processes), vec![48, 49]);
}

#[cfg(windows)]
#[test]
fn find_codex_processes_finds_manually_selected_portable_chatgpt_shell() {
    let processes = [WindowsProcessInfo {
        process_id: 50,
        parent_process_id: 0,
        exe_file: "ChatGPT.exe".to_string(),
        executable_path: Some(std::path::PathBuf::from(
            r"D:\Portable\OpenAI Desktop\ChatGPT.exe",
        )),
    }];

    assert_eq!(find_codex_processes_from_snapshot(&processes), vec![50]);
}

#[cfg(windows)]
#[test]
fn find_codex_processes_combines_store_and_local_installs() {
    let processes = [
        WindowsProcessInfo {
            process_id: 11,
            parent_process_id: 0,
            exe_file: "Codex.exe".to_string(),
            executable_path: Some(std::path::PathBuf::from(
                r"C:\Program Files\WindowsApps\OpenAI.Codex_1.0.0.0_x64__abc\app\Codex.exe",
            )),
        },
        WindowsProcessInfo {
            process_id: 42,
            parent_process_id: 0,
            exe_file: "Codex.exe".to_string(),
            executable_path: Some(std::path::PathBuf::from(
                r"D:\360Downloads\codexapp\app\Codex.exe",
            )),
        },
    ];

    assert_eq!(find_codex_processes_from_snapshot(&processes), vec![11, 42]);
}

#[cfg(windows)]
#[test]
fn find_codex_processes_ignores_unrelated_processes() {
    let processes = [
        WindowsProcessInfo {
            process_id: 10,
            parent_process_id: 0,
            exe_file: "notepad.exe".to_string(),
            executable_path: Some(std::path::PathBuf::from(r"C:\Windows\notepad.exe")),
        },
        WindowsProcessInfo {
            process_id: 20,
            parent_process_id: 0,
            exe_file: "mirror-x-codex.exe".to_string(),
            executable_path: Some(std::path::PathBuf::from(
                r"D:\Programs\Mirror X Codex\mirror-x-codex.exe",
            )),
        },
    ];

    assert!(find_codex_processes_from_snapshot(&processes).is_empty());
}
