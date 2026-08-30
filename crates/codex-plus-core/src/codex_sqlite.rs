use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

const CODEX_SQLITE_HOME_ENV: &str = "CODEX_SQLITE_HOME";
const EXTERNAL_SQLITE_BACKUP_DIR: &str = "external-sqlite";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexSqliteHomeResolution {
    pub path: Option<PathBuf>,
    pub environment_configured: bool,
    pub issue: Option<String>,
}

pub fn default_codex_home_dir() -> PathBuf {
    crate::codex_home::default_codex_home_dir()
}

pub fn resolve_codex_sqlite_home() -> CodexSqliteHomeResolution {
    let Some(raw) = std::env::var_os(CODEX_SQLITE_HOME_ENV) else {
        return CodexSqliteHomeResolution {
            path: None,
            environment_configured: false,
            issue: None,
        };
    };
    if raw.to_string_lossy().trim().is_empty() {
        return CodexSqliteHomeResolution {
            path: None,
            environment_configured: true,
            issue: Some(
                "CODEX_SQLITE_HOME 已设置但值为空。请删除该环境变量或改为已存在的目录。"
                    .to_string(),
            ),
        };
    }
    let path = PathBuf::from(raw);
    let issue = if !path.is_absolute() {
        Some(format!(
            "CODEX_SQLITE_HOME 必须是绝对目录，当前值为 {}。相对路径会随启动位置变化，已停止会话写入。",
            path.display()
        ))
    } else if !path.exists() {
        Some(format!(
            "CODEX_SQLITE_HOME 指向不存在的目录 {}。请先创建目录，避免会话索引被写回其他磁盘。",
            path.display()
        ))
    } else if !path.is_dir() {
        Some(format!(
            "CODEX_SQLITE_HOME 指向的不是目录：{}。请修正环境变量后重试。",
            path.display()
        ))
    } else {
        None
    };
    CodexSqliteHomeResolution {
        path: Some(path),
        environment_configured: true,
        issue,
    }
}

pub fn validate_codex_sqlite_home_environment() -> anyhow::Result<()> {
    match resolve_codex_sqlite_home().issue {
        Some(issue) => anyhow::bail!(issue),
        None => Ok(()),
    }
}

pub fn codex_session_db_path() -> PathBuf {
    codex_session_db_path_from_home(&default_codex_home_dir())
}

pub fn codex_session_db_path_from_home(home: &Path) -> PathBuf {
    let paths = codex_session_db_paths_from_home(home);
    paths
        .iter()
        .find(|path| sqlite_has_table(path, "threads"))
        .cloned()
        .or_else(|| paths.into_iter().next())
        .unwrap_or_else(|| legacy_state_db_path(home))
}

pub fn codex_session_db_paths_from_home(home: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(sqlite_home) = resolve_codex_sqlite_home().path {
        append_unique_paths(&mut paths, sqlite_dir_session_dbs(&sqlite_home));
    }
    append_unique_paths(&mut paths, sqlite_dir_session_dbs(&home.join("sqlite")));
    let legacy = legacy_state_db_path(home);
    if !paths.iter().any(|path| path == &legacy) {
        paths.push(legacy);
    }
    paths
}

/// codex 客户端日志数据库路径（固定文件名）。
pub fn codex_logs_db_path_from_home(home: &Path) -> PathBuf {
    if let Some(sqlite_home) = resolve_codex_sqlite_home().path {
        let external = sqlite_home.join("logs_2.sqlite");
        if external.is_file() {
            return external;
        }
    }
    home.join("logs_2.sqlite")
}

pub fn codex_sqlite_sidecar_paths(db_path: &Path) -> [PathBuf; 3] {
    [
        db_path.to_path_buf(),
        PathBuf::from(format!("{}-wal", db_path.to_string_lossy())),
        PathBuf::from(format!("{}-shm", db_path.to_string_lossy())),
    ]
}

pub fn relative_to_codex_home(home: &Path, path: &Path) -> PathBuf {
    if let Ok(relative) = path.strip_prefix(home) {
        return relative.to_path_buf();
    }
    if let Some(sqlite_home) = resolve_codex_sqlite_home().path
        && let Ok(relative) = path.strip_prefix(sqlite_home)
    {
        return PathBuf::from(EXTERNAL_SQLITE_BACKUP_DIR).join(relative);
    }
    PathBuf::from(EXTERNAL_SQLITE_BACKUP_DIR).join(
        path.file_name()
            .unwrap_or_else(|| OsStr::new("unknown.sqlite")),
    )
}

pub fn path_from_backup_relative(home: &Path, relative: &Path) -> PathBuf {
    if let Ok(external_relative) = relative.strip_prefix(EXTERNAL_SQLITE_BACKUP_DIR)
        && let Some(sqlite_home) = resolve_codex_sqlite_home().path
    {
        return sqlite_home.join(external_relative);
    }
    home.join(relative)
}

fn legacy_state_db_path(home: &Path) -> PathBuf {
    home.join("state_5.sqlite")
}

fn sqlite_dir_session_dbs(sqlite_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(sqlite_dir) else {
        return Vec::new();
    };
    let mut candidates = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter(|path| is_sqlite_candidate(path))
        .filter(|path| has_session_table(path))
        .collect::<Vec<_>>();
    candidates.sort_by_key(|path| {
        (
            path.file_name()
                .map(|name| name != OsStr::new("codex-dev.db"))
                .unwrap_or(true),
            path.file_name().map(|name| name.to_os_string()),
        )
    });
    candidates
}

fn append_unique_paths(target: &mut Vec<PathBuf>, source: Vec<PathBuf>) {
    for path in source {
        if !target.iter().any(|candidate| candidate == &path) {
            target.push(path);
        }
    }
}

fn is_sqlite_candidate(path: &Path) -> bool {
    matches!(
        path.extension().and_then(OsStr::to_str),
        Some("db") | Some("sqlite") | Some("sqlite3")
    )
}

fn has_session_table(path: &Path) -> bool {
    ["threads", "automation_runs", "inbox_items"]
        .iter()
        .any(|table| sqlite_has_table(path, table))
}

fn sqlite_has_table(path: &Path, table: &str) -> bool {
    let Ok(db) = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return false;
    };
    db.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1 LIMIT 1",
        [table],
        |_| Ok(()),
    )
    .is_ok()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SanitizeModelSuffixResult {
    pub scanned: usize,
    pub updated: usize,
}

/// 扫描 codex session 数据库中的 threads 表，把 model 字段里带合法后缀的
/// 记录改写为剥离后缀的 slug，使 codex 模型选择器不再显示带后缀的历史项。
pub fn sanitize_thread_model_suffixes(home: &Path) -> anyhow::Result<SanitizeModelSuffixResult> {
    let mut result = SanitizeModelSuffixResult::default();
    for db_path in codex_session_db_paths_from_home(home) {
        if !db_path.exists() {
            continue;
        }
        let (scanned, updated) = sanitize_thread_model_suffixes_in_db(&db_path)?;
        result.scanned += scanned;
        result.updated += updated;
    }
    Ok(result)
}

/// 同时清理 threads.model 与 logs_2.sqlite 中残留的带后缀模型名。
/// 返回的 scanned/updated 只统计 threads 表的改动数量；日志清理仅作为副作用。
pub fn sanitize_historical_model_suffixes(
    home: &Path,
) -> anyhow::Result<SanitizeModelSuffixResult> {
    let result = sanitize_thread_model_suffixes(home)?;
    if let Err(error) = sanitize_logs_model_suffixes(home) {
        // 日志清理失败不应阻断启动流程，仅记录诊断日志。
        let _ = crate::diagnostic_log::append_diagnostic_log(
            "codex_sqlite.sanitize_logs_model_suffixes_failed",
            serde_json::json!({
                "error": error.to_string(),
            }),
        );
    }
    Ok(result)
}

fn sanitize_thread_model_suffixes_in_db(db_path: &Path) -> anyhow::Result<(usize, usize)> {
    let mut conn = Connection::open(db_path)?;
    let tx = conn.transaction()?;
    let has_model = tx
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'threads' LIMIT 1",
            [],
            |_| Ok(()),
        )
        .is_ok()
        && tx
            .query_row(
                "SELECT 1 FROM pragma_table_info('threads') WHERE name = 'model' LIMIT 1",
                [],
                |_| Ok(()),
            )
            .is_ok();
    if !has_model {
        return Ok((0, 0));
    }

    let mut stmt = tx.prepare("SELECT id, model FROM threads WHERE model LIKE '%[%'")?;
    let rows: Vec<(String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .filter_map(Result::ok)
        .collect();
    drop(stmt);

    let scanned = rows.len();
    let mut updated = 0;
    for (id, model) in rows {
        let (slug, suffix_window) = crate::model_suffix::parse_model_suffix(&model);
        if suffix_window.is_some() && slug != model {
            tx.execute("UPDATE threads SET model = ?1 WHERE id = ?2", [&slug, &id])?;
            updated += 1;
        }
    }
    tx.commit()?;
    Ok((scanned, updated))
}

/// 清理 logs_2.sqlite 中 feedback_log_body 字段里包含模型后缀的日志。
/// 这些日志只是历史记录，不会直接影响模型选择器，但清理后可避免
/// 诊断/遥测中继续出现已废弃的带后缀模型名。
fn sanitize_logs_model_suffixes(home: &Path) -> anyhow::Result<()> {
    let db_path = codex_logs_db_path_from_home(home);
    if !db_path.exists() {
        return Ok(());
    }
    let mut conn = Connection::open(&db_path)?;
    let has_table = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'logs' LIMIT 1",
            [],
            |_| Ok(()),
        )
        .is_ok();
    if !has_table {
        return Ok(());
    }
    let has_body = conn
        .query_row(
            "SELECT 1 FROM pragma_table_info('logs') WHERE name = 'feedback_log_body' LIMIT 1",
            [],
            |_| Ok(()),
        )
        .is_ok();
    if !has_body {
        return Ok(());
    }
    // 用保守模式匹配：包含 '[' 且以 ']%' 或包含 '[1M]' 等常见后缀。
    // 这里只替换明确符合 parse_model_suffix 规则的模型名，避免误改无关日志文本。
    let mut stmt = conn
        .prepare("SELECT rowid, feedback_log_body FROM logs WHERE feedback_log_body LIKE '%[%'")?;
    let rows: Vec<(i64, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .filter_map(Result::ok)
        .collect();
    drop(stmt);

    let tx = conn.transaction()?;
    let mut update = tx.prepare("UPDATE logs SET feedback_log_body = ?1 WHERE rowid = ?2")?;
    for (rowid, body) in rows {
        let sanitized = sanitize_model_suffixes_in_text(&body);
        if sanitized != body {
            update.execute([&sanitized, &rowid.to_string()])?;
        }
    }
    drop(update);
    tx.commit()?;
    Ok(())
}

/// 在一段文本中把所有符合 "slug[<number>K|M]" 格式的模型窗口后缀替换为纯 slug。
/// 只处理明确看起来像窗口大小后缀的形式（如 [1M]、[200K]），避免误改普通数组下标。
pub(crate) fn sanitize_model_suffixes_in_text(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut result = String::with_capacity(text.len());
    let mut last = 0; // 上次已复制到 result 的字符索引
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '[' {
            // 向后找窗口后缀：数字 + K/M（大小写均可）
            let digits_start = i + 1;
            let mut j = digits_start;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            let has_digits = j > digits_start;
            let unit_seen = j < chars.len() && matches!(chars[j], 'K' | 'k' | 'M' | 'm');
            if unit_seen {
                j += 1;
            }
            if has_digits && unit_seen && j < chars.len() && chars[j] == ']' {
                // 向前找 slug
                let mut slug_start = i;
                while slug_start > 0 && is_model_id_char(chars[slug_start - 1]) {
                    slug_start -= 1;
                }
                if slug_start < i {
                    result.extend(chars[last..slug_start].iter());
                    result.extend(chars[slug_start..i].iter());
                    last = j + 1;
                    i = j + 1;
                    continue;
                }
            }
        }
        i += 1;
    }
    result.extend(chars[last..].iter());
    result
}

fn is_model_id_char(c: char) -> bool {
    c.is_alphanumeric() || c == '.' || c == '/' || c == '_' || c == '-' || c == ':'
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::sync::Mutex;

    static CODEX_SQLITE_HOME_ENV_LOCK: Mutex<()> = Mutex::new(());

    struct SqliteHomeEnvGuard {
        previous: Option<OsString>,
    }

    impl SqliteHomeEnvGuard {
        fn set(path: &Path) -> Self {
            let previous = std::env::var_os(CODEX_SQLITE_HOME_ENV);
            unsafe { std::env::set_var(CODEX_SQLITE_HOME_ENV, path) };
            Self { previous }
        }
    }

    impl Drop for SqliteHomeEnvGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var(CODEX_SQLITE_HOME_ENV, value),
                    None => std::env::remove_var(CODEX_SQLITE_HOME_ENV),
                }
            }
        }
    }

    #[test]
    fn strips_trailing_suffix_from_model_names() {
        assert_eq!(
            sanitize_model_suffixes_in_text("model=deepseek-v4-flash[1M]"),
            "model=deepseek-v4-flash"
        );
        assert_eq!(
            sanitize_model_suffixes_in_text("nvidia/nemotron-3-super-120b-a12b:free[1M]"),
            "nvidia/nemotron-3-super-120b-a12b:free"
        );
        assert_eq!(sanitize_model_suffixes_in_text("glm-5.2[1M]"), "glm-5.2");
    }

    #[test]
    fn leaves_non_model_brackets_unchanged() {
        assert_eq!(
            sanitize_model_suffixes_in_text("array[0] and foo[bar]"),
            "array[0] and foo[bar]"
        );
        assert_eq!(
            sanitize_model_suffixes_in_text("some [placeholder] text"),
            "some [placeholder] text"
        );
    }

    #[test]
    fn leaves_text_without_brackets_unchanged() {
        let text = "no suffix here";
        assert_eq!(sanitize_model_suffixes_in_text(text), text);
    }

    #[test]
    fn discovers_session_database_from_codex_sqlite_home() {
        let _lock = CODEX_SQLITE_HOME_ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let sqlite_home = temp.path().join("sqlite-home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&sqlite_home).unwrap();
        let db_path = sqlite_home.join("codex.db");
        let db = Connection::open(&db_path).unwrap();
        db.execute("CREATE TABLE threads (id TEXT PRIMARY KEY)", [])
            .unwrap();
        drop(db);
        let _guard = SqliteHomeEnvGuard::set(&sqlite_home);

        let paths = codex_session_db_paths_from_home(&home);
        assert_eq!(paths.first(), Some(&db_path));
        assert!(paths.contains(&home.join("state_5.sqlite")));
        assert_eq!(
            relative_to_codex_home(&home, &db_path),
            PathBuf::from(EXTERNAL_SQLITE_BACKUP_DIR).join("codex.db")
        );
        assert_eq!(
            path_from_backup_relative(
                &home,
                &PathBuf::from(EXTERNAL_SQLITE_BACKUP_DIR).join("codex.db")
            ),
            db_path
        );
    }

    #[test]
    fn missing_codex_sqlite_home_is_reported() {
        let _lock = CODEX_SQLITE_HOME_ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing-sqlite-home");
        let _guard = SqliteHomeEnvGuard::set(&missing);

        let resolution = resolve_codex_sqlite_home();
        assert_eq!(resolution.path.as_deref(), Some(missing.as_path()));
        assert!(resolution.issue.is_some());
        assert!(validate_codex_sqlite_home_environment().is_err());
    }

    #[test]
    fn relative_codex_sqlite_home_is_rejected() {
        let _lock = CODEX_SQLITE_HOME_ENV_LOCK.lock().unwrap();
        let _guard = SqliteHomeEnvGuard::set(Path::new("relative-sqlite-home"));

        let resolution = resolve_codex_sqlite_home();
        assert_eq!(
            resolution.path.as_deref(),
            Some(Path::new("relative-sqlite-home"))
        );
        assert!(
            resolution
                .issue
                .as_deref()
                .is_some_and(|value| value.contains("绝对目录"))
        );
        assert!(validate_codex_sqlite_home_environment().is_err());
    }
}
