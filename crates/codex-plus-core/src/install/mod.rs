use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

pub mod macos;
pub mod windows;

pub const SILENT_NAME: &str = "mirror x codex";
pub const MANAGER_NAME: &str = "mirror x codex 管理器";
pub const SILENT_BINARY: &str = "mirror-x-codex";
pub const MANAGER_BINARY: &str = "mirror-x-codex-manager";
pub const LEGACY_SILENT_BINARY: &str = "codex-plus-plus";
pub const LEGACY_MANAGER_BINARY: &str = "codex-plus-plus-manager";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct InstallOptions {
    #[serde(default)]
    pub install_root: Option<PathBuf>,
    #[serde(default)]
    pub launcher_path: Option<PathBuf>,
    #[serde(default)]
    pub manager_path: Option<PathBuf>,
    #[serde(default)]
    pub remove_owned_data: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShortcutState {
    pub installed: bool,
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EntryPointState {
    pub silent_shortcut: ShortcutState,
    pub management_shortcut: ShortcutState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstallActionResult {
    pub status: String,
    pub message: String,
    pub silent_shortcut: ShortcutState,
    pub management_shortcut: ShortcutState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacosAppBundle {
    pub app_path: PathBuf,
    pub info_plist: String,
    pub launch_script: String,
    pub binary_source: Option<PathBuf>,
    pub binary_target_name: Option<String>,
}

impl ShortcutState {
    pub fn missing(path: Option<PathBuf>) -> Self {
        Self {
            installed: false,
            path: path.map(|path| path.to_string_lossy().to_string()),
        }
    }

    pub fn from_candidates(candidates: Vec<PathBuf>) -> Self {
        if let Some(path) = candidates.iter().find(|path| path.exists()) {
            return Self {
                installed: true,
                path: Some(path.to_string_lossy().to_string()),
            };
        }
        Self::missing(candidates.into_iter().next())
    }
}

pub fn shortcut_names() -> (&'static str, &'static str) {
    ("mirror x codex.lnk", "mirror x codex 管理器.lnk")
}

pub fn app_bundle_names() -> (&'static str, &'static str) {
    ("mirror x codex.app", "mirror x codex 管理器.app")
}

pub fn inspect_entrypoints() -> EntryPointState {
    let root = default_install_root();
    EntryPointState {
        silent_shortcut: ShortcutState::from_candidates(entrypoint_candidates(&root, false)),
        management_shortcut: ShortcutState::from_candidates(entrypoint_candidates(&root, true)),
    }
}

pub fn install_entrypoints(options: &InstallOptions) -> InstallActionResult {
    let result = platform_install(options);
    action_result(result, "入口已安装。")
}

pub fn uninstall_entrypoints(options: &InstallOptions) -> InstallActionResult {
    let result = uninstall_entrypoints_checked(options);
    action_result(result, "入口已卸载。")
}

pub fn repair_entrypoints(options: &InstallOptions) -> InstallActionResult {
    let result = platform_install(options);
    action_result(result, "入口已修复。")
}

pub fn build_windows_entrypoint_plan(options: &InstallOptions) -> windows::WindowsEntrypointPlan {
    windows::build_windows_entrypoint_plan(options)
}

pub fn build_macos_app_bundle(options: &InstallOptions, manager: bool) -> MacosAppBundle {
    macos::build_app_bundle(options, manager)
}

fn uninstall_entrypoints_checked(options: &InstallOptions) -> anyhow::Result<()> {
    uninstall_entrypoints_checked_with(
        options,
        ensure_owned_data_removable,
        platform_uninstall,
        remove_owned_data,
        platform_install,
    )
}

fn uninstall_entrypoints_checked_with<G, U, R, I>(
    options: &InstallOptions,
    guard: G,
    uninstall: U,
    remove: R,
    install: I,
) -> anyhow::Result<()>
where
    G: FnOnce() -> anyhow::Result<()>,
    U: FnOnce(&InstallOptions) -> anyhow::Result<()>,
    R: FnOnce() -> anyhow::Result<()>,
    I: FnOnce(&InstallOptions) -> anyhow::Result<()>,
{
    if options.remove_owned_data {
        guard()?;
    }
    uninstall(options)?;
    if options.remove_owned_data
        && let Err(error) = remove()
    {
        return match install(options) {
            Ok(()) => Err(error).context("托管数据未删除，入口卸载已自动回滚"),
            Err(rollback_error) => Err(error).context(format!(
                "托管数据未删除，且入口自动恢复失败：{rollback_error}"
            )),
        };
    }
    Ok(())
}

pub fn remove_owned_data() -> anyhow::Result<()> {
    let state_dir = crate::paths::default_app_state_dir();
    let codex_home = crate::relay_config::default_codex_home_dir();
    remove_owned_data_at_with(&state_dir, &codex_home, None, |path| {
        std::fs::remove_dir_all(path)
    })
}

fn ensure_owned_data_removable() -> anyhow::Result<()> {
    let state_dir = crate::paths::default_app_state_dir();
    let codex_home = crate::relay_config::default_codex_home_dir();
    ensure_owned_data_removable_at(&state_dir, &codex_home, None)
}

fn ensure_owned_data_removable_at(
    state_dir: &Path,
    codex_home: &Path,
    imagegen_config: Option<&Path>,
) -> anyhow::Result<()> {
    crate::mirror_access::ensure_restored_for_state_removal(codex_home, state_dir)?;
    match imagegen_config {
        Some(config) => crate::imagegen_skill::ensure_restored_for_state_removal_at(
            codex_home, state_dir, config,
        ),
        None => crate::imagegen_skill::ensure_restored_for_state_removal(codex_home, state_dir),
    }
}

fn remove_owned_data_at_with<F>(
    state_dir: &Path,
    codex_home: &Path,
    imagegen_config: Option<&Path>,
    remover: F,
) -> anyhow::Result<()>
where
    F: FnOnce(&Path) -> std::io::Result<()>,
{
    let tombstone = owned_data_tombstone_path(state_dir)?;
    recover_interrupted_owned_data_removal(state_dir, &tombstone)?;
    ensure_owned_data_removable_at(state_dir, codex_home, imagegen_config)?;

    let metadata = match std::fs::symlink_metadata(state_dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("无法读取托管数据目录 {}", state_dir.display()));
        }
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        anyhow::bail!(
            "托管数据路径不是可安全删除的目录，已停止操作：{}",
            state_dir.display()
        );
    }

    std::fs::rename(state_dir, &tombstone).with_context(|| {
        format!(
            "无法暂存托管数据 {} -> {}，未删除任何数据",
            state_dir.display(),
            tombstone.display()
        )
    })?;
    if let Err(error) = remover(&tombstone) {
        return match std::fs::rename(&tombstone, state_dir) {
            Ok(()) => Err(error).with_context(|| {
                format!("删除托管数据失败，剩余数据已恢复到 {}", state_dir.display())
            }),
            Err(rollback_error) => Err(error).with_context(|| {
                format!(
                    "删除托管数据失败，且自动恢复失败：{rollback_error}；剩余数据保留在 {}",
                    tombstone.display()
                )
            }),
        };
    }
    Ok(())
}

fn owned_data_tombstone_path(state_dir: &Path) -> anyhow::Result<PathBuf> {
    let parent = state_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("托管数据目录缺少父目录：{}", state_dir.display()))?;
    let name = state_dir
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("托管数据目录缺少名称：{}", state_dir.display()))?;
    let mut tombstone_name = name.to_os_string();
    tombstone_name.push(".removing");
    Ok(parent.join(tombstone_name))
}

fn recover_interrupted_owned_data_removal(
    state_dir: &Path,
    tombstone: &Path,
) -> anyhow::Result<()> {
    let tombstone_exists = match std::fs::symlink_metadata(tombstone) {
        Ok(metadata) => {
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                anyhow::bail!(
                    "发现异常的托管数据恢复路径，已停止操作：{}",
                    tombstone.display()
                );
            }
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(error)
                .with_context(|| format!("无法检查托管数据恢复路径 {}", tombstone.display()));
        }
    };
    if !tombstone_exists {
        return Ok(());
    }
    match std::fs::symlink_metadata(state_dir) {
        Ok(_) => {
            anyhow::bail!(
                "托管数据目录和恢复目录同时存在，未自动覆盖；请保留 {} 并联系支持。",
                tombstone.display()
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("无法检查托管数据目录 {}", state_dir.display()));
        }
    }
    std::fs::rename(tombstone, state_dir).with_context(|| {
        format!(
            "检测到上次未完成的数据删除，但无法恢复 {} -> {}",
            tombstone.display(),
            state_dir.display()
        )
    })
}

pub fn default_install_root() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        return crate::windows_integration::desktop_dir().or_else(|| {
            directories::UserDirs::new().and_then(|dirs| dirs.desktop_dir().map(PathBuf::from))
        });
    }

    #[cfg(target_os = "macos")]
    {
        let sys_apps = PathBuf::from("/Applications");
        if sys_apps.join(format!("{SILENT_NAME}.app")).exists()
            || sys_apps.join(format!("{MANAGER_NAME}.app")).exists()
        {
            return Some(sys_apps);
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = macos_applications_dir_from_exe(&exe) {
                if is_macos_applications_dir(&dir) {
                    return Some(dir);
                }
            }
        }
        return Some(sys_apps);
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    {
        directories::UserDirs::new().and_then(|dirs| dirs.desktop_dir().map(PathBuf::from))
    }
}

pub fn default_install_root_strategy() -> &'static str {
    if cfg!(windows) {
        "windows-known-folder"
    } else if cfg!(target_os = "macos") {
        "macos-applications"
    } else {
        "user-dirs-desktop"
    }
}

fn platform_install(options: &InstallOptions) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        windows::install_shortcuts(options)
    }

    #[cfg(target_os = "macos")]
    {
        macos::install_app_bundles(options)
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = options;
        anyhow::bail!("当前平台暂不支持安装 mirror+ 入口")
    }
}

fn platform_uninstall(options: &InstallOptions) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        windows::uninstall_shortcuts(options)
    }

    #[cfg(target_os = "macos")]
    {
        macos::uninstall_app_bundles(options)
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = options;
        anyhow::bail!("当前平台暂不支持卸载 mirror+ 入口")
    }
}

fn action_result(result: anyhow::Result<()>, success_message: &str) -> InstallActionResult {
    let state = inspect_entrypoints();
    match result {
        Ok(()) => InstallActionResult {
            status: "ok".to_string(),
            message: success_message.to_string(),
            silent_shortcut: state.silent_shortcut,
            management_shortcut: state.management_shortcut,
        },
        Err(error) => InstallActionResult {
            status: "failed".to_string(),
            message: error.to_string(),
            silent_shortcut: state.silent_shortcut,
            management_shortcut: state.management_shortcut,
        },
    }
}

fn entrypoint_candidates(root: &Option<PathBuf>, manager: bool) -> Vec<PathBuf> {
    let Some(root) = root else {
        return Vec::new();
    };
    let name = if manager { MANAGER_NAME } else { SILENT_NAME };
    if cfg!(windows) {
        vec![root.join(format!("{name}.lnk"))]
    } else if cfg!(target_os = "macos") {
        vec![root.join(format!("{name}.app"))]
    } else {
        vec![root.join(format!("{name}.desktop"))]
    }
}

pub fn option_or_current_exe(value: &Option<PathBuf>, binary: &str) -> PathBuf {
    if let Some(value) = value {
        return value.clone();
    }
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("."));
    companion_binary_path_from_exe(&exe, binary)
}

pub fn companion_binary_path(binary: &str) -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("."));
    companion_binary_path_from_exe(&exe, binary)
}

pub fn companion_binary_path_from_exe(exe: &Path, binary: &str) -> PathBuf {
    let dir = exe.parent().unwrap_or_else(|| Path::new("."));
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    if let Some(bundle_binary) = macos_companion_binary_from_exe(exe, binary) {
        return bundle_binary;
    }
    let same_bundle = dir.join(binary);
    if same_bundle.exists() {
        return same_bundle;
    }
    dir.join(format!("{binary}{suffix}"))
}

fn macos_companion_binary_from_exe(exe: &Path, binary: &str) -> Option<PathBuf> {
    let (applications_dir, app_name) = macos_applications_dir_and_app_name_from_exe(exe)?;
    if binary == SILENT_BINARY {
        if app_name == format!("{SILENT_NAME}.app") {
            return Some(macos_preferred_bundle_binary(exe, SILENT_BINARY));
        }
        let macos = applications_dir
            .join(format!("{SILENT_NAME}.app"))
            .join("Contents")
            .join("MacOS");
        return Some(macos.join(SILENT_BINARY));
    }
    if binary == MANAGER_BINARY {
        if app_name == format!("{MANAGER_NAME}.app") {
            return Some(macos_preferred_bundle_binary(exe, MANAGER_BINARY));
        }
        let macos = applications_dir
            .join(format!("{MANAGER_NAME}.app"))
            .join("Contents")
            .join("MacOS");
        return Some(macos.join(MANAGER_BINARY));
    }
    None
}

fn macos_preferred_bundle_binary(exe: &Path, sidecar_name: &str) -> PathBuf {
    let macos = exe.parent().unwrap_or_else(|| Path::new("."));
    let sidecar = macos.join(sidecar_name);
    if sidecar.exists() {
        return sidecar;
    }
    sidecar
}

#[cfg(target_os = "macos")]
fn macos_applications_dir_from_exe(exe: &Path) -> Option<PathBuf> {
    macos_applications_dir_and_app_name_from_exe(exe).map(|(dir, _)| dir)
}

fn macos_applications_dir_and_app_name_from_exe(exe: &Path) -> Option<(PathBuf, String)> {
    let mut path = exe;
    while let Some(parent) = path.parent() {
        if path.extension().and_then(|extension| extension.to_str()) == Some("app") {
            let app_name = path.file_name()?.to_string_lossy().to_string();
            return Some((parent.to_path_buf(), app_name));
        }
        path = parent;
    }
    None
}

#[cfg(target_os = "macos")]
fn is_macos_applications_dir(path: &Path) -> bool {
    if path == Path::new("/Applications") {
        return true;
    }
    directories::BaseDirs::new()
        .map(|dirs| path == dirs.home_dir().join("Applications"))
        .unwrap_or(false)
}

pub(crate) fn install_root_or_default(options: &InstallOptions) -> PathBuf {
    options
        .install_root
        .clone()
        .or_else(default_install_root)
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::Cell;
    use std::fs;

    fn test_paths() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join(".mirrorplus");
        let home = temp.path().join(".codex");
        let image_config = temp.path().join("imagegen.json");
        (temp, state, home, image_config)
    }

    #[test]
    fn owned_data_removal_refuses_active_access_before_calling_remover() {
        let (_temp, state, home, image_config) = test_paths();
        let settings = state.join("settings.json");
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join("config.toml"), "model_provider = \"openai\"\n").unwrap();
        let discovery = crate::mirror_access::parse_model_discovery(&json!({
            "data": [{"id": "gpt-5.4"}]
        }))
        .unwrap();
        crate::mirror_access::enable_access(
            &home,
            &state,
            &settings,
            "sk-test",
            crate::mirror_access::MirrorAccessMode::MixedApi,
            discovery,
        )
        .unwrap();

        let called = Cell::new(false);
        let error = remove_owned_data_at_with(&state, &home, Some(&image_config), |_| {
            called.set(true);
            Ok(())
        })
        .unwrap_err();

        assert!(error.to_string().contains("仍处于"), "{error:#}");
        assert!(!called.get());
        assert!(state.is_dir());
    }

    #[test]
    fn owned_data_removal_refuses_managed_imagegen_before_calling_remover() {
        let (_temp, state, home, image_config) = test_paths();
        let skill = home.join("skills").join("jingzi-imagegen");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join(".mirror-x-managed.json"), "{}").unwrap();
        fs::create_dir_all(&state).unwrap();

        let called = Cell::new(false);
        let error = remove_owned_data_at_with(&state, &home, Some(&image_config), |_| {
            called.set(true);
            Ok(())
        })
        .unwrap_err();

        assert!(error.to_string().contains("仍由"), "{error:#}");
        assert!(!called.get());
        assert!(state.is_dir());
    }

    #[test]
    fn owned_data_removal_restores_tombstone_when_remover_fails() {
        let (_temp, state, home, image_config) = test_paths();
        fs::create_dir_all(&state).unwrap();
        fs::write(state.join("keep.txt"), "keep").unwrap();

        let error = remove_owned_data_at_with(&state, &home, Some(&image_config), |_| {
            Err(std::io::Error::other("injected removal failure"))
        })
        .unwrap_err();

        assert!(error.to_string().contains("已恢复"), "{error:#}");
        assert_eq!(fs::read_to_string(state.join("keep.txt")).unwrap(), "keep");
        assert!(!owned_data_tombstone_path(&state).unwrap().exists());
    }

    #[test]
    fn owned_data_removal_deletes_only_the_renamed_tombstone() {
        let (_temp, state, home, image_config) = test_paths();
        fs::create_dir_all(&state).unwrap();
        fs::write(state.join("owned.txt"), "owned").unwrap();
        let expected_tombstone = owned_data_tombstone_path(&state).unwrap();

        remove_owned_data_at_with(&state, &home, Some(&image_config), |path| {
            assert_eq!(path, expected_tombstone);
            assert!(!state.exists());
            assert_eq!(fs::read_to_string(path.join("owned.txt")).unwrap(), "owned");
            fs::remove_dir_all(path)
        })
        .unwrap();

        assert!(!state.exists());
        assert!(!expected_tombstone.exists());
    }

    #[test]
    fn interrupted_owned_data_removal_is_restored_before_the_next_operation() {
        let (_temp, state, _home, _image_config) = test_paths();
        let tombstone = owned_data_tombstone_path(&state).unwrap();
        fs::create_dir_all(&tombstone).unwrap();
        fs::write(tombstone.join("recover.txt"), "recover").unwrap();

        recover_interrupted_owned_data_removal(&state, &tombstone).unwrap();

        assert_eq!(
            fs::read_to_string(state.join("recover.txt")).unwrap(),
            "recover"
        );
        assert!(!tombstone.exists());
    }

    #[test]
    fn interrupted_owned_data_removal_never_overwrites_a_new_state_directory() {
        let (_temp, state, _home, _image_config) = test_paths();
        let tombstone = owned_data_tombstone_path(&state).unwrap();
        fs::create_dir_all(&state).unwrap();
        fs::write(state.join("new.txt"), "new").unwrap();
        fs::create_dir_all(&tombstone).unwrap();
        fs::write(tombstone.join("old.txt"), "old").unwrap();

        let error = recover_interrupted_owned_data_removal(&state, &tombstone).unwrap_err();

        assert!(error.to_string().contains("同时存在"), "{error:#}");
        assert_eq!(fs::read_to_string(state.join("new.txt")).unwrap(), "new");
        assert_eq!(
            fs::read_to_string(tombstone.join("old.txt")).unwrap(),
            "old"
        );
    }

    #[test]
    fn owned_data_removal_is_idempotent_when_state_is_absent() {
        let (_temp, state, home, image_config) = test_paths();
        let called = Cell::new(false);

        remove_owned_data_at_with(&state, &home, Some(&image_config), |_| {
            called.set(true);
            Ok(())
        })
        .unwrap();

        assert!(!called.get());
    }

    #[test]
    fn owned_data_guard_failure_prevents_entrypoint_uninstall() {
        let options = InstallOptions {
            remove_owned_data: true,
            ..InstallOptions::default()
        };
        let uninstall_called = Cell::new(false);
        let remove_called = Cell::new(false);
        let install_called = Cell::new(false);

        let error = uninstall_entrypoints_checked_with(
            &options,
            || anyhow::bail!("injected guard failure"),
            |_| {
                uninstall_called.set(true);
                Ok(())
            },
            || {
                remove_called.set(true);
                Ok(())
            },
            |_| {
                install_called.set(true);
                Ok(())
            },
        )
        .unwrap_err();

        assert!(error.to_string().contains("guard failure"), "{error:#}");
        assert!(!uninstall_called.get());
        assert!(!remove_called.get());
        assert!(!install_called.get());
    }

    #[test]
    fn owned_data_removal_failure_reinstalls_entrypoints() {
        let options = InstallOptions {
            remove_owned_data: true,
            ..InstallOptions::default()
        };
        let uninstall_called = Cell::new(false);
        let install_called = Cell::new(false);

        let error = uninstall_entrypoints_checked_with(
            &options,
            || Ok(()),
            |_| {
                uninstall_called.set(true);
                Ok(())
            },
            || anyhow::bail!("injected removal failure"),
            |_| {
                install_called.set(true);
                Ok(())
            },
        )
        .unwrap_err();

        assert!(uninstall_called.get());
        assert!(install_called.get());
        assert!(error.to_string().contains("已自动回滚"), "{error:#}");
    }

    #[test]
    fn owned_data_removal_reports_entrypoint_rollback_failure() {
        let options = InstallOptions {
            remove_owned_data: true,
            ..InstallOptions::default()
        };

        let error = uninstall_entrypoints_checked_with(
            &options,
            || Ok(()),
            |_| Ok(()),
            || anyhow::bail!("injected removal failure"),
            |_| anyhow::bail!("injected reinstall failure"),
        )
        .unwrap_err();

        let message = format!("{error:#}");
        assert!(message.contains("入口自动恢复失败"), "{message}");
        assert!(message.contains("reinstall failure"), "{message}");
    }

    #[test]
    fn entrypoint_only_uninstall_never_touches_owned_data() {
        let options = InstallOptions::default();
        let guard_called = Cell::new(false);
        let remove_called = Cell::new(false);
        let install_called = Cell::new(false);

        uninstall_entrypoints_checked_with(
            &options,
            || {
                guard_called.set(true);
                Ok(())
            },
            |_| Ok(()),
            || {
                remove_called.set(true);
                Ok(())
            },
            |_| {
                install_called.set(true);
                Ok(())
            },
        )
        .unwrap();

        assert!(!guard_called.get());
        assert!(!remove_called.get());
        assert!(!install_called.get());
    }
}
