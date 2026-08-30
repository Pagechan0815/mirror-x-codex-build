use std::path::{Path, PathBuf};

use anyhow::Context;

const CODEX_HOME_ENV: &str = "CODEX_HOME";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexHomeResolution {
    pub path: PathBuf,
    pub environment_configured: bool,
    pub issue: Option<String>,
}

pub fn default_codex_home_dir() -> PathBuf {
    resolve_codex_home().path
}

pub fn resolve_codex_home() -> CodexHomeResolution {
    let Some(raw) = std::env::var_os(CODEX_HOME_ENV) else {
        return CodexHomeResolution {
            path: default_user_codex_home_dir(),
            environment_configured: false,
            issue: None,
        };
    };
    if raw.to_string_lossy().trim().is_empty() {
        return CodexHomeResolution {
            path: default_user_codex_home_dir(),
            environment_configured: true,
            issue: Some(
                "CODEX_HOME 已设置但值为空；已阻止静默回退到用户目录。请删除该环境变量或改为已存在的目录。"
                    .to_string(),
            ),
        };
    }
    let path = PathBuf::from(raw);
    let issue = if !path.is_absolute() {
        Some(format!(
            "CODEX_HOME 必须是绝对目录，当前值为 {}。相对路径会随启动位置变化，已停止接管。",
            path.display()
        ))
    } else if !path.exists() {
        Some(format!(
            "CODEX_HOME 指向不存在的目录 {}。请先创建该目录；工具不会静默改用 C 盘。",
            path.display()
        ))
    } else if !path.is_dir() {
        Some(format!(
            "CODEX_HOME 指向的不是目录：{}。请修正环境变量后重试。",
            path.display()
        ))
    } else {
        None
    };
    CodexHomeResolution {
        path,
        environment_configured: true,
        issue,
    }
}

pub fn validate_codex_home_environment() -> anyhow::Result<()> {
    let resolution = resolve_codex_home();
    match resolution.issue {
        Some(issue) => anyhow::bail!(issue),
        None => Ok(()),
    }
}

/// Returns a stable identity for a concrete Codex home directory.
///
/// Recovery metadata uses this value to ensure a baseline captured from one
/// `CODEX_HOME` can never be restored into another directory.
pub fn codex_home_identity(path: &Path) -> anyhow::Result<String> {
    if !path.is_absolute() {
        anyhow::bail!("CODEX_HOME 必须是绝对目录：{}", path.display());
    }
    let canonical = std::fs::canonicalize(path)
        .with_context(|| format!("无法解析 CODEX_HOME 真实路径：{}", path.display()))?;
    let mut rendered = canonical.to_string_lossy().replace('\\', "/");
    if let Some(without_prefix) = rendered.strip_prefix("//?/UNC/") {
        rendered = format!("//{without_prefix}");
    } else if let Some(without_prefix) = rendered.strip_prefix("//?/") {
        rendered = without_prefix.to_string();
    }
    while rendered.len() > 3 && rendered.ends_with('/') {
        rendered.pop();
    }
    if cfg!(windows) {
        rendered.make_ascii_lowercase();
    }
    Ok(rendered)
}

fn default_user_codex_home_dir() -> PathBuf {
    directories::BaseDirs::new()
        .map(|dirs| dirs.home_dir().join(".codex"))
        .unwrap_or_else(|| PathBuf::from(".codex"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::path::Path;
    use std::sync::Mutex;

    static CODEX_HOME_ENV_LOCK: Mutex<()> = Mutex::new(());

    struct CodexHomeEnvGuard {
        previous: Option<OsString>,
    }

    impl CodexHomeEnvGuard {
        fn set(path: &Path) -> Self {
            let previous = std::env::var_os("CODEX_HOME");
            unsafe {
                std::env::set_var("CODEX_HOME", path);
            }
            Self { previous }
        }

        fn set_raw(value: &str) -> Self {
            let previous = std::env::var_os("CODEX_HOME");
            unsafe {
                std::env::set_var("CODEX_HOME", value);
            }
            Self { previous }
        }
    }

    impl Drop for CodexHomeEnvGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var("CODEX_HOME", value),
                    None => std::env::remove_var("CODEX_HOME"),
                }
            }
        }
    }

    #[test]
    fn default_codex_home_dir_uses_existing_codex_home_env_dir() {
        let _lock = CODEX_HOME_ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let codex_home = temp.path().join("custom-codex-home");
        std::fs::create_dir_all(&codex_home).unwrap();
        let _guard = CodexHomeEnvGuard::set(&codex_home);

        assert_eq!(default_codex_home_dir(), codex_home);
        assert_eq!(crate::relay_config::default_codex_home_dir(), codex_home);
        assert_eq!(crate::codex_sqlite::default_codex_home_dir(), codex_home);
    }

    #[test]
    fn missing_codex_home_env_path_is_preserved_and_reported() {
        let _lock = CODEX_HOME_ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing-codex-home");
        let _guard = CodexHomeEnvGuard::set(&missing);

        assert_eq!(default_codex_home_dir(), missing);
        assert_eq!(crate::relay_config::default_codex_home_dir(), missing);
        assert_eq!(crate::codex_sqlite::default_codex_home_dir(), missing);
        assert!(validate_codex_home_environment().is_err());
    }

    #[test]
    fn empty_codex_home_env_is_reported_instead_of_silently_accepted() {
        let _lock = CODEX_HOME_ENV_LOCK.lock().unwrap();
        let expected = default_user_codex_home_dir();
        let _guard = CodexHomeEnvGuard::set_raw("   ");

        let resolution = resolve_codex_home();
        assert_eq!(resolution.path, expected);
        assert!(resolution.environment_configured);
        assert!(
            resolution
                .issue
                .as_deref()
                .is_some_and(|value| value.contains("为空"))
        );
        assert!(validate_codex_home_environment().is_err());
    }

    #[test]
    fn relative_codex_home_env_is_rejected() {
        let _lock = CODEX_HOME_ENV_LOCK.lock().unwrap();
        let _guard = CodexHomeEnvGuard::set_raw("relative-codex-home");

        let resolution = resolve_codex_home();
        assert_eq!(resolution.path, PathBuf::from("relative-codex-home"));
        assert!(
            resolution
                .issue
                .as_deref()
                .is_some_and(|value| value.contains("绝对目录"))
        );
        assert!(validate_codex_home_environment().is_err());
    }

    #[test]
    fn codex_home_identity_is_stable_for_equivalent_paths() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        std::fs::create_dir_all(&home).unwrap();

        let direct = codex_home_identity(&home).unwrap();
        let dotted = codex_home_identity(&home.join(".")).unwrap();

        assert_eq!(direct, dotted);
    }
}
