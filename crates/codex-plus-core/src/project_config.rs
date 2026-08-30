use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use toml_edit::DocumentMut;

const MAX_PROJECT_CONFIG_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectConfigOverride {
    pub project_path: String,
    pub config_path: String,
    pub model_provider: Option<String>,
    pub model: Option<String>,
    pub profile: Option<String>,
    pub changes_active_provider: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectConfigScan {
    pub scanned_projects: usize,
    pub overrides: Vec<ProjectConfigOverride>,
    pub unreadable_configs: Vec<String>,
}

pub fn scan_recent_project_configs(
    codex_home: &Path,
    expected_provider: &str,
    limit: usize,
) -> ProjectConfigScan {
    scan_project_paths(
        recent_project_paths(codex_home, limit.max(1)),
        expected_provider,
    )
}

pub fn active_provider_from_home(codex_home: &Path) -> anyhow::Result<String> {
    let config_path = codex_home.join("config.toml");
    match fs::metadata(&config_path) {
        Ok(metadata) if metadata.len() > MAX_PROJECT_CONFIG_BYTES => {
            anyhow::bail!("Codex config.toml 超过启动检查允许的 1 MB 上限");
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok("openai".to_string());
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("无法检查 Codex 配置 {}", config_path.display()));
        }
    }
    let contents = match fs::read_to_string(&config_path) {
        Ok(contents) => contents,
        Err(error) => {
            return Err(error)
                .with_context(|| format!("无法读取 Codex 配置 {}", config_path.display()));
        }
    };
    let document = contents
        .parse::<DocumentMut>()
        .with_context(|| format!("无法解析 Codex 配置 {}", config_path.display()))?;
    Ok(string_value(&document, "model_provider").unwrap_or_else(|| "openai".to_string()))
}

fn recent_project_paths(codex_home: &Path, limit: usize) -> Vec<PathBuf> {
    let mut projects = Vec::new();
    let mut seen = HashSet::new();
    for db_path in crate::codex_sqlite::codex_session_db_paths_from_home(codex_home) {
        if projects.len() >= limit || !db_path.is_file() {
            continue;
        }
        let Ok(db) = Connection::open_with_flags(
            &db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) else {
            continue;
        };
        let _ = db.busy_timeout(std::time::Duration::from_millis(250));
        let columns = table_columns(&db, "threads");
        if !columns.contains("cwd") {
            continue;
        }
        let order = if columns.contains("updated_at") {
            "updated_at"
        } else if columns.contains("created_at") {
            "created_at"
        } else {
            "rowid"
        };
        let sql = format!(
            "SELECT cwd FROM threads WHERE cwd IS NOT NULL AND TRIM(cwd) != '' ORDER BY {order} DESC LIMIT ?1"
        );
        let Ok(mut statement) = db.prepare(&sql) else {
            continue;
        };
        let Ok(rows) = statement.query_map([limit as i64], |row| row.get::<_, String>(0)) else {
            continue;
        };
        for cwd in rows.flatten() {
            let cwd = cwd.trim();
            if cwd.is_empty() {
                continue;
            }
            let path = PathBuf::from(cwd);
            if seen.insert(path.clone()) {
                projects.push(path);
                if projects.len() >= limit {
                    break;
                }
            }
        }
    }
    projects
}

fn table_columns(db: &Connection, table: &str) -> HashSet<String> {
    let Ok(mut statement) = db.prepare(&format!("PRAGMA table_info({table})")) else {
        return HashSet::new();
    };
    let Ok(rows) = statement.query_map([], |row| row.get::<_, String>(1)) else {
        return HashSet::new();
    };
    rows.flatten().collect()
}

fn scan_project_paths(projects: Vec<PathBuf>, expected_provider: &str) -> ProjectConfigScan {
    let mut scan = ProjectConfigScan {
        scanned_projects: projects.len(),
        ..ProjectConfigScan::default()
    };
    for project in projects {
        let config_path = project.join(".codex").join("config.toml");
        if !config_path.is_file() {
            continue;
        }
        match inspect_project_config(&project, &config_path, expected_provider) {
            Ok(Some(override_config)) => scan.overrides.push(override_config),
            Ok(None) => {}
            Err(_) => scan
                .unreadable_configs
                .push(config_path.to_string_lossy().to_string()),
        }
    }
    scan
}

fn inspect_project_config(
    project: &Path,
    config_path: &Path,
    expected_provider: &str,
) -> anyhow::Result<Option<ProjectConfigOverride>> {
    if fs::metadata(config_path)?.len() > MAX_PROJECT_CONFIG_BYTES {
        anyhow::bail!("project config is larger than the inspection limit");
    }
    let contents = fs::read_to_string(config_path)
        .with_context(|| format!("无法读取项目配置 {}", config_path.display()))?;
    let document = contents
        .parse::<DocumentMut>()
        .with_context(|| format!("无法解析项目配置 {}", config_path.display()))?;
    let model_provider = string_value(&document, "model_provider");
    let model = string_value(&document, "model");
    let profile = string_value(&document, "profile");
    let expected_provider_redefined = document
        .get("model_providers")
        .and_then(|item| item.as_table())
        .is_some_and(|providers| providers.contains_key(expected_provider));
    let provider_conflicts = model_provider
        .as_deref()
        .is_some_and(|provider| provider != expected_provider);
    if !provider_conflicts && model.is_none() && profile.is_none() && !expected_provider_redefined {
        return Ok(None);
    }
    let mut details = Vec::new();
    if let Some(provider) = &model_provider
        && provider_conflicts
    {
        details.push(format!(
            "项目写入 Provider {provider}（Codex 项目配置层会忽略此键）"
        ));
    }
    if let Some(model) = &model {
        details.push(format!("项目固定模型 {model}"));
    }
    if let Some(profile) = &profile {
        details.push(format!(
            "项目写入 Profile {profile}（Codex 项目配置层会忽略此键）"
        ));
    }
    if expected_provider_redefined {
        details.push(format!(
            "项目重新定义 Provider {expected_provider}（Codex 项目配置层会忽略该表）"
        ));
    }
    // Codex ignores provider selection, provider definitions, and profiles in
    // project-scoped config. Keep these entries visible for diagnostics only.
    let changes_active_provider = false;
    Ok(Some(ProjectConfigOverride {
        project_path: project.to_string_lossy().to_string(),
        config_path: config_path.to_string_lossy().to_string(),
        model_provider,
        model,
        profile,
        changes_active_provider,
        detail: details.join("；"),
    }))
}

fn string_value(document: &DocumentMut, key: &str) -> Option<String> {
    document
        .get(key)
        .and_then(|item| item.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_project_provider_model_and_profile_overrides() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        fs::create_dir_all(project.join(".codex")).unwrap();
        fs::write(
            project.join(".codex/config.toml"),
            "model_provider = \"official\"\nmodel = \"gpt-5\"\nprofile = \"work\"\n",
        )
        .unwrap();

        let scan = scan_project_paths(vec![project], "mirrorplus");
        assert_eq!(scan.scanned_projects, 1);
        assert_eq!(scan.overrides.len(), 1);
        assert_eq!(
            scan.overrides[0].model_provider.as_deref(),
            Some("official")
        );
        assert!(!scan.overrides[0].changes_active_provider);
        assert!(scan.overrides[0].detail.contains("固定模型"));
        assert!(scan.overrides[0].detail.contains("项目配置层会忽略"));
    }

    #[test]
    fn ignores_project_config_without_routing_overrides() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        fs::create_dir_all(project.join(".codex")).unwrap();
        fs::write(
            project.join(".codex/config.toml"),
            "approval_policy = \"on-request\"\n",
        )
        .unwrap();

        let scan = scan_project_paths(vec![project], "mirrorplus");
        assert!(scan.overrides.is_empty());
        assert!(scan.unreadable_configs.is_empty());
    }

    #[test]
    fn fixed_model_is_reported_without_claiming_provider_change() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        fs::create_dir_all(project.join(".codex")).unwrap();
        fs::write(project.join(".codex/config.toml"), "model = \"gpt-5\"\n").unwrap();

        let scan = scan_project_paths(vec![project], "mirrorplus");
        assert_eq!(scan.overrides.len(), 1);
        assert!(!scan.overrides[0].changes_active_provider);
    }

    #[test]
    fn project_provider_table_is_diagnostic_only() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        fs::create_dir_all(project.join(".codex")).unwrap();
        fs::write(
            project.join(".codex/config.toml"),
            "model_provider = \"official\"\nprofile = \"work\"\n[model_providers.mirrorplus]\nbase_url = \"https://ignored.example\"\n",
        )
        .unwrap();

        let scan = scan_project_paths(vec![project], "mirrorplus");
        assert_eq!(scan.overrides.len(), 1);
        assert!(!scan.overrides[0].changes_active_provider);
        assert!(scan.overrides[0].detail.contains("忽略该表"));
    }

    #[test]
    fn reads_active_provider_from_home_without_mutating_missing_config() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(active_provider_from_home(temp.path()).unwrap(), "openai");
        assert!(!temp.path().join("config.toml").exists());

        fs::write(
            temp.path().join("config.toml"),
            "model_provider = \"mirrorplus\"\n",
        )
        .unwrap();
        assert_eq!(
            active_provider_from_home(temp.path()).unwrap(),
            "mirrorplus"
        );
    }
}
