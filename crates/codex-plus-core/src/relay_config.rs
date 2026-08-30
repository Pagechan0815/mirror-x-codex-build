use anyhow::Context;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use toml_edit::{DocumentMut, Item, Table, TableLike};

use crate::settings::{RelayContextSelection, RelayProfile, RelayProtocol};

const RELAY_PROVIDER: &str = "custom";
const LEGACY_RELAY_PROVIDERS: &[&str] = &["mirrorplus", "CodexPlusPlus", "CodexPP"];
const CC_SWITCH_MODEL_CATALOG_FILENAME: &str = "cc-switch-model-catalog.json";
const CHAT_UPSTREAM_BASE_URL_KEY: &str = "codex_plus_chat_base_url";
const PROVIDER_SPECIFIC_COMMON_ROOT_KEYS: &[&str] = &[
    "model",
    "model_provider",
    "base_url",
    "openai_base_url",
    "chatgpt_base_url",
    "model_catalog_json",
    "OPENAI_API_KEY",
    CHAT_UPSTREAM_BASE_URL_KEY,
];
const RESERVED_MODEL_PROVIDER_IDS: &[&str] = &[
    "amazon-bedrock",
    "openai",
    "ollama",
    "lmstudio",
    "oss",
    "ollama-chat",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatGptAuthStatus {
    pub authenticated: bool,
    pub source: String,
    pub account_label: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayConfigStatus {
    pub configured: bool,
    pub requires_openai_auth: bool,
    pub has_bearer_token: bool,
    pub config_path: String,
    pub state_unreadable: bool,
    pub state_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayStatus {
    pub authenticated: bool,
    pub auth_source: String,
    pub account_label: Option<String>,
    pub config_path: String,
    pub configured: bool,
    pub requires_openai_auth: bool,
    pub has_bearer_token: bool,
    pub state_unreadable: bool,
    pub state_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayApplyResult {
    pub config_path: String,
    pub backup_path: Option<String>,
    pub configured: bool,
}

#[derive(Debug, Clone)]
pub struct RelayLiveSnapshot {
    files: Vec<RelayLiveSnapshotFile>,
}

#[derive(Debug, Clone)]
struct RelayLiveSnapshotFile {
    path: PathBuf,
    contents: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayProfileTestResult {
    pub http_status: u16,
    pub endpoint: String,
    pub response_preview: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexContextEntry {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub summary: String,
    pub toml_body: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexContextEntries {
    pub mcp_servers: Vec<CodexContextEntry>,
    pub skills: Vec<CodexContextEntry>,
    pub plugins: Vec<CodexContextEntry>,
}

pub fn default_codex_home_dir() -> PathBuf {
    crate::codex_home::default_codex_home_dir()
}

pub fn capture_relay_live_snapshot(
    home: &Path,
    profiles: &[RelayProfile],
) -> anyhow::Result<RelayLiveSnapshot> {
    let mut paths = vec![home.join("auth.json")];
    let mut catalog_paths = profiles
        .iter()
        .map(|profile| {
            home.join("model-catalogs")
                .join(format!("{}.json", sanitize_catalog_filename(&profile.id)))
        })
        .collect::<Vec<_>>();
    catalog_paths.sort();
    catalog_paths.dedup();
    paths.extend(catalog_paths);
    // config.toml is the commit pointer for auth and catalog state, so restore it last.
    paths.push(home.join("config.toml"));

    let files = paths
        .into_iter()
        .map(|path| {
            let contents = read_optional_bytes(&path)?;
            Ok(RelayLiveSnapshotFile { path, contents })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(RelayLiveSnapshot { files })
}

pub fn restore_relay_live_snapshot(snapshot: &RelayLiveSnapshot) -> anyhow::Result<()> {
    let mut failures = Vec::new();
    for file in &snapshot.files {
        if let Err(error) = restore_optional_file(&file.path, file.contents.as_deref()) {
            failures.push(format!("{}: {error:#}", file.path.display()));
        }
    }
    for file in &snapshot.files {
        match read_optional_bytes(&file.path) {
            Ok(contents) if contents == file.contents => {}
            Ok(_) => failures.push(format!("{}: 恢复后内容校验不一致", file.path.display())),
            Err(error) => failures.push(format!(
                "{}: 恢复后无法读取：{error:#}",
                file.path.display()
            )),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        anyhow::bail!("Codex live 文件恢复不完整：{}", failures.join("；"))
    }
}

pub fn relay_profile_from_live_for_probe(
    home: &Path,
    profile: &RelayProfile,
) -> anyhow::Result<RelayProfile> {
    let mut persisted = profile.clone();
    persisted.config_contents = read_optional_text(&home.join("config.toml"))?;
    persisted.auth_contents = read_optional_text(&home.join("auth.json"))?;
    // A post-write probe must use only values reconstructed from the files that
    // Codex will read. Retaining the in-memory fields would let a missing or
    // malformed persisted endpoint/key silently fall back to the pre-write
    // profile and produce a false-positive verification.
    persisted.base_url.clear();
    persisted.upstream_base_url.clear();
    persisted.api_key.clear();
    persisted.base_url = relay_profile_base_url(&persisted);
    persisted.api_key = relay_profile_api_key(&persisted);
    Ok(persisted)
}

pub fn default_relay_status() -> RelayStatus {
    relay_status_from_home(&default_codex_home_dir())
}

pub fn set_codex_goals_feature_in_home(home: &Path, enabled: bool) -> anyhow::Result<()> {
    std::fs::create_dir_all(home)?;
    let config_path = home.join("config.toml");
    let existing = read_optional_text(&config_path)?;
    let mut doc = parse_toml_document(&existing)?;
    if enabled {
        let features = table_mut_or_insert(&mut doc, "features")?;
        features["goals"] = toml_edit::value(true);
    } else if let Some(features) = table_mut_if_exists(&mut doc, "features") {
        features.remove("goals");
        if features.is_empty() {
            doc.as_table_mut().remove("features");
        }
    }
    let updated = ensure_trailing_newline(doc.to_string());
    crate::settings::atomic_write(&config_path, updated.as_bytes())
}

fn table_mut_or_insert<'a>(doc: &'a mut DocumentMut, key: &str) -> anyhow::Result<&'a mut Table> {
    if !doc.as_table().contains_key(key) {
        doc[key] = toml_edit::table();
    }
    if doc.get(key).and_then(Item::as_table).is_none() {
        doc[key] = toml_edit::table();
    }
    doc.get_mut(key)
        .and_then(Item::as_table_mut)
        .ok_or_else(|| anyhow::anyhow!("{key} 必须是 TOML table"))
}

fn table_mut_if_exists<'a>(doc: &'a mut DocumentMut, key: &str) -> Option<&'a mut Table> {
    doc.get_mut(key).and_then(Item::as_table_mut)
}

pub fn relay_status_from_home(home: &Path) -> RelayStatus {
    let auth = chatgpt_auth_status_from_home(home);
    let config = relay_config_status_from_home(home);
    RelayStatus {
        authenticated: auth.authenticated,
        auth_source: auth.source,
        account_label: auth.account_label,
        config_path: config.config_path,
        configured: config.configured,
        requires_openai_auth: config.requires_openai_auth,
        has_bearer_token: config.has_bearer_token,
        state_unreadable: config.state_unreadable,
        state_error: config.state_error,
    }
}

pub fn chatgpt_auth_status_from_home(home: &Path) -> ChatGptAuthStatus {
    let auth_path = home.join("auth.json");
    if let Some(account_label) = auth_json_chatgpt_account_label(&auth_path) {
        return ChatGptAuthStatus {
            authenticated: true,
            source: auth_path.to_string_lossy().to_string(),
            account_label,
            message: "已通过 auth.json 和 config.toml 检测到 ChatGPT 登录。".to_string(),
        };
    }

    ChatGptAuthStatus {
        authenticated: false,
        source: String::new(),
        account_label: None,
        message: "未检测到 ChatGPT 登录账号。".to_string(),
    }
}

pub fn relay_config_status_from_home(home: &Path) -> RelayConfigStatus {
    let config_path = home.join("config.toml");
    match try_relay_config_status_from_home(home) {
        Ok(status) => status,
        Err(error) => RelayConfigStatus {
            configured: false,
            requires_openai_auth: false,
            has_bearer_token: false,
            config_path: config_path.to_string_lossy().to_string(),
            state_unreadable: true,
            state_error: Some(error.to_string()),
        },
    }
}

pub fn try_relay_config_status_from_home(home: &Path) -> anyhow::Result<RelayConfigStatus> {
    let config_path = home.join("config.toml");
    let auth_path = home.join("auth.json");
    let config_contents = read_optional_text_for_status(&config_path, "config.toml")?;
    let auth_contents = read_optional_text_for_status(&auth_path, "auth.json")?;
    let config_doc = parse_toml_document(config_contents.as_deref().unwrap_or_default())
        .map_err(|_| anyhow::anyhow!("config.toml 不是有效 TOML，无法确认当前供应商状态。"))?;
    let auth = match auth_contents.as_deref() {
        Some(contents) if !contents.trim().is_empty() => {
            let value = serde_json::from_str::<Value>(contents).map_err(|_| {
                anyhow::anyhow!("auth.json 不是有效 JSON，无法确认当前登录或 API Key 状态。")
            })?;
            if !value.is_object() {
                anyhow::bail!("auth.json 必须是 JSON 对象，无法确认当前登录或 API Key 状态。");
            }
            Some(value)
        }
        Some(_) | None => None,
    };

    let root_provider = match config_doc.get("model_provider") {
        Some(item) => Some(
            item.as_str()
                .ok_or_else(|| anyhow::anyhow!("config.toml 的 model_provider 必须是字符串。"))?
                .trim()
                .to_string(),
        ),
        None => None,
    };
    let effective_provider = effective_model_provider(&config_doc, root_provider.as_deref())?;
    let provider = match effective_provider
        .as_deref()
        .filter(|provider| !provider.is_empty())
    {
        Some(provider_id) => relay_status_provider_table(&config_doc, provider_id)?,
        None => None,
    };
    let requires_openai_auth = relay_status_optional_bool(
        provider,
        "requires_openai_auth",
        "config.toml 的 requires_openai_auth 必须是布尔值。",
    )?
    .unwrap_or(false);
    let has_bearer_token = relay_status_optional_string(
        provider,
        "experimental_bearer_token",
        "config.toml 的 experimental_bearer_token 必须是字符串。",
    )?
    .is_some_and(|value| !value.trim().is_empty());
    let has_base_url = relay_status_optional_string(
        provider,
        "base_url",
        "config.toml 的 base_url 必须是字符串。",
    )?
    .is_some_and(|value| !value.trim().is_empty());
    let has_auth_api_key = match auth.as_ref().and_then(Value::as_object) {
        Some(object) => match object.get("OPENAI_API_KEY") {
            Some(value) => !value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("auth.json 的 OPENAI_API_KEY 必须是字符串。"))?
                .trim()
                .is_empty(),
            None => false,
        },
        None => false,
    };

    Ok(RelayConfigStatus {
        configured: effective_provider
            .as_deref()
            .is_some_and(|provider| !provider.is_empty())
            && (has_bearer_token || has_auth_api_key)
            && has_base_url,
        requires_openai_auth,
        has_bearer_token,
        config_path: config_path.to_string_lossy().to_string(),
        state_unreadable: false,
        state_error: None,
    })
}

pub fn effective_model_provider_from_home(home: &Path) -> anyhow::Result<String> {
    let config_path = home.join("config.toml");
    let config_contents = read_optional_text_for_status(&config_path, "config.toml")?;
    let config_doc = parse_toml_document(config_contents.as_deref().unwrap_or_default())
        .map_err(|_| anyhow::anyhow!("config.toml 不是有效 TOML，无法确认当前供应商。"))?;
    let root_provider = config_doc
        .get("model_provider")
        .map(|item| {
            item.as_str()
                .ok_or_else(|| anyhow::anyhow!("config.toml 的 model_provider 必须是字符串。"))
                .map(str::trim)
        })
        .transpose()?;
    Ok(effective_model_provider(&config_doc, root_provider)?
        .unwrap_or_default()
        .trim()
        .to_string())
}

fn effective_model_provider(
    doc: &DocumentMut,
    root_provider: Option<&str>,
) -> anyhow::Result<Option<String>> {
    let Some(profile_name) = doc.get("profile") else {
        return Ok(root_provider.map(str::to_string));
    };
    let profile_name = profile_name
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("config.toml 的 profile 必须是字符串。"))?
        .trim();
    if profile_name.is_empty() {
        return Ok(root_provider.map(str::to_string));
    }
    let Some(profile) = doc
        .get("profiles")
        .and_then(Item::as_table_like)
        .and_then(|profiles| profiles.get(profile_name))
        .and_then(Item::as_table_like)
    else {
        return Ok(root_provider.map(str::to_string));
    };
    match profile.get("model_provider") {
        Some(item) => item
            .as_str()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "config.toml 的 profiles.{profile_name}.model_provider 必须是字符串。"
                )
            })
            .map(|provider| Some(provider.trim().to_string())),
        None => Ok(root_provider.map(str::to_string)),
    }
}

fn read_optional_text_for_status(path: &Path, label: &str) -> anyhow::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(anyhow::anyhow!("无法读取 {label}：{error}")),
    }
}

fn relay_status_provider_table<'a>(
    doc: &'a DocumentMut,
    provider_id: &str,
) -> anyhow::Result<Option<&'a dyn TableLike>> {
    let Some(providers_item) = doc.get("model_providers") else {
        return Ok(None);
    };
    let providers = providers_item
        .as_table_like()
        .ok_or_else(|| anyhow::anyhow!("config.toml 的 model_providers 必须是 table。"))?;
    let Some(provider_item) = providers.get(provider_id) else {
        return Ok(None);
    };
    provider_item
        .as_table_like()
        .map(Some)
        .ok_or_else(|| anyhow::anyhow!("config.toml 的当前 model provider 必须是 table。"))
}

fn relay_status_optional_bool(
    provider: Option<&dyn TableLike>,
    key: &str,
    invalid_message: &str,
) -> anyhow::Result<Option<bool>> {
    let Some(item) = provider.and_then(|table| table.get(key)) else {
        return Ok(None);
    };
    item.as_bool()
        .map(Some)
        .ok_or_else(|| anyhow::anyhow!(invalid_message.to_string()))
}

fn relay_status_optional_string<'a>(
    provider: Option<&'a dyn TableLike>,
    key: &str,
    invalid_message: &str,
) -> anyhow::Result<Option<&'a str>> {
    let Some(item) = provider.and_then(|table| table.get(key)) else {
        return Ok(None);
    };
    item.as_str()
        .map(Some)
        .ok_or_else(|| anyhow::anyhow!(invalid_message.to_string()))
}

pub fn apply_relay_config_to_home(
    home: &Path,
    base_url: &str,
    bearer_token: &str,
) -> anyhow::Result<RelayApplyResult> {
    apply_relay_config_to_home_with_protocol(
        home,
        base_url,
        bearer_token,
        RelayProtocol::Responses,
        crate::protocol_proxy::DEFAULT_PROTOCOL_PROXY_PORT,
    )
}

pub fn apply_relay_config_to_home_with_protocol(
    home: &Path,
    base_url: &str,
    bearer_token: &str,
    protocol: RelayProtocol,
    proxy_port: u16,
) -> anyhow::Result<RelayApplyResult> {
    let base_url = base_url.trim();
    if base_url.is_empty() {
        anyhow::bail!("中转 Base URL 不能为空");
    }
    let bearer_token = bearer_token.trim();
    if bearer_token.is_empty() {
        anyhow::bail!("中转 Key 不能为空");
    }
    let codex_base_url = codex_base_url_for_protocol(base_url, protocol, proxy_port);
    let updated = upsert_model_provider_config("", &codex_base_url, bearer_token, true)?;
    let auth_contents = serde_json::to_string_pretty(&json!({
        "OPENAI_API_KEY": bearer_token
    }))?;
    let backup_path =
        write_codex_live_atomic(home, Some(&updated), Some(auth_contents.as_bytes()), false)?;
    let status = try_relay_config_status_from_home(home)?;
    Ok(RelayApplyResult {
        config_path: status.config_path,
        backup_path,
        configured: status.configured,
    })
}

pub fn apply_pure_api_config_to_home(
    home: &Path,
    base_url: &str,
    bearer_token: &str,
) -> anyhow::Result<RelayApplyResult> {
    apply_pure_api_config_to_home_with_protocol(
        home,
        base_url,
        bearer_token,
        RelayProtocol::Responses,
        crate::protocol_proxy::DEFAULT_PROTOCOL_PROXY_PORT,
    )
}

pub fn apply_relay_files_to_home(
    home: &Path,
    config_contents: &str,
    auth_contents: &str,
) -> anyhow::Result<RelayApplyResult> {
    apply_relay_files_to_home_with_computer_use_guard(home, config_contents, auth_contents, false)
}

pub fn apply_relay_files_to_home_with_computer_use_guard(
    home: &Path,
    config_contents: &str,
    auth_contents: &str,
    preserve_computer_use_guard: bool,
) -> anyhow::Result<RelayApplyResult> {
    if config_contents.trim().is_empty() {
        anyhow::bail!("config.toml 内容不能为空");
    }
    std::fs::create_dir_all(home)?;

    let backup_path = write_codex_live_atomic(
        home,
        Some(config_contents),
        Some(auth_contents.as_bytes()),
        preserve_computer_use_guard,
    )?;

    let status = try_relay_config_status_from_home(home)?;
    Ok(RelayApplyResult {
        config_path: status.config_path,
        backup_path,
        configured: status.configured,
    })
}

pub fn apply_relay_files_to_home_with_common(
    home: &Path,
    config_contents: &str,
    auth_contents: &str,
    common_config_contents: &str,
) -> anyhow::Result<RelayApplyResult> {
    let config_contents = merge_common_config_into_config(config_contents, common_config_contents)?;
    apply_relay_files_to_home(home, &config_contents, auth_contents)
}

pub fn apply_relay_files_to_home_with_context(
    home: &Path,
    config_contents: &str,
    auth_contents: &str,
    common_config_contents: &str,
    selection: &RelayContextSelection,
    context_window: &str,
    auto_compact_limit: &str,
) -> anyhow::Result<RelayApplyResult> {
    let selected_common = filter_common_config_for_selection(common_config_contents, selection)?;
    let config_with_common = merge_common_config_into_config(config_contents, &selected_common)?;
    let config_with_common =
        preserve_unmanaged_live_context_entries(home, &config_with_common, common_config_contents)?;
    let config_with_limits =
        apply_context_limits_to_config(&config_with_common, context_window, auto_compact_limit)?;
    apply_relay_files_to_home(home, &config_with_limits, auth_contents)
}

pub fn apply_relay_profile_files_to_home_with_context(
    home: &Path,
    profile: &RelayProfile,
    common_config_contents: &str,
) -> anyhow::Result<RelayApplyResult> {
    let selected_common = if profile.use_common_config {
        filter_common_config_for_profile(common_config_contents, profile)?
    } else {
        String::new()
    };
    let profile_config = complete_relay_profile_config(profile)?;
    let config_with_common = merge_common_config_into_config(&profile_config, &selected_common)?;
    let config_with_common =
        preserve_unmanaged_live_context_entries(home, &config_with_common, common_config_contents)?;
    let config_with_limits = apply_context_limits_to_config(
        &config_with_common,
        &profile.context_window,
        &profile.auto_compact_limit,
    )?;
    let config_with_catalog = apply_model_catalog_to_config(home, profile, &config_with_limits)?;
    apply_relay_files_to_home(home, &config_with_catalog, &profile.auth_contents)
}

pub fn apply_relay_profile_to_home_with_switch_rules(
    home: &Path,
    profile: &RelayProfile,
    common_config_contents: &str,
) -> anyhow::Result<RelayApplyResult> {
    apply_relay_profile_to_home_with_switch_rules_and_computer_use_guard(
        home,
        profile,
        common_config_contents,
        false,
    )
}

pub fn apply_relay_profile_to_home_with_switch_rules_and_computer_use_guard(
    home: &Path,
    profile: &RelayProfile,
    common_config_contents: &str,
    preserve_computer_use_guard: bool,
) -> anyhow::Result<RelayApplyResult> {
    let selected_common = if profile.use_common_config {
        filter_common_config_for_profile(common_config_contents, profile)?
    } else {
        String::new()
    };
    let profile_config = complete_relay_profile_config(profile)?;
    let config_with_common = merge_common_config_into_config(&profile_config, &selected_common)?;
    let config_with_common =
        preserve_unmanaged_live_context_entries(home, &config_with_common, common_config_contents)?;
    let config_with_limits = apply_context_limits_to_config(
        &config_with_common,
        &profile.context_window,
        &profile.auto_compact_limit,
    )?;
    let config_with_catalog = apply_model_catalog_to_config(home, profile, &config_with_limits)?;

    if profile.relay_mode == crate::settings::RelayMode::PureApi {
        apply_relay_files_to_home_with_computer_use_guard(
            home,
            &config_with_catalog,
            &profile.auth_contents,
            preserve_computer_use_guard,
        )
    } else {
        let auth_contents = official_profile_auth_for_switch(home, &profile.auth_contents)?;
        apply_relay_files_to_home_with_computer_use_guard(
            home,
            &config_with_catalog,
            &auth_contents,
            preserve_computer_use_guard,
        )
    }
}

pub fn apply_relay_profile_config_to_home_with_context(
    home: &Path,
    profile: &RelayProfile,
    common_config_contents: &str,
) -> anyhow::Result<RelayApplyResult> {
    let selected_common = if profile.use_common_config {
        filter_common_config_for_selection(common_config_contents, &profile.context_selection)?
    } else {
        String::new()
    };
    let profile_config = complete_relay_profile_config(profile)?;
    let config_with_common = merge_common_config_into_config(&profile_config, &selected_common)?;
    let config_with_limits = apply_context_limits_to_config(
        &config_with_common,
        &profile.context_window,
        &profile.auto_compact_limit,
    )?;
    let config_with_catalog = apply_model_catalog_to_config(home, profile, &config_with_limits)?;
    apply_relay_config_file_to_home(home, &config_with_catalog)
}

pub fn apply_relay_config_file_to_home(
    home: &Path,
    config_contents: &str,
) -> anyhow::Result<RelayApplyResult> {
    let config_contents = config_contents
        .strip_prefix('\u{feff}')
        .unwrap_or(config_contents);
    if config_contents.trim().is_empty() {
        anyhow::bail!("config.toml 内容不能为空");
    }
    std::fs::create_dir_all(home)?;

    let backup_path = write_codex_live_atomic(home, Some(config_contents), None, false)?;

    let status = try_relay_config_status_from_home(home)?;
    Ok(RelayApplyResult {
        config_path: status.config_path,
        backup_path,
        configured: status.configured,
    })
}

pub fn apply_relay_auth_file_to_home(
    home: &Path,
    auth_contents: &str,
) -> anyhow::Result<RelayApplyResult> {
    let backup_path = write_codex_live_atomic(home, None, Some(auth_contents.as_bytes()), false)?;
    let status = try_relay_config_status_from_home(home)?;
    Ok(RelayApplyResult {
        config_path: status.config_path,
        backup_path,
        configured: status.configured,
    })
}

pub fn apply_pure_api_config_to_home_with_protocol(
    home: &Path,
    base_url: &str,
    bearer_token: &str,
    protocol: RelayProtocol,
    proxy_port: u16,
) -> anyhow::Result<RelayApplyResult> {
    let base_url = base_url.trim();
    if base_url.is_empty() {
        anyhow::bail!("中转 Base URL 不能为空");
    }
    let bearer_token = bearer_token.trim();
    if bearer_token.is_empty() {
        anyhow::bail!("中转 Key 不能为空");
    }
    let codex_base_url = codex_base_url_for_protocol(base_url, protocol, proxy_port);
    let updated = upsert_model_provider_config("", &codex_base_url, bearer_token, false)?;
    let auth_contents = serde_json::to_string_pretty(&json!({
        "OPENAI_API_KEY": bearer_token
    }))?;
    let backup_path =
        write_codex_live_atomic(home, Some(&updated), Some(auth_contents.as_bytes()), false)?;
    let status = try_relay_config_status_from_home(home)?;
    Ok(RelayApplyResult {
        config_path: status.config_path,
        backup_path,
        configured: status.configured,
    })
}

pub async fn test_relay_profile(
    profile: &RelayProfile,
    model: &str,
) -> anyhow::Result<RelayProfileTestResult> {
    test_relay_profile_with_mode(profile, model, false).await
}

pub async fn test_relay_profile_stream(
    profile: &RelayProfile,
    model: &str,
) -> anyhow::Result<RelayProfileTestResult> {
    test_relay_profile_with_mode(profile, model, true).await
}

async fn test_relay_profile_with_mode(
    profile: &RelayProfile,
    model: &str,
    stream: bool,
) -> anyhow::Result<RelayProfileTestResult> {
    let base_url = relay_profile_base_url(profile);
    let base_url = base_url.trim().trim_end_matches('/');
    if base_url.is_empty() {
        anyhow::bail!("Base URL 不能为空");
    }
    let api_key = relay_profile_api_key(profile);
    let api_key = api_key.trim();
    if api_key.is_empty() {
        anyhow::bail!("API Key 不能为空");
    }

    let client = crate::http_client::proxied_client("mirrorplus/RelayTest")?;
    let endpoint = match profile.protocol {
        RelayProtocol::Responses => format!("{base_url}/responses"),
        RelayProtocol::ChatCompletions => format!("{base_url}/chat/completions"),
    };
    let test_model = model.trim();
    if test_model.is_empty() {
        anyhow::bail!("测试模型不能为空");
    }

    let payload = relay_profile_test_payload(profile.protocol, test_model, stream);
    let (http_status, response_text) =
        send_relay_test_request(&client, &endpoint, api_key, &payload).await?;

    // 如果 404 且 base_url 末尾没有 /v1，尝试自动补 /v1 后再发一次。
    // 许多上游（中转站、自建代理）暴露的路径以 /v1/ 开头，
    // 用户容易遗漏这个前缀，导致 /responses 或 /chat/completions 404。
    if http_status == 404 && !base_url.ends_with("/v1") {
        let v1_url = format!("{base_url}/v1");
        let v1_endpoint = match profile.protocol {
            RelayProtocol::Responses => format!("{v1_url}/responses"),
            RelayProtocol::ChatCompletions => format!("{v1_url}/chat/completions"),
        };
        let (v1_status, v1_response_text) =
            send_relay_test_request(&client, &v1_endpoint, api_key, &payload).await?;
        if v1_status < 400 {
            validate_relay_probe_response(profile.protocol, v1_status, &v1_response_text, stream)?;
            return Ok(RelayProfileTestResult {
                http_status: v1_status,
                endpoint: v1_endpoint,
                response_preview: format!(
                    "（Base URL 建议加上 /v1 前缀）{}",
                    v1_response_text.chars().take(280).collect::<String>()
                ),
            });
        }
    }

    validate_relay_probe_response(profile.protocol, http_status, &response_text, stream)?;
    Ok(RelayProfileTestResult {
        http_status,
        endpoint,
        response_preview: response_text.chars().take(320).collect(),
    })
}

async fn send_relay_test_request(
    client: &reqwest::Client,
    endpoint: &str,
    api_key: &str,
    payload: &Value,
) -> anyhow::Result<(u16, String)> {
    let response = client
        .post(endpoint)
        .bearer_auth(api_key)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .json(payload)
        .timeout(std::time::Duration::from_secs(45))
        .send()
        .await?;
    let http_status = response.status().as_u16();
    let response_text = response
        .text()
        .await
        .with_context(|| "无法读取中转探测响应")?;
    Ok((http_status, response_text))
}

fn validate_relay_probe_response(
    protocol: RelayProtocol,
    http_status: u16,
    response_text: &str,
    stream: bool,
) -> anyhow::Result<()> {
    if stream {
        validate_relay_stream_test_response(protocol, http_status, response_text)
    } else {
        validate_relay_test_response(protocol, http_status, response_text)
    }
}

fn relay_profile_test_payload(protocol: RelayProtocol, model: &str, stream: bool) -> Value {
    match protocol {
        RelayProtocol::Responses => serde_json::json!({
            "model": model,
            "input": "Reply with exactly OK.",
            "max_output_tokens": 128,
            "store": false,
            "stream": stream
        }),
        RelayProtocol::ChatCompletions => serde_json::json!({
            "model": model,
            "messages": [
                { "role": "user", "content": "Reply with exactly OK." }
            ],
            "max_tokens": 128,
            "stream": stream
        }),
    }
}

fn codex_base_url_for_protocol(base_url: &str, protocol: RelayProtocol, proxy_port: u16) -> String {
    match protocol {
        RelayProtocol::Responses => base_url.to_string(),
        RelayProtocol::ChatCompletions => {
            crate::protocol_proxy::local_responses_proxy_base_url(proxy_port)
        }
    }
}

pub fn clear_relay_config_to_home(home: &Path) -> anyhow::Result<RelayApplyResult> {
    clear_relay_config_to_home_with_auth(home, None)
}

pub fn clear_relay_config_to_home_with_auth(
    home: &Path,
    auth_contents: Option<&str>,
) -> anyhow::Result<RelayApplyResult> {
    clear_relay_config_to_home_with_auth_and_computer_use_guard(home, auth_contents, false)
}

pub fn clear_relay_config_to_home_with_auth_and_computer_use_guard(
    home: &Path,
    auth_contents: Option<&str>,
    preserve_computer_use_guard: bool,
) -> anyhow::Result<RelayApplyResult> {
    std::fs::create_dir_all(home)?;
    let auth_bytes = match auth_contents {
        Some(contents) if !contents.trim().is_empty() => Some(contents.as_bytes().to_vec()),
        _ => pure_api_auth_json_removed(home)?,
    };
    let config_path = home.join("config.toml");
    let existing = read_optional_text(&config_path)?;
    // Validate before any derived changes or atomic writes. A malformed live
    // config is user data that must remain untouched until it can be parsed.
    parse_toml_document(&existing)?;
    let mut without_tables = remove_table(&existing, &format!("model_providers.{RELAY_PROVIDER}"));
    for legacy_provider in LEGACY_RELAY_PROVIDERS {
        without_tables = remove_table(
            &without_tables,
            &format!("model_providers.{legacy_provider}"),
        );
    }
    let mut updated = without_tables;
    for key in [
        "OPENAI_API_KEY",
        "model_provider",
        "model_catalog_json",
        "base_url",
    ] {
        updated = remove_root_key(&updated, key);
    }
    let backup_path = write_codex_live_atomic(
        home,
        Some(&updated),
        auth_bytes.as_deref(),
        preserve_computer_use_guard,
    )?;
    let status = try_relay_config_status_from_home(home)?;
    Ok(RelayApplyResult {
        config_path: status.config_path,
        backup_path,
        configured: status.configured,
    })
}

fn pure_api_auth_json_removed(home: &Path) -> anyhow::Result<Option<Vec<u8>>> {
    let auth_path = home.join("auth.json");
    if !auth_path.exists() {
        return Ok(None);
    }

    let existing = std::fs::read_to_string(&auth_path)?;
    let Ok(mut value) = serde_json::from_str::<Value>(&existing) else {
        return Ok(None);
    };
    let Some(object) = value.as_object_mut() else {
        return Ok(None);
    };
    if object.remove("OPENAI_API_KEY").is_none() {
        return Ok(None);
    }

    Ok(Some(serde_json::to_vec_pretty(&value)?))
}

pub fn backfill_relay_profile_from_home(
    home: &Path,
    profile: &mut RelayProfile,
) -> anyhow::Result<()> {
    profile.config_contents = read_optional_text(&home.join("config.toml"))?;
    profile.auth_contents = read_optional_text(&home.join("auth.json"))?;
    let live_config = profile.config_contents.clone();
    sync_context_limits_from_config(profile, &live_config);
    if profile.model.trim().is_empty() {
        if let Some(model) = root_key_string(&profile.config_contents, "model") {
            profile.model = model;
        }
    }
    Ok(())
}

pub fn backfill_relay_profile_from_home_with_common(
    home: &Path,
    profile: &mut RelayProfile,
    common_config_contents: &mut String,
) -> anyhow::Result<()> {
    let live_config = read_optional_text(&home.join("config.toml"))?;
    let template_config = profile.config_contents.clone();
    let template_auth = profile.auth_contents.clone();
    let template_api_key = relay_profile_api_key(profile);
    profile.config_contents = if profile.use_common_config {
        strip_common_config_from_config(&live_config, common_config_contents)?
    } else {
        ensure_trailing_newline(live_config.clone())
    };
    profile.config_contents =
        restore_profile_provider_id_for_backfill(&profile.config_contents, &template_config)?;
    let live_auth = read_optional_text(&home.join("auth.json"))?;
    restore_profile_credentials_after_backfill(
        profile,
        &template_auth,
        &template_api_key,
        &live_auth,
    )?;
    sync_profile_mode_from_backfilled_live(profile);
    sync_context_limits_from_config(profile, &live_config);
    if profile.model.trim().is_empty() {
        if let Some(model) = root_key_string(&live_config, "model") {
            profile.model = model;
        }
    }
    Ok(())
}

pub fn extract_common_config_from_config(config_text: &str) -> anyhow::Result<String> {
    let mut doc = parse_toml_document(config_text)?;
    remove_provider_specific_common_keys(doc.as_table_mut());
    Ok(normalize_optional_toml(doc))
}

pub fn sanitize_common_config_contents(common_config: &str) -> String {
    match parse_toml_document(common_config) {
        Ok(mut doc) => {
            remove_provider_specific_common_keys(doc.as_table_mut());
            normalize_optional_toml(doc)
        }
        Err(_) => sanitize_common_config_text_fallback(common_config),
    }
}

pub fn strip_common_config_from_config(
    config_text: &str,
    common_config_contents: &str,
) -> anyhow::Result<String> {
    let trimmed = common_config_contents.trim();
    if trimmed.is_empty() {
        return Ok(normalize_duplicate_toml_text(config_text));
    }

    match (
        parse_toml_document(config_text),
        parse_toml_document(trimmed),
    ) {
        (Ok(mut target_doc), Ok(source_doc)) => {
            remove_toml_table_like(target_doc.as_table_mut(), source_doc.as_table());
            Ok(normalize_optional_toml(target_doc))
        }
        _ => Ok(strip_common_config_text_fallback(config_text, trimmed)),
    }
}

pub fn merge_common_config_into_config(
    config_text: &str,
    common_config_contents: &str,
) -> anyhow::Result<String> {
    let sanitized_common = sanitize_common_config_contents(common_config_contents);
    let trimmed = sanitized_common.trim();
    if trimmed.is_empty() {
        return Ok(ensure_trailing_newline(config_text.to_string()));
    }

    let mut target_doc = parse_toml_document(config_text)?;
    let source_doc = parse_toml_document(trimmed)?;
    merge_toml_table_like(target_doc.as_table_mut(), source_doc.as_table());
    Ok(normalize_optional_toml(target_doc))
}

pub fn list_context_entries_from_common_config(
    common_config: &str,
) -> anyhow::Result<CodexContextEntries> {
    let normalized = normalize_duplicate_toml_text(common_config);
    let doc = parse_toml_document(&normalized)?;
    Ok(CodexContextEntries {
        mcp_servers: list_context_entries_for_table(&doc, "mcp_servers"),
        skills: list_context_entries_for_table(&doc, "skills"),
        plugins: list_context_entries_for_table(&doc, "plugins"),
    })
}

pub fn upsert_context_entry_in_common_config(
    common_config: &str,
    kind: &str,
    id: &str,
    toml_body: &str,
) -> anyhow::Result<String> {
    let id = id.trim();
    if id.is_empty() {
        anyhow::bail!("上下文 id 不能为空");
    }
    let table_name = context_table_name(kind)?;
    let body_doc = parse_toml_document(toml_body)?;
    let normalized = normalize_duplicate_toml_text(common_config);
    let mut doc = parse_toml_document(&normalized)?;
    if !doc.as_table().contains_key(table_name) {
        doc[table_name] = toml_edit::table();
    }
    if doc[table_name].as_table().is_none() {
        anyhow::bail!("{table_name} 必须是 TOML 表");
    }
    doc[table_name][id] = Item::Table(body_doc.as_table().clone());
    Ok(normalize_optional_toml(doc))
}

pub fn delete_context_entry_from_common_config(
    common_config: &str,
    kind: &str,
    id: &str,
) -> anyhow::Result<String> {
    let table_name = context_table_name(kind)?;
    let normalized = normalize_duplicate_toml_text(common_config);
    let mut doc = parse_toml_document(&normalized)?;
    if let Some(table) = doc[table_name].as_table_mut() {
        table.remove(id.trim());
        if table.is_empty() {
            doc.as_table_mut().remove(table_name);
        }
    }
    Ok(normalize_optional_toml(doc))
}

pub fn filter_common_config_for_selection(
    common_config: &str,
    selection: &RelayContextSelection,
) -> anyhow::Result<String> {
    let sanitized_common = sanitize_common_config_contents(common_config);
    let mut filtered = parse_toml_document(&sanitized_common)?;
    filter_context_tables_for_selection(filtered.as_table_mut(), selection);
    remove_disabled_context_tables(filtered.as_table_mut());
    Ok(normalize_optional_toml(filtered))
}

fn filter_common_config_for_profile(
    common_config: &str,
    profile: &RelayProfile,
) -> anyhow::Result<String> {
    if profile.context_selection_initialized {
        filter_common_config_for_selection(common_config, &profile.context_selection)
    } else {
        let sanitized_common = sanitize_common_config_contents(common_config);
        let mut filtered = parse_toml_document(&sanitized_common)?;
        remove_disabled_context_tables(filtered.as_table_mut());
        Ok(normalize_optional_toml(filtered))
    }
}

pub fn sync_live_config_context_entries(
    live_config: &str,
    context_config: &str,
) -> anyhow::Result<String> {
    let normalized_live = normalize_duplicate_toml_text(live_config);
    let normalized_context = normalize_duplicate_toml_text(context_config);
    let mut live_doc = parse_toml_document(&normalized_live)?;
    if normalized_context.trim().is_empty() {
        return Ok(normalize_optional_toml(live_doc));
    }
    let managed_doc = parse_toml_document(&normalized_context)?;
    remove_managed_context_entries(live_doc.as_table_mut(), managed_doc.as_table());
    let mut context_doc = managed_doc;
    remove_disabled_context_tables(context_doc.as_table_mut());
    merge_managed_context_tables(live_doc.as_table_mut(), context_doc.as_table());
    Ok(normalize_optional_toml(live_doc))
}

fn preserve_unmanaged_live_context_entries(
    home: &Path,
    config_text: &str,
    managed_context_config: &str,
) -> anyhow::Result<String> {
    let live_config = read_optional_text(&home.join("config.toml"))?;
    if live_config.trim().is_empty() {
        return Ok(ensure_trailing_newline(config_text.to_string()));
    }
    let mut target_doc = parse_toml_document(config_text)?;
    let live_doc = parse_toml_document(&live_config)?;
    let managed_doc =
        parse_toml_document(&sanitize_common_config_contents(managed_context_config))?;
    preserve_unmanaged_context_tables(
        target_doc.as_table_mut(),
        live_doc.as_table(),
        managed_doc.as_table(),
    );
    Ok(normalize_optional_toml(target_doc))
}

fn filter_context_tables_for_selection(
    table: &mut toml_edit::Table,
    selection: &RelayContextSelection,
) {
    filter_context_table_for_ids(table, "mcp_servers", &selection.mcp_servers);
    filter_context_table_for_ids(table, "skills", &selection.skills);
    filter_context_table_for_ids(table, "plugins", &selection.plugins);
}

fn filter_context_table_for_ids(
    table: &mut toml_edit::Table,
    table_name: &str,
    selected_ids: &[String],
) {
    let Some(item) = table.get_mut(table_name) else {
        return;
    };
    let Some(context_table) = item.as_table_mut() else {
        return;
    };
    let selected = selected_ids
        .iter()
        .map(|id| id.trim())
        .filter(|id| !id.is_empty())
        .collect::<HashSet<_>>();
    let remove_ids = context_table
        .iter()
        .filter_map(|(id, _)| (!selected.contains(id)).then_some(id.to_string()))
        .collect::<Vec<_>>();
    for id in remove_ids {
        context_table.remove(&id);
    }
}

fn merge_managed_context_tables(target: &mut toml_edit::Table, managed: &toml_edit::Table) {
    for table_name in ["mcp_servers", "skills", "plugins"] {
        merge_managed_context_table(target, managed, table_name);
    }
}

fn merge_managed_context_table(
    target: &mut toml_edit::Table,
    managed: &toml_edit::Table,
    table_name: &str,
) {
    let Some(managed_item) = managed.get(table_name) else {
        return;
    };
    let Some(managed_table) = managed_item.as_table_like() else {
        return;
    };
    if target.get(table_name).is_none() {
        target[table_name] = toml_edit::table();
    }
    let Some(target_table) = target.get_mut(table_name).and_then(Item::as_table_like_mut) else {
        target[table_name] = managed_item.clone();
        return;
    };
    for (id, item) in managed_table.iter() {
        target_table.insert(id, item.clone());
    }
}

fn remove_managed_context_entries(target: &mut toml_edit::Table, managed: &toml_edit::Table) {
    for table_name in ["mcp_servers", "skills", "plugins"] {
        remove_managed_context_entry_table(target, managed, table_name);
    }
}

fn remove_managed_context_entry_table(
    target: &mut toml_edit::Table,
    managed: &toml_edit::Table,
    table_name: &str,
) {
    let Some(managed_item) = managed.get(table_name) else {
        return;
    };
    let Some(managed_table) = managed_item.as_table_like() else {
        return;
    };
    let Some(target_table) = target.get_mut(table_name).and_then(Item::as_table_like_mut) else {
        return;
    };
    for (id, _) in managed_table.iter() {
        target_table.remove(id);
    }
}

fn preserve_unmanaged_context_tables(
    target: &mut toml_edit::Table,
    live: &toml_edit::Table,
    managed: &toml_edit::Table,
) {
    for table_name in ["mcp_servers", "skills", "plugins"] {
        preserve_unmanaged_context_table(target, live, managed, table_name);
    }
}

fn preserve_unmanaged_context_table(
    target: &mut toml_edit::Table,
    live: &toml_edit::Table,
    managed: &toml_edit::Table,
    table_name: &str,
) {
    let Some(live_item) = live.get(table_name) else {
        return;
    };
    let Some(live_table) = live_item.as_table_like() else {
        return;
    };
    if target.get(table_name).is_none() {
        target[table_name] = toml_edit::table();
    }
    let Some(target_table) = target.get_mut(table_name).and_then(Item::as_table_like_mut) else {
        return;
    };
    let managed_ids = managed
        .get(table_name)
        .and_then(Item::as_table_like)
        .map(|table| {
            table
                .iter()
                .map(|(id, _)| id.to_string())
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();
    for (id, item) in live_table.iter() {
        if !managed_ids.contains(id) && target_table.get(id).is_none() {
            target_table.insert(id, item.clone());
        }
    }
}

fn remove_disabled_context_tables(table: &mut toml_edit::Table) {
    for table_name in ["mcp_servers", "skills", "plugins"] {
        let Some(item) = table.get_mut(table_name) else {
            continue;
        };
        let Some(context_table) = item.as_table_mut() else {
            continue;
        };
        let disabled_ids: Vec<String> = context_table
            .iter()
            .filter_map(|(id, item)| {
                let enabled = item.as_table().map(context_entry_enabled).unwrap_or(true);
                (!enabled).then_some(id.to_string())
            })
            .collect();
        for id in disabled_ids {
            context_table.remove(&id);
        }
    }
}

fn write_codex_live_atomic(
    home: &Path,
    config_text: Option<&str>,
    auth_bytes: Option<&[u8]>,
    preserve_computer_use_guard: bool,
) -> anyhow::Result<Option<String>> {
    let initial_planned_bytes = config_text
        .map(|contents| contents.len() as u64)
        .unwrap_or_default()
        .saturating_add(
            auth_bytes
                .map(|contents| contents.len() as u64)
                .unwrap_or_default(),
        )
        .saturating_mul(2)
        .saturating_add(4096);
    crate::mirror_access::ensure_storage_headroom(
        home,
        initial_planned_bytes,
        crate::mirror_access::MIN_SAFE_FREE_SPACE_BYTES,
    )?;
    std::fs::create_dir_all(home)?;
    let config_path = home.join("config.toml");
    let auth_path = home.join("auth.json");
    #[cfg(windows)]
    let guarded_config_text = match config_text {
        Some(config_text) if preserve_computer_use_guard => {
            let notify_exe = crate::computer_use_guard::find_computer_use_notify_exe(home);
            let marketplace_path =
                crate::computer_use_guard::ensure_openai_bundled_marketplace(home)?;
            let guarded = if let Some(marketplace_path) = marketplace_path.as_deref() {
                crate::computer_use_guard::guard_config_text_with_marketplace(
                    config_text,
                    notify_exe.as_deref(),
                    Some(marketplace_path),
                )?
            } else {
                crate::computer_use_guard::guard_config_text(config_text, notify_exe.as_deref())?
            };
            Some(guarded)
        }
        Some(config_text) => Some(normalize_config_text_for_write(config_text)),
        None => None,
    };
    #[cfg(windows)]
    let config_text = guarded_config_text.as_deref();

    let config_text = match config_text {
        Some(config_text) => Some(preserve_live_marketplace_configs(home, config_text)?),
        None => None,
    };
    let config_text = config_text.as_deref();

    let config_text = match config_text {
        Some(config_text) => Some(
            crate::plugin_marketplace::preserve_openai_curated_remote_marketplace_config(
                home,
                config_text,
            )?,
        ),
        None => None,
    };
    let config_text = config_text.as_deref();

    if let Some(config_text) = config_text {
        validate_toml_config(config_text, &config_path)?;
    }
    if let Some(auth_bytes) = auth_bytes {
        validate_auth_json(auth_bytes, &auth_path)?;
    }

    let old_config = read_optional_bytes(&config_path)?;
    let old_auth = read_optional_bytes(&auth_path)?;
    let planned_bytes = initial_planned_bytes
        .saturating_add(
            old_config
                .as_ref()
                .map(|contents| contents.len() as u64)
                .unwrap_or_default(),
        )
        .saturating_add(
            old_auth
                .as_ref()
                .map(|contents| contents.len() as u64)
                .unwrap_or_default(),
        );
    crate::mirror_access::ensure_storage_headroom(
        home,
        planned_bytes,
        crate::mirror_access::MIN_SAFE_FREE_SPACE_BYTES,
    )?;
    let backup_path = create_live_backup(home, old_config.as_deref(), old_auth.as_deref())?;

    if let Some(auth_bytes) = auth_bytes {
        if let Err(error) = crate::settings::atomic_write(&auth_path, auth_bytes) {
            return Err(error.context("写入 auth.json 失败"));
        }
    }

    if let Some(config_text) = config_text {
        if let Err(error) = crate::settings::atomic_write(&config_path, config_text.as_bytes()) {
            return rollback_live_write_error(
                &config_path,
                old_config.as_deref(),
                &auth_path,
                old_auth.as_deref(),
                error.context("写入 config.toml 失败"),
            );
        }
    }

    let verification = (|| -> anyhow::Result<()> {
        if let Some(auth_bytes) = auth_bytes {
            let persisted = read_optional_bytes(&auth_path)?;
            if persisted.as_deref() != Some(auth_bytes) {
                anyhow::bail!("auth.json 写后逐字节校验不一致");
            }
        }
        if let Some(config_text) = config_text {
            let persisted = read_optional_bytes(&config_path)?;
            if persisted.as_deref() != Some(config_text.as_bytes()) {
                anyhow::bail!("config.toml 写后逐字节校验不一致");
            }
        }
        try_relay_config_status_from_home(home)
            .context("写后无法重新解析 config.toml / auth.json")?;
        Ok(())
    })();
    if let Err(error) = verification {
        return rollback_live_write_error(
            &config_path,
            old_config.as_deref(),
            &auth_path,
            old_auth.as_deref(),
            error,
        );
    }

    Ok(backup_path)
}

fn rollback_live_write_error<T>(
    config_path: &Path,
    old_config: Option<&[u8]>,
    auth_path: &Path,
    old_auth: Option<&[u8]>,
    write_error: anyhow::Error,
) -> anyhow::Result<T> {
    let mut recovery_failures = Vec::new();
    if let Err(error) = restore_optional_file(auth_path, old_auth) {
        recovery_failures.push(format!("恢复 auth.json 失败：{error:#}"));
    }
    if let Err(error) = restore_optional_file(config_path, old_config) {
        recovery_failures.push(format!("恢复 config.toml 失败：{error:#}"));
    }
    if recovery_failures.is_empty() {
        Err(write_error.context("已恢复写入前的 Codex live 配置"))
    } else {
        Err(anyhow::anyhow!(
            "{write_error:#}；自动恢复也未完整成功：{}",
            recovery_failures.join("；")
        ))
    }
}

fn preserve_live_marketplace_configs(home: &Path, config_text: &str) -> anyhow::Result<String> {
    let live_config = read_optional_text(&home.join("config.toml"))?;
    if live_config.trim().is_empty() {
        return Ok(config_text.to_string());
    }

    let mut target = parse_toml_document(config_text)?;
    let live = parse_toml_document(&live_config)?;
    let Some(live_marketplaces) = live.get("marketplaces").and_then(Item::as_table_like) else {
        return Ok(ensure_trailing_newline(target.to_string()));
    };
    if live_marketplaces.is_empty() {
        return Ok(ensure_trailing_newline(target.to_string()));
    }

    if target.get("marketplaces").is_none() {
        target["marketplaces"] = toml_edit::table();
    }
    if target
        .get("marketplaces")
        .and_then(Item::as_table_like)
        .is_none()
    {
        target["marketplaces"] = toml_edit::table();
    }
    let Some(target_marketplaces) = target
        .get_mut("marketplaces")
        .and_then(Item::as_table_like_mut)
    else {
        return Ok(ensure_trailing_newline(target.to_string()));
    };

    for (name, marketplace) in live_marketplaces.iter() {
        if target_marketplaces.get(name).is_none() {
            target_marketplaces.insert(name, marketplace.clone());
        }
    }

    Ok(ensure_trailing_newline(target.to_string()))
}

fn active_provider_id(doc: &DocumentMut) -> Option<String> {
    doc.get("model_provider")
        .and_then(Item::as_str)
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
        .map(ToString::to_string)
}

fn active_or_default_provider_id(doc: &DocumentMut) -> String {
    active_provider_id(doc)
        .filter(|provider| {
            is_custom_provider_id(provider) && !LEGACY_RELAY_PROVIDERS.contains(&provider.as_str())
        })
        .unwrap_or_else(|| RELAY_PROVIDER.to_string())
}

fn is_custom_provider_id(provider: &str) -> bool {
    !provider.is_empty() && !RESERVED_MODEL_PROVIDER_IDS.contains(&provider)
}

fn provider_table_exists(doc: &DocumentMut, provider_id: &str) -> bool {
    doc.get("model_providers")
        .and_then(Item::as_table)
        .and_then(|table| table.get(provider_id))
        .is_some()
}

fn parse_toml_document(contents: &str) -> anyhow::Result<DocumentMut> {
    let contents = contents.trim_start_matches('\u{feff}');
    if contents.trim().is_empty() {
        Ok(DocumentMut::new())
    } else {
        contents
            .parse::<DocumentMut>()
            .map_err(|error| anyhow::anyhow!("config.toml TOML 解析失败：{error}"))
    }
}

fn remove_provider_specific_common_keys(table: &mut dyn TableLike) {
    for key in PROVIDER_SPECIFIC_COMMON_ROOT_KEYS {
        table.remove(key);
    }
    let credential_keys = table
        .iter()
        .map(|(key, _)| key.to_string())
        .filter(|key| is_provider_credential_root_key(key))
        .collect::<Vec<_>>();
    for key in credential_keys {
        table.remove(&key);
    }
    table.remove("model_providers");
}

fn is_provider_specific_common_root_key(key: &str) -> bool {
    let key = key.trim().trim_matches(['"', '\'']);
    PROVIDER_SPECIFIC_COMMON_ROOT_KEYS.contains(&key) || is_provider_credential_root_key(key)
}

fn is_provider_credential_root_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    matches!(
        key.as_str(),
        "api_key" | "access_token" | "bearer_token" | "experimental_bearer_token"
    ) || key.ends_with("_api_key")
        || key.ends_with("_access_token")
        || key.ends_with("_bearer_token")
}

fn sanitize_common_config_text_fallback(common_config: &str) -> String {
    let mut kept = Vec::new();
    let mut in_root = true;
    let mut skipping_model_providers = false;

    for line in common_config.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_root = false;
            skipping_model_providers =
                trimmed == "[model_providers]" || trimmed.starts_with("[model_providers.");
            if skipping_model_providers {
                continue;
            }
        } else if skipping_model_providers {
            continue;
        }

        if in_root {
            if let Some((key, _)) = trimmed.split_once('=') {
                if is_provider_specific_common_root_key(key) {
                    continue;
                }
            }
        }

        kept.push(line);
    }

    normalize_text_toml(kept.join("\n"))
}

fn normalize_text_toml(contents: String) -> String {
    let trimmed = contents.trim();
    if trimmed.is_empty() {
        String::new()
    } else {
        ensure_trailing_newline(trimmed.to_string())
    }
}

pub fn normalize_config_text(contents: &str) -> String {
    normalize_duplicate_toml_text(contents)
}

fn normalize_duplicate_toml_text(contents: &str) -> String {
    let mut seen_root_keys = HashSet::new();
    let mut seen_headers = HashSet::new();
    let mut kept = Vec::new();
    let mut skipping_duplicate_table = false;
    let mut in_root = true;

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_root = false;
            skipping_duplicate_table = !seen_headers.insert(trimmed.to_string());
            if skipping_duplicate_table {
                continue;
            }
            kept.push(line);
            continue;
        }

        if skipping_duplicate_table {
            continue;
        }

        if in_root && !trimmed.is_empty() && !trimmed.starts_with('#') {
            if let Some((key, _)) = trimmed.split_once('=') {
                let key = key.trim();
                if !key.is_empty() && !key.contains('.') && !seen_root_keys.insert(key.to_string())
                {
                    continue;
                }
            }
        }

        kept.push(line);
    }

    normalize_text_toml(kept.join("\n"))
}

fn strip_common_config_text_fallback(config_text: &str, common_config: &str) -> String {
    let normalized = normalize_duplicate_toml_text(config_text);
    let anchors = common_config_anchors(common_config);
    if anchors.root_keys.is_empty() && anchors.table_headers.is_empty() {
        return normalized;
    }

    let mut kept = Vec::new();
    let mut skipping_table = false;

    for line in normalized.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            skipping_table = anchors.table_headers.contains(trimmed);
            if skipping_table {
                continue;
            }
            kept.push(line);
            continue;
        }

        if skipping_table {
            continue;
        }

        if !trimmed.is_empty() && !trimmed.starts_with('#') {
            if let Some((key, _)) = trimmed.split_once('=') {
                if anchors.root_keys.contains(key.trim()) {
                    continue;
                }
            }
        }

        kept.push(line);
    }

    normalize_text_toml(kept.join("\n"))
}

struct CommonConfigAnchors {
    root_keys: HashSet<String>,
    table_headers: HashSet<String>,
}

fn common_config_anchors(common_config: &str) -> CommonConfigAnchors {
    let mut root_keys = HashSet::new();
    let mut table_headers = HashSet::new();
    let mut in_root = true;

    for line in common_config.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_root = false;
            table_headers.insert(trimmed.to_string());
            continue;
        }

        if in_root && !trimmed.is_empty() && !trimmed.starts_with('#') {
            if let Some((key, _)) = trimmed.split_once('=') {
                let key = key.trim();
                if !key.is_empty() {
                    root_keys.insert(key.to_string());
                }
            }
        }
    }

    CommonConfigAnchors {
        root_keys,
        table_headers,
    }
}

fn validate_toml_config(config_text: &str, path: &Path) -> anyhow::Result<()> {
    let config_text = config_text.trim_start_matches('\u{feff}');
    if config_text.trim().is_empty() {
        return Ok(());
    }
    config_text
        .parse::<toml::Table>()
        .with_context(|| format!("{} 不是有效 TOML", path.display()))?;
    Ok(())
}

fn normalize_config_text_for_write(config_text: &str) -> String {
    config_text.trim_start_matches('\u{feff}').to_string()
}

fn validate_auth_json(auth_bytes: &[u8], path: &Path) -> anyhow::Result<()> {
    if auth_bytes.iter().all(|byte| byte.is_ascii_whitespace()) {
        return Ok(());
    }
    serde_json::from_slice::<Value>(auth_bytes)
        .with_context(|| format!("{} 不是有效 JSON", path.display()))?;
    Ok(())
}

fn parse_optional_positive_u64(value: &str, label: &str) -> anyhow::Result<Option<u64>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let parsed = trimmed
        .parse::<u64>()
        .with_context(|| format!("{label}必须是正整数"))?;
    if parsed == 0 {
        anyhow::bail!("{label}必须大于 0");
    }
    Ok(Some(parsed))
}

fn apply_context_limits_to_config(
    config_text: &str,
    context_window: &str,
    auto_compact_limit: &str,
) -> anyhow::Result<String> {
    let mut doc = parse_toml_document(config_text)?;
    if let Some(value) = parse_optional_positive_u64(context_window, "上下文大小")? {
        doc["model_context_window"] = toml_edit::value(value as i64);
    }
    if let Some(value) = parse_optional_positive_u64(auto_compact_limit, "压缩上下文大小")? {
        doc["model_auto_compact_token_limit"] = toml_edit::value(value as i64);
    }
    Ok(normalize_optional_toml(doc))
}

fn apply_model_catalog_to_config(
    home: &Path,
    profile: &RelayProfile,
    config_text: &str,
) -> anyhow::Result<String> {
    let catalog_relative = format!(
        "model-catalogs/{}.json",
        sanitize_catalog_filename(&profile.id)
    );
    let mut config_text = config_text.to_string();
    let custom_responses = custom_responses_provider(&config_text);
    // 用户手写的 catalog 继续保留；cc-switch 的固定 catalog 是其他管理器的运行时投影，
    // 切换到 Mirror X Provider 时必须撤销，否则它会继续覆盖当前模型列表。
    if let Some(existing) = root_key_string(&config_text, "model_catalog_json") {
        if existing != catalog_relative {
            if is_cc_switch_model_catalog(&existing) {
                config_text = remove_root_key(&config_text, "model_catalog_json");
            } else if custom_responses
                && copy_standard_responses_catalog(home, &existing, &catalog_relative)?
            {
                let mut doc = parse_toml_document(&config_text)?;
                doc["model_catalog_json"] = toml_edit::value(catalog_relative);
                return Ok(normalize_optional_toml(doc));
            } else {
                return Ok(config_text);
            }
        }
    }
    if let Some(external_catalog) = live_external_model_catalog(home) {
        let mut doc = parse_toml_document(&config_text)?;
        if custom_responses
            && copy_standard_responses_catalog(home, &external_catalog, &catalog_relative)?
        {
            doc["model_catalog_json"] = toml_edit::value(catalog_relative);
        } else {
            doc["model_catalog_json"] = toml_edit::value(external_catalog);
        }
        return Ok(normalize_optional_toml(doc));
    }
    let (model_list, model_windows): (String, std::collections::HashMap<String, String>) =
        if profile.model_windows.trim().is_empty() && profile.model_list.contains('[') {
            crate::model_suffix::migrate_model_list_with_suffixes(&profile.model_list)
        } else {
            (
                profile.model_list.clone(),
                serde_json::from_str(&profile.model_windows).unwrap_or_default(),
            )
        };
    let entries =
        crate::model_suffix::collect_catalog_entries(&model_list, &model_windows, &profile.model);
    if !entries.iter().any(|entry| {
        entry.suffix_window.is_some()
            || crate::model_suffix::requires_bundled_metadata_catalog(&entry.slug)
    }) {
        return Ok(config_text);
    }
    let fallback = parse_optional_positive_u64(&profile.context_window, "上下文大小")?;
    let catalog_path = home.join(&catalog_relative);
    if let Some(parent) = catalog_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Only custom Responses providers need the standard Responses tool wire format. Official
    // profiles and custom Chat Completions retain the model template's original Lite behavior.
    let catalog_json = crate::model_suffix::build_model_catalog_json_with_capabilities(
        &entries,
        fallback,
        None,
        custom_responses.then_some(false),
    );
    std::fs::write(&catalog_path, catalog_json)?;
    let mut doc = parse_toml_document(&config_text)?;
    doc["model_catalog_json"] = toml_edit::value(catalog_relative);
    Ok(normalize_optional_toml(doc))
}

fn custom_responses_provider(config_text: &str) -> bool {
    let Ok(doc) = parse_toml_document(config_text) else {
        return false;
    };
    let Some(provider_id) = active_provider_id(&doc) else {
        return false;
    };
    if !is_custom_provider_id(&provider_id) {
        return false;
    }
    doc.get("model_providers")
        .and_then(Item::as_table)
        .and_then(|providers| providers.get(&provider_id))
        .and_then(Item::as_table_like)
        .and_then(|provider| provider.get("wire_api"))
        .and_then(Item::as_str)
        .is_some_and(|wire_api| wire_api.trim().eq_ignore_ascii_case("responses"))
}

fn copy_standard_responses_catalog(
    home: &Path,
    source: &str,
    target_relative: &str,
) -> anyhow::Result<bool> {
    let source_path = {
        let path = Path::new(source);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            home.join(path)
        }
    };
    let Ok(contents) = std::fs::read_to_string(source_path) else {
        return Ok(false);
    };
    let Ok(mut catalog) = serde_json::from_str::<Value>(&contents) else {
        return Ok(false);
    };
    let Some(models) = catalog.get_mut("models").and_then(Value::as_array_mut) else {
        return Ok(false);
    };
    let mut changed = false;
    for model in models {
        if model.get("use_responses_lite").and_then(Value::as_bool) == Some(true) {
            model["use_responses_lite"] = Value::Bool(false);
            changed = true;
        }
    }
    if !changed {
        return Ok(false);
    }

    let target = home.join(target_relative);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(target, serde_json::to_string_pretty(&catalog)?)?;
    Ok(true)
}

fn live_external_model_catalog(home: &Path) -> Option<String> {
    let live_text = read_optional_text(&home.join("config.toml")).ok()?;
    let live = parse_toml_document(&live_text).ok()?;
    let path = live.get("model_catalog_json")?.as_str()?.trim();
    (!path.is_empty()
        && !is_codex_plus_managed_model_catalog(home, path)
        && !is_cc_switch_model_catalog(path))
    .then(|| path.to_string())
}

fn is_codex_plus_managed_model_catalog(home: &Path, path: &str) -> bool {
    let normalized = path.trim().replace('\\', "/");
    let relative = normalized.trim_start_matches("./");
    if relative.to_ascii_lowercase().starts_with("model-catalogs/") {
        return true;
    }
    let normalized_lower = normalized.to_ascii_lowercase();
    if normalized_lower.contains("/model-catalogs/")
        || normalized_lower.ends_with("/model-catalogs")
    {
        return true;
    }
    let managed_root = home
        .join("model-catalogs")
        .to_string_lossy()
        .replace('\\', "/");
    let managed_root = managed_root.trim_end_matches('/');
    normalized.eq_ignore_ascii_case(managed_root)
        || normalized
            .get(..managed_root.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(managed_root))
            && normalized
                .as_bytes()
                .get(managed_root.len())
                .is_some_and(|byte| *byte == b'/')
}

fn sanitize_catalog_filename(id: &str) -> String {
    id.chars()
        .map(|char| {
            if char.is_ascii_alphanumeric() || char == '-' || char == '_' {
                char
            } else {
                '-'
            }
        })
        .collect()
}

fn sync_context_limits_from_config(profile: &mut RelayProfile, config_text: &str) {
    if let Some(value) = root_positive_int_string(config_text, "model_context_window") {
        profile.context_window = value;
    }
    if let Some(value) = root_positive_int_string(config_text, "model_auto_compact_token_limit") {
        profile.auto_compact_limit = value;
    }
}

fn root_positive_int_string(config_text: &str, key: &str) -> Option<String> {
    if let Ok(doc) = parse_toml_document(config_text) {
        if let Some(value) = doc
            .get(key)
            .and_then(Item::as_value)
            .and_then(toml_edit::Value::as_integer)
            .filter(|value| *value > 0)
        {
            return Some(value.to_string());
        }
    }

    root_key_value(config_text, key)
        .and_then(|value| value.split('#').next())
        .map(str::trim)
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.to_string())
}

fn toml_value_is_subset(target: &toml_edit::Value, source: &toml_edit::Value) -> bool {
    match (target, source) {
        (toml_edit::Value::String(target), toml_edit::Value::String(source)) => {
            target.value() == source.value()
        }
        (toml_edit::Value::Integer(target), toml_edit::Value::Integer(source)) => {
            target.value() == source.value()
        }
        (toml_edit::Value::Float(target), toml_edit::Value::Float(source)) => {
            target.value() == source.value()
        }
        (toml_edit::Value::Boolean(target), toml_edit::Value::Boolean(source)) => {
            target.value() == source.value()
        }
        (toml_edit::Value::Datetime(target), toml_edit::Value::Datetime(source)) => {
            target.value() == source.value()
        }
        (toml_edit::Value::Array(target), toml_edit::Value::Array(source)) => {
            toml_array_contains_subset(target, source)
        }
        (toml_edit::Value::InlineTable(target), toml_edit::Value::InlineTable(source)) => {
            source.iter().all(|(key, source_item)| {
                target
                    .get(key)
                    .is_some_and(|target_item| toml_value_is_subset(target_item, source_item))
            })
        }
        _ => false,
    }
}

fn toml_array_contains_subset(target: &toml_edit::Array, source: &toml_edit::Array) -> bool {
    let mut matched = vec![false; target.len()];
    let target_items: Vec<&toml_edit::Value> = target.iter().collect();

    source.iter().all(|source_item| {
        if let Some((index, _)) = target_items
            .iter()
            .enumerate()
            .find(|(index, target_item)| {
                !matched[*index] && toml_value_is_subset(target_item, source_item)
            })
        {
            matched[index] = true;
            true
        } else {
            false
        }
    })
}

fn toml_remove_array_items(target: &mut toml_edit::Array, source: &toml_edit::Array) {
    for source_item in source.iter() {
        let index = {
            let target_items: Vec<&toml_edit::Value> = target.iter().collect();
            target_items
                .iter()
                .enumerate()
                .find(|(_, target_item)| toml_value_is_subset(target_item, source_item))
                .map(|(index, _)| index)
        };

        if let Some(index) = index {
            target.remove(index);
        }
    }
}

fn merge_toml_item(target: &mut Item, source: &Item) {
    if let Some(source_table) = source.as_table_like() {
        if let Some(target_table) = target.as_table_like_mut() {
            merge_toml_table_like(target_table, source_table);
            return;
        }
    }

    *target = source.clone();
}

fn merge_toml_table_like(target: &mut dyn TableLike, source: &dyn TableLike) {
    for (key, source_item) in source.iter() {
        match target.get_mut(key) {
            Some(target_item) => merge_toml_item(target_item, source_item),
            None => {
                target.insert(key, source_item.clone());
            }
        }
    }
}

fn remove_toml_item(target: &mut Item, source: &Item) {
    if let Some(source_table) = source.as_table_like() {
        if let Some(target_table) = target.as_table_like_mut() {
            remove_toml_table_like(target_table, source_table);
            if target_table.is_empty() {
                *target = Item::None;
            }
            return;
        }
    }

    if let Some(source_value) = source.as_value() {
        let mut remove_item = false;

        if let Some(target_value) = target.as_value_mut() {
            match (target_value, source_value) {
                (toml_edit::Value::Array(target_arr), toml_edit::Value::Array(source_arr)) => {
                    toml_remove_array_items(target_arr, source_arr);
                    remove_item = target_arr.is_empty();
                }
                (target_value, source_value)
                    if toml_value_is_subset(target_value, source_value) =>
                {
                    remove_item = true;
                }
                _ => {}
            }
        }

        if remove_item {
            *target = Item::None;
        }
    }
}

fn remove_toml_table_like(target: &mut dyn TableLike, source: &dyn TableLike) {
    let keys: Vec<String> = source.iter().map(|(key, _)| key.to_string()).collect();

    for key in keys {
        let mut remove_key = false;
        if let (Some(target_item), Some(source_item)) = (target.get_mut(&key), source.get(&key)) {
            remove_toml_item(target_item, source_item);
            remove_key = target_item.is_none()
                || target_item
                    .as_table_like()
                    .is_some_and(|table_like| table_like.is_empty());
        }

        if remove_key {
            target.remove(&key);
        }
    }
}

fn normalize_optional_toml(doc: DocumentMut) -> String {
    let contents = doc.to_string();
    if contents.trim().is_empty() {
        String::new()
    } else {
        ensure_trailing_newline(contents)
    }
}

fn list_context_entries_for_table(doc: &DocumentMut, table_name: &str) -> Vec<CodexContextEntry> {
    let Some(table) = doc.get(table_name).and_then(Item::as_table) else {
        return Vec::new();
    };
    table
        .iter()
        .filter_map(|(id, item)| {
            let table = item.as_table()?;
            let body = table_body_to_string(table);
            Some(CodexContextEntry {
                id: id.to_string(),
                kind: context_kind_name(table_name).to_string(),
                title: id.to_string(),
                summary: context_entry_summary(&body),
                toml_body: body,
                enabled: context_entry_enabled(table),
            })
        })
        .collect()
}

fn table_body_to_string(table: &Table) -> String {
    let mut doc = DocumentMut::new();
    merge_toml_table_like(doc.as_table_mut(), table);
    normalize_optional_toml(doc)
}

fn context_table_name(kind: &str) -> anyhow::Result<&'static str> {
    match kind {
        "mcp" | "mcpServer" | "mcpServers" => Ok("mcp_servers"),
        "skill" | "skills" => Ok("skills"),
        "plugin" | "plugins" => Ok("plugins"),
        other => anyhow::bail!("未知上下文类型：{other}"),
    }
}

fn context_kind_name(table: &str) -> &'static str {
    match table {
        "mcp_servers" => "mcp",
        "skills" => "skill",
        "plugins" => "plugin",
        _ => "unknown",
    }
}

fn context_entry_summary(body: &str) -> String {
    body.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .unwrap_or("")
        .chars()
        .take(96)
        .collect()
}

fn context_entry_enabled(table: &Table) -> bool {
    if table
        .get("enabled")
        .and_then(|value| value.as_bool())
        .is_some_and(|enabled| !enabled)
    {
        return false;
    }
    if table
        .get("disabled")
        .and_then(|value| value.as_bool())
        .is_some_and(|disabled| disabled)
    {
        return false;
    }
    true
}

fn set_provider_id(doc: &mut DocumentMut, provider_id: &str) {
    doc["model_provider"] = toml_edit::value(provider_id);
}

fn restore_profile_provider_id_for_backfill(
    live_config: &str,
    template_config: &str,
) -> anyhow::Result<String> {
    let Some(template_provider_id) = provider_id_with_table_from_config(template_config)? else {
        return Ok(ensure_trailing_newline(live_config.to_string()));
    };
    if live_config.trim().is_empty() {
        return Ok(ensure_trailing_newline(live_config.to_string()));
    }

    let mut doc = parse_toml_document(live_config)?;
    let Some(live_provider_id) = active_provider_id(&doc) else {
        return Ok(ensure_trailing_newline(doc.to_string()));
    };
    if live_provider_id == template_provider_id {
        return Ok(ensure_trailing_newline(doc.to_string()));
    }
    if live_provider_id != RELAY_PROVIDER || template_provider_id == RELAY_PROVIDER {
        return Ok(ensure_trailing_newline(doc.to_string()));
    }
    if !provider_table_exists(&doc, &live_provider_id) {
        return Ok(ensure_trailing_newline(doc.to_string()));
    }

    rename_provider_table(&mut doc, &live_provider_id, &template_provider_id);
    rewrite_profile_provider_refs(&mut doc, &live_provider_id, &template_provider_id);
    set_provider_id(&mut doc, &template_provider_id);
    Ok(ensure_trailing_newline(doc.to_string()))
}

fn provider_id_with_table_from_config(config_text: &str) -> anyhow::Result<Option<String>> {
    if config_text.trim().is_empty() {
        return Ok(None);
    }
    let doc = parse_toml_document(config_text)?;
    let Some(provider_id) = active_provider_id(&doc) else {
        return Ok(None);
    };
    Ok(provider_table_exists(&doc, &provider_id).then_some(provider_id))
}

fn restore_profile_credentials_after_backfill(
    profile: &mut RelayProfile,
    template_auth: &str,
    template_api_key: &str,
    live_auth: &str,
) -> anyhow::Result<()> {
    if profile.relay_mode == crate::settings::RelayMode::PureApi {
        profile.config_contents =
            set_experimental_bearer_token_in_config(&profile.config_contents, template_api_key)?;
        profile.auth_contents =
            set_openai_api_key_in_auth_contents(template_auth, template_api_key)?;
        profile.api_key = template_api_key.trim().to_string();
        return Ok(());
    }

    if profile.relay_mode == crate::settings::RelayMode::Official && profile.official_mix_api_key {
        profile.auth_contents = remove_openai_api_key_from_auth_contents(live_auth)?;
        profile.config_contents =
            set_experimental_bearer_token_in_config(&profile.config_contents, template_api_key)?;
        profile.api_key = template_api_key.trim().to_string();
        return Ok(());
    }

    profile.auth_contents = live_auth.to_string();
    let Some(token) = experimental_bearer_token_from_config(&profile.config_contents)? else {
        return Ok(());
    };
    profile.api_key = token.clone();

    if !profile.auth_contents.trim().is_empty() {
        if codex_auth_api_key(&profile.auth_contents).is_none() {
            return Ok(());
        }
        profile.config_contents =
            remove_experimental_bearer_token_from_config(&profile.config_contents)?;
        return Ok(());
    }

    profile.config_contents =
        remove_experimental_bearer_token_from_config(&profile.config_contents)?;

    profile.auth_contents = set_openai_api_key_in_auth_contents(template_auth, &token)?;
    Ok(())
}

fn validate_relay_test_response(
    protocol: RelayProtocol,
    http_status: u16,
    response_text: &str,
) -> anyhow::Result<()> {
    if !(200..300).contains(&http_status) {
        let preview = response_text.trim().chars().take(320).collect::<String>();
        if preview.is_empty() {
            anyhow::bail!("中转真实请求返回 HTTP {http_status}");
        }
        anyhow::bail!("中转真实请求返回 HTTP {http_status}：{preview}");
    }
    if response_text.trim().is_empty() {
        anyhow::bail!("中转返回 HTTP {http_status}，但响应正文为空");
    }

    let payload: Value = serde_json::from_str(response_text)
        .with_context(|| format!("中转返回 HTTP {http_status}，但正文不是有效 JSON"))?;
    if let Some(error) = payload.get("error").filter(|value| !value.is_null()) {
        anyhow::bail!("中转返回成功状态码，但正文包含错误：{error}");
    }

    let has_text = match protocol {
        RelayProtocol::Responses => {
            if let Some(status) = payload.get("status").and_then(Value::as_str)
                && status != "completed"
            {
                anyhow::bail!("Responses 探测未完成，返回状态为 {status}");
            }
            response_text_from_responses(&payload)
        }
        RelayProtocol::ChatCompletions => response_text_from_chat_completions(&payload),
    };
    if !has_text {
        anyhow::bail!("中转返回 HTTP {http_status}，但没有可消费的模型文本输出");
    }
    Ok(())
}

fn validate_relay_stream_test_response(
    protocol: RelayProtocol,
    http_status: u16,
    response_text: &str,
) -> anyhow::Result<()> {
    if !(200..300).contains(&http_status) {
        let preview = response_text.trim().chars().take(320).collect::<String>();
        if preview.is_empty() {
            anyhow::bail!("中转真实流式请求返回 HTTP {http_status}");
        }
        anyhow::bail!("中转真实流式请求返回 HTTP {http_status}：{preview}");
    }
    if response_text.trim().is_empty() {
        anyhow::bail!("中转流式请求返回 HTTP {http_status}，但响应正文为空");
    }

    let mut saw_event = false;
    let mut saw_completed = false;
    let mut saw_done = false;
    let mut saw_text = false;
    for line in response_text.lines() {
        let Some(data) = line.trim_end_matches('\r').strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() {
            continue;
        }
        if data == "[DONE]" {
            saw_done = true;
            continue;
        }

        let payload: Value = serde_json::from_str(data)
            .with_context(|| "中转流式响应包含无法解析的 SSE data JSON")?;
        saw_event = true;
        if let Some(error) = payload.get("error").filter(|value| !value.is_null()) {
            anyhow::bail!("中转流式响应包含错误：{error}");
        }

        match protocol {
            RelayProtocol::Responses => {
                let event_type = payload
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if matches!(
                    event_type,
                    "error" | "response.failed" | "response.incomplete"
                ) {
                    anyhow::bail!("Responses 流式探测收到失败终止事件：{event_type}");
                }
                if event_type == "response.completed" {
                    let response = payload.get("response").unwrap_or(&payload);
                    if let Some(status) = response.get("status").and_then(Value::as_str)
                        && status != "completed"
                    {
                        anyhow::bail!("Responses 流式探测完成事件状态异常：{status}");
                    }
                    saw_completed = true;
                    saw_text |= response_text_from_responses(response);
                }
                saw_text |= ["delta", "text"]
                    .into_iter()
                    .filter_map(|key| payload.get(key).and_then(Value::as_str))
                    .any(|text| !text.trim().is_empty());
            }
            RelayProtocol::ChatCompletions => {
                if payload.get("type").and_then(Value::as_str) == Some("error") {
                    anyhow::bail!("Chat Completions 流式探测收到错误事件");
                }
                saw_text |= response_text_from_chat_stream(&payload);
            }
        }
    }

    if !saw_event {
        anyhow::bail!("中转返回 HTTP {http_status}，但正文不是可消费的 SSE 事件流");
    }
    match protocol {
        RelayProtocol::Responses if !saw_completed => {
            anyhow::bail!("Responses 流式连接在 response.completed 之前结束");
        }
        RelayProtocol::ChatCompletions if !saw_done => {
            anyhow::bail!("Chat Completions 流式连接在 [DONE] 之前结束");
        }
        _ => {}
    }
    if !saw_text {
        anyhow::bail!("中转返回 HTTP {http_status}，但流中没有可消费的模型文本输出");
    }
    Ok(())
}

fn response_text_from_responses(payload: &Value) -> bool {
    if payload
        .get("output_text")
        .and_then(Value::as_str)
        .is_some_and(|text| !text.trim().is_empty())
    {
        return true;
    }
    payload
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("content").and_then(Value::as_array))
        .flatten()
        .any(|content| {
            ["text", "output_text"]
                .into_iter()
                .filter_map(|key| content.get(key).and_then(Value::as_str))
                .any(|text| !text.trim().is_empty())
        })
}

fn response_text_from_chat_completions(payload: &Value) -> bool {
    payload
        .get("choices")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|choice| choice.get("message"))
        .filter_map(|message| message.get("content"))
        .any(|content| match content {
            Value::String(text) => !text.trim().is_empty(),
            Value::Array(parts) => parts.iter().any(|part| {
                part.get("text")
                    .and_then(Value::as_str)
                    .is_some_and(|text| !text.trim().is_empty())
            }),
            _ => false,
        })
}

fn response_text_from_chat_stream(payload: &Value) -> bool {
    payload
        .get("choices")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|choice| choice.get("delta").or_else(|| choice.get("message")))
        .filter_map(|message| message.get("content"))
        .any(|content| match content {
            Value::String(text) => !text.trim().is_empty(),
            Value::Array(parts) => parts.iter().any(|part| {
                part.get("text")
                    .and_then(Value::as_str)
                    .is_some_and(|text| !text.trim().is_empty())
            }),
            _ => false,
        })
}

fn is_cc_switch_model_catalog(path: &str) -> bool {
    path.trim()
        .replace('\\', "/")
        .rsplit('/')
        .next()
        .is_some_and(|name| name.eq_ignore_ascii_case(CC_SWITCH_MODEL_CATALOG_FILENAME))
}

fn set_openai_api_key_in_auth_contents(
    auth_contents: &str,
    api_key: &str,
) -> anyhow::Result<String> {
    let mut auth = if auth_contents.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str::<Value>(auth_contents).with_context(|| "auth.json JSON 解析失败")?
    };
    if !auth.is_object() {
        auth = json!({});
    }
    if let Some(auth_object) = auth.as_object_mut() {
        if api_key.trim().is_empty() {
            auth_object.remove("OPENAI_API_KEY");
        } else {
            auth_object.insert(
                "OPENAI_API_KEY".to_string(),
                Value::String(api_key.trim().to_string()),
            );
        }
    } else {
        anyhow::bail!("auth.json 必须是 JSON 对象");
    }
    Ok(format!("{}\n", serde_json::to_string_pretty(&auth)?))
}

fn set_experimental_bearer_token_in_config(
    config_contents: &str,
    api_key: &str,
) -> anyhow::Result<String> {
    let mut doc = parse_toml_document(config_contents)?;
    let provider_id = active_or_default_provider_id(&doc);
    let provider = ensure_provider_table(&mut doc, &provider_id)?;
    if api_key.trim().is_empty() {
        provider.remove("experimental_bearer_token");
    } else {
        provider["experimental_bearer_token"] = toml_edit::value(api_key.trim());
    }
    Ok(move_model_providers_before_profiles(
        &ensure_trailing_newline(doc.to_string()),
    ))
}

fn sync_profile_mode_from_backfilled_live(profile: &mut RelayProfile) {
    if profile.relay_mode == crate::settings::RelayMode::Official && !profile.official_mix_api_key {
        return;
    }

    if codex_auth_api_key(&profile.auth_contents)
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        profile.relay_mode = crate::settings::RelayMode::PureApi;
        profile.official_mix_api_key = false;
        return;
    }

    let has_provider_endpoint = provider_string_from_config(&profile.config_contents, "base_url")
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());
    if has_provider_endpoint || !profile.api_key.trim().is_empty() {
        profile.relay_mode = crate::settings::RelayMode::Official;
        profile.official_mix_api_key = true;
    }
}

fn official_profile_auth_for_switch(home: &Path, auth_contents: &str) -> anyhow::Result<String> {
    let source = if auth_contents.trim().is_empty() {
        read_optional_text(&home.join("auth.json"))?
    } else {
        auth_contents.to_string()
    };
    remove_openai_api_key_from_auth_contents(&source)
}

fn codex_auth_api_key(auth_contents: &str) -> Option<String> {
    let auth: Value = serde_json::from_str(auth_contents).ok()?;
    auth.get("OPENAI_API_KEY")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(ToString::to_string)
}

/// 解析 profile 實際使用的模型：優先取 config.toml 裡的 `model =`，
/// 否則退回 profile.model 欄位。供應商測試用它做回退，避免串到別家供應商的模型名。
pub fn relay_profile_model(profile: &RelayProfile) -> String {
    root_key_string(&profile.config_contents, "model")
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| profile.model.trim().to_string())
}

pub fn relay_profile_base_url(profile: &RelayProfile) -> String {
    if profile.relay_mode == crate::settings::RelayMode::Aggregate {
        return crate::protocol_proxy::local_responses_proxy_base_url(
            crate::protocol_proxy::DEFAULT_PROTOCOL_PROXY_PORT,
        );
    }
    if profile.protocol == RelayProtocol::ChatCompletions {
        if !profile.upstream_base_url.trim().is_empty() {
            return profile.upstream_base_url.trim().to_string();
        }
        if let Some(value) = root_key_string(&profile.config_contents, CHAT_UPSTREAM_BASE_URL_KEY)
            .filter(|value| !value.trim().is_empty())
        {
            return value;
        }
        if !profile.base_url.trim().is_empty() {
            return profile.base_url.trim().to_string();
        }
    }
    let provider_base_url = provider_string_from_config(&profile.config_contents, "base_url")
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_default();
    if profile.protocol == RelayProtocol::ChatCompletions
        && provider_base_url
            == crate::protocol_proxy::local_responses_proxy_base_url(
                crate::protocol_proxy::DEFAULT_PROTOCOL_PROXY_PORT,
            )
    {
        String::new()
    } else if !provider_base_url.is_empty() {
        provider_base_url
    } else {
        profile.base_url.trim().to_string()
    }
}

pub fn relay_profile_api_key(profile: &RelayProfile) -> String {
    if profile.relay_mode == crate::settings::RelayMode::Aggregate {
        return "codex-plus-aggregate".to_string();
    }
    // Codex resolves provider-scoped credentials before ambient auth. In
    // particular, experimental_bearer_token wins over auth.json, including
    // for a provider that also requires an existing OpenAI login. Keep the
    // Manager's pre/post-write probes on that same credential path so a stale
    // OPENAI_API_KEY cannot replace or falsely verify a Mixed API key.
    experimental_bearer_token_from_config(&profile.config_contents)
        .ok()
        .flatten()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| codex_auth_api_key(&profile.auth_contents))
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| profile.api_key.trim().to_string())
}

fn complete_relay_profile_config(profile: &RelayProfile) -> anyhow::Result<String> {
    let mut doc = parse_toml_document(&profile.config_contents)?;
    let provider_id = active_or_default_provider_id(&doc);
    set_provider_id(&mut doc, &provider_id);
    // Activating a saved Codex profile can override model_provider after this
    // function has completed the root provider table. Preserve profile
    // definitions but clear the active selection for deterministic relay startup.
    doc.as_table_mut().remove("profile");

    let mut model = relay_profile_model(profile);
    // 若用户未填写默认模型，但 model_list 有内容，则取第一条作为默认 model，
    // 避免 codex 启动时回退到历史会话中带后缀的模型名。
    if model.trim().is_empty() && !profile.model_list.trim().is_empty() {
        if let Some(first) = profile
            .model_list
            .split(['\r', '\n', ','])
            .map(str::trim)
            .find(|value| !value.is_empty())
        {
            model = crate::model_suffix::parse_model_suffix(first).0;
        }
    }
    // 若用户把后缀语法（如 deepseek-v4-flash[1M]）写在 model 字段，
    // 写入 config.toml 前需剥离后缀；codex 本身不理解后缀，只会按原串匹配 catalog slug。
    let (model, _) = crate::model_suffix::parse_model_suffix(&model);
    if !model.trim().is_empty() {
        doc["model"] = toml_edit::value(model.trim());
    }

    let base_url = relay_profile_base_url(profile);
    let api_key = relay_profile_api_key(profile);
    doc.as_table_mut().remove(CHAT_UPSTREAM_BASE_URL_KEY);
    retain_only_provider_table(&mut doc, &provider_id);
    for legacy_provider in LEGACY_RELAY_PROVIDERS {
        if provider_id != *legacy_provider {
            remove_provider_table(&mut doc, legacy_provider);
        }
    }
    let provider = ensure_provider_table(&mut doc, &provider_id)?;
    if provider
        .get("name")
        .and_then(Item::as_str)
        .map(str::trim)
        .is_none_or(str::is_empty)
    {
        provider["name"] = toml_edit::value(provider_id.as_str());
    }
    if provider
        .get("wire_api")
        .and_then(Item::as_str)
        .map(str::trim)
        .is_none_or(str::is_empty)
    {
        provider["wire_api"] = toml_edit::value("responses");
    }
    if profile.relay_mode == crate::settings::RelayMode::PureApi {
        provider.remove("requires_openai_auth");
    } else if provider
        .get("requires_openai_auth")
        .and_then(Item::as_bool)
        .is_none()
    {
        provider["requires_openai_auth"] = toml_edit::value(true);
    }
    let provider_base_url = codex_base_url_for_protocol(
        base_url.trim(),
        profile.protocol,
        crate::protocol_proxy::DEFAULT_PROTOCOL_PROXY_PORT,
    );
    if !provider_base_url.trim().is_empty() {
        provider["base_url"] = toml_edit::value(provider_base_url.trim());
    }
    if !api_key.trim().is_empty() {
        provider["experimental_bearer_token"] = toml_edit::value(api_key.trim());
    } else {
        provider.remove("experimental_bearer_token");
    }

    Ok(move_model_providers_before_profiles(
        &ensure_trailing_newline(doc.to_string()),
    ))
}

pub fn normalize_relay_profile_for_storage(profile: &mut RelayProfile) -> anyhow::Result<()> {
    if profile.model_windows.trim().is_empty() && profile.model_list.contains('[') {
        let (clean_list, windows) =
            crate::model_suffix::migrate_model_list_with_suffixes(&profile.model_list);
        profile.model_list = clean_list;
        profile.model_windows = serde_json::to_string(&windows).unwrap_or_default();
    }
    if profile.relay_mode == crate::settings::RelayMode::Official && !profile.official_mix_api_key {
        let has_api_config = !profile.base_url.trim().is_empty()
            || !profile.api_key.trim().is_empty()
            || codex_auth_api_key(&profile.auth_contents).is_some()
            || config_has_model_provider(profile.config_contents.as_str());
        if has_api_config {
            profile.config_contents.clear();
        }
        if !profile.model_list.trim().is_empty() {
            profile.model_list = merge_model_into_model_list(&profile.model, &profile.model_list);
        }
        profile.model.clear();
        profile.base_url.clear();
        profile.upstream_base_url.clear();
        profile.api_key.clear();
        if auth_contents_looks_like_chatgpt_auth(&profile.auth_contents) {
            profile.auth_contents =
                remove_openai_api_key_from_auth_contents(&profile.auth_contents)?;
        } else {
            profile.auth_contents.clear();
        }
        return Ok(());
    }
    let source_base_url = relay_profile_base_url(profile);
    let source_api_key = relay_profile_api_key(profile);
    if !profile.config_contents.trim().is_empty()
        || profile.relay_mode == crate::settings::RelayMode::PureApi
        || profile.official_mix_api_key
    {
        profile.config_contents = complete_relay_profile_config(profile)?;
    }
    if profile.relay_mode == crate::settings::RelayMode::PureApi
        && profile.auth_contents.trim().is_empty()
        && !source_api_key.trim().is_empty()
    {
        profile.auth_contents = serde_json::to_string_pretty(&json!({
            "OPENAI_API_KEY": source_api_key.trim()
        }))?;
    }
    if profile.relay_mode == crate::settings::RelayMode::Official {
        profile.auth_contents = remove_openai_api_key_from_auth_contents(&profile.auth_contents)?;
    }
    profile.model = relay_profile_model(profile);
    profile.model_list = merge_model_into_model_list(&profile.model, &profile.model_list);
    profile.upstream_base_url = source_base_url.clone();
    profile.base_url = source_base_url;
    profile.api_key = relay_profile_api_key(profile);
    Ok(())
}

fn remove_openai_api_key_from_auth_contents(auth_contents: &str) -> anyhow::Result<String> {
    if auth_contents.trim().is_empty() {
        return Ok(String::new());
    }
    let mut value =
        serde_json::from_str::<Value>(auth_contents).with_context(|| "auth.json JSON 解析失败")?;
    let Some(object) = value.as_object_mut() else {
        anyhow::bail!("auth.json 必须是 JSON 对象");
    };
    object.remove("OPENAI_API_KEY");
    if object.is_empty() {
        return Ok(String::new());
    }
    Ok(format!("{}\n", serde_json::to_string_pretty(&value)?))
}

fn merge_model_into_model_list(model: &str, model_list: &str) -> String {
    let model = model.trim();
    let mut models = Vec::new();
    if !model.is_empty() {
        models.push(model.to_string());
    }
    for item in model_list.split(['\r', '\n', ',']).map(str::trim) {
        if !item.is_empty() && !models.iter().any(|existing| existing == item) {
            models.push(item.to_string());
        }
    }
    models.join("\n")
}

fn config_has_model_provider(config_contents: &str) -> bool {
    parse_toml_document(config_contents)
        .ok()
        .and_then(|doc| {
            doc.get("model_provider")
                .and_then(Item::as_str)
                .map(str::to_string)
        })
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

fn auth_contents_looks_like_chatgpt_auth(contents: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(contents) else {
        return false;
    };
    let is_chatgpt = value
        .get("auth_mode")
        .and_then(Value::as_str)
        .map(|mode| mode.eq_ignore_ascii_case("chatgpt"))
        .unwrap_or(false);
    is_chatgpt
        && value
            .get("tokens")
            .map(tokens_have_login_secret)
            .unwrap_or(false)
}

fn provider_string_from_config(config_contents: &str, key: &str) -> Option<String> {
    let doc = parse_toml_document(config_contents).ok()?;
    let active = active_provider_id(&doc);
    if let Some(provider_id) = active.as_deref() {
        if let Some(value) = doc
            .get("model_providers")
            .and_then(Item::as_table)
            .and_then(|providers| providers.get(provider_id))
            .and_then(Item::as_table)
            .and_then(|provider| provider.get(key))
            .and_then(Item::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Some(value.to_string());
        }
    }

    for provider in provider_tables(&doc) {
        if let Some(value) = provider
            .get(key)
            .and_then(Item::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Some(value.to_string());
        }
    }
    None
}

fn experimental_bearer_token_from_config(config_contents: &str) -> anyhow::Result<Option<String>> {
    let doc = parse_toml_document(config_contents)?;
    if let Some(provider_id) = active_provider_id(&doc) {
        if let Some(token) = doc
            .get("model_providers")
            .and_then(Item::as_table)
            .and_then(|providers| providers.get(&provider_id))
            .and_then(Item::as_table)
            .and_then(|provider| provider.get("experimental_bearer_token"))
            .and_then(Item::as_str)
            .map(str::trim)
            .filter(|token| !token.is_empty())
        {
            return Ok(Some(token.to_string()));
        }
    }
    Ok(None)
}

fn remove_experimental_bearer_token_from_config(config_contents: &str) -> anyhow::Result<String> {
    let mut doc = parse_toml_document(config_contents)?;
    if let Some(providers) = doc.get_mut("model_providers").and_then(Item::as_table_mut) {
        for (_, item) in providers.iter_mut() {
            if let Some(provider) = item.as_table_like_mut() {
                provider.remove("experimental_bearer_token");
            }
        }
    }
    Ok(ensure_trailing_newline(doc.to_string()))
}

fn provider_tables(doc: &DocumentMut) -> Vec<&dyn TableLike> {
    let mut tables: Vec<&dyn TableLike> = Vec::new();
    if let Some(providers) = doc.get("model_providers").and_then(Item::as_table) {
        for (_, item) in providers.iter() {
            if let Some(provider) = item.as_table_like() {
                tables.push(provider);
            }
        }
    }
    tables
}

fn ensure_provider_table<'a>(
    doc: &'a mut DocumentMut,
    provider_id: &str,
) -> anyhow::Result<&'a mut Table> {
    let providers = table_mut_or_insert(doc, "model_providers")?;
    if !providers.contains_key(provider_id)
        || providers
            .get(provider_id)
            .and_then(Item::as_table)
            .is_none()
    {
        providers.insert(provider_id, toml_edit::table());
    }
    providers
        .get_mut(provider_id)
        .and_then(Item::as_table_mut)
        .ok_or_else(|| anyhow::anyhow!("model_providers.{provider_id} 必须是 TOML table"))
}

fn remove_provider_table(doc: &mut DocumentMut, provider_id: &str) {
    if let Some(providers) = doc.get_mut("model_providers").and_then(Item::as_table_mut) {
        providers.remove(provider_id);
        if providers.is_empty() {
            doc.as_table_mut().remove("model_providers");
        }
    }
}

fn retain_only_provider_table(doc: &mut DocumentMut, provider_id: &str) {
    if let Some(providers) = doc.get_mut("model_providers").and_then(Item::as_table_mut) {
        let provider = providers
            .remove(provider_id)
            .unwrap_or_else(toml_edit::table);
        providers.clear();
        providers.insert(provider_id, provider);
    }
}

fn rename_provider_table(doc: &mut DocumentMut, from: &str, to: &str) {
    if from == to {
        return;
    }
    if let Some(providers) = doc.get_mut("model_providers").and_then(Item::as_table_mut) {
        let moved = providers.remove(from).unwrap_or_else(toml_edit::table);
        providers.insert(to, moved);
    }
}

fn rewrite_profile_provider_refs(doc: &mut DocumentMut, from: &str, to: &str) {
    let Some(profiles) = doc.get_mut("profiles").and_then(Item::as_table_mut) else {
        return;
    };
    for (_, item) in profiles.iter_mut() {
        let Some(profile) = item.as_table_mut() else {
            continue;
        };
        if profile
            .get("model_provider")
            .and_then(Item::as_str)
            .is_some_and(|provider| provider == from)
        {
            profile.insert("model_provider", toml_edit::value(to));
        }
    }
}

fn read_optional_text(path: &Path) -> anyhow::Result<String> {
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error.into()),
    }
}

fn read_optional_bytes(path: &Path) -> anyhow::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn restore_optional_file(path: &Path, contents: Option<&[u8]>) -> anyhow::Result<()> {
    match contents {
        Some(contents) => crate::settings::atomic_write(path, contents),
        None => match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        },
    }
}

fn create_live_backup(
    home: &Path,
    config: Option<&[u8]>,
    auth: Option<&[u8]>,
) -> anyhow::Result<Option<String>> {
    if config.is_none() && auth.is_none() {
        return Ok(None);
    }

    let backup_root = home.join("backups");
    std::fs::create_dir_all(&backup_root)?;
    let transaction_id = uuid::Uuid::new_v4();
    let backup_dir = backup_root.join(format!(
        "codex-plus-live-{}-{transaction_id}",
        timestamp_millis()
    ));
    let staging_dir = backup_root.join(format!(".codex-plus-live-{transaction_id}.tmp"));
    std::fs::create_dir(&staging_dir)?;
    if let Some(config) = config {
        crate::settings::atomic_write(&staging_dir.join("config.toml"), config)?;
    }
    if let Some(auth) = auth {
        crate::settings::atomic_write(&staging_dir.join("auth.json"), auth)?;
    }
    let manifest = serde_json::to_vec_pretty(&json!({
        "schemaVersion": 1,
        "configExisted": config.is_some(),
        "authExisted": auth.is_some()
    }))?;
    crate::settings::atomic_write(&staging_dir.join("manifest.json"), &manifest)?;
    std::fs::rename(&staging_dir, &backup_dir).with_context(|| {
        format!(
            "无法提交 Codex live 备份 {} -> {}",
            staging_dir.display(),
            backup_dir.display()
        )
    })?;
    Ok(Some(backup_dir.to_string_lossy().to_string()))
}

fn timestamp_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn ensure_trailing_newline(mut contents: String) -> String {
    if !contents.ends_with('\n') {
        contents.push('\n');
    }
    contents
}

fn move_model_providers_before_profiles(contents: &str) -> String {
    let lines = contents.lines().collect::<Vec<_>>();
    let Some(provider_start) = lines
        .iter()
        .position(|line| line.trim_start().starts_with("[model_providers."))
    else {
        return ensure_trailing_newline(contents.to_string());
    };
    let provider_end = lines[provider_start + 1..]
        .iter()
        .position(|line| line.trim_start().starts_with('['))
        .map(|offset| provider_start + 1 + offset)
        .unwrap_or(lines.len());
    let Some(profile_start) = lines
        .iter()
        .position(|line| line.trim_start().starts_with("[profiles."))
    else {
        return ensure_trailing_newline(contents.to_string());
    };
    if provider_start < profile_start {
        return ensure_trailing_newline(contents.to_string());
    }

    let mut output = Vec::with_capacity(lines.len());
    output.extend_from_slice(&lines[..profile_start]);
    output.extend_from_slice(&lines[provider_start..provider_end]);
    if output.last().is_some_and(|line| !line.trim().is_empty()) {
        output.push("");
    }
    output.extend_from_slice(&lines[profile_start..provider_start]);
    output.extend_from_slice(&lines[provider_end..]);
    ensure_trailing_newline(output.join("\n"))
}

fn auth_json_chatgpt_account_label(path: &Path) -> Option<Option<String>> {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return None;
    };
    let Ok(value) = serde_json::from_str::<Value>(&contents) else {
        return None;
    };
    let is_chatgpt = value
        .get("auth_mode")
        .and_then(Value::as_str)
        .map(|mode| mode.eq_ignore_ascii_case("chatgpt"))
        .unwrap_or(false);
    let tokens = value.get("tokens")?;
    if !is_chatgpt || !tokens_have_login_secret(tokens) {
        return None;
    }
    Some(account_label_from_tokens(tokens))
}

fn tokens_have_login_secret(tokens: &Value) -> bool {
    ["access_token", "id_token", "refresh_token"]
        .iter()
        .any(|key| {
            tokens
                .get(*key)
                .and_then(Value::as_str)
                .map(|token| !token.trim().is_empty())
                .unwrap_or(false)
        })
}

fn account_label_from_tokens(tokens: &Value) -> Option<String> {
    ["id_token", "access_token"].iter().find_map(|key| {
        tokens
            .get(*key)
            .and_then(Value::as_str)
            .and_then(account_label_from_jwt)
    })
}

fn account_label_from_jwt(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    use base64::Engine;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.as_bytes())
        .ok()
        .or_else(|| {
            base64::engine::general_purpose::URL_SAFE
                .decode(payload.as_bytes())
                .ok()
        })?;
    let value: Value = serde_json::from_slice(&decoded).ok()?;
    value
        .get("email")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .get("https://api.openai.com/profile")
                .and_then(|profile| profile.get("email"))
                .and_then(Value::as_str)
        })
        .or_else(|| value.get("name").and_then(Value::as_str))
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct MockRelayResponse {
        status: &'static str,
        content_type: &'static str,
        body: &'static str,
    }

    async fn spawn_mock_relay(
        responses: Vec<MockRelayResponse>,
    ) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut requests = Vec::with_capacity(responses.len());
            for response in responses {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut buffer = [0_u8; 4096];
                loop {
                    let read = stream.read(&mut buffer).await.unwrap();
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n")
                    else {
                        continue;
                    };
                    let header_end = header_end + 4;
                    let headers = String::from_utf8_lossy(&request[..header_end]);
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or_default();
                    if request.len() >= header_end + content_length {
                        break;
                    }
                }
                requests.push(String::from_utf8_lossy(&request).to_string());
                let reply = format!(
                    "HTTP/1.1 {}\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    response.status,
                    response.content_type,
                    response.body.len(),
                    response.body
                );
                stream.write_all(reply.as_bytes()).await.unwrap();
            }
            requests
        });
        (format!("http://{address}"), server)
    }

    fn mock_probe_profile(base_url: String) -> RelayProfile {
        RelayProfile {
            base_url,
            api_key: "sk-mock-probe".to_string(),
            protocol: RelayProtocol::Responses,
            ..RelayProfile::default()
        }
    }

    #[test]
    fn backfill_relay_profile_from_home_with_common_restores_template_provider_id() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("config.toml"),
            "model_provider = \"custom\"\nmodel = \"gpt-image-2\"\n\n[model_providers.custom]\nname = \"custom\"\nwire_api = \"responses\"\nrequires_openai_auth = true\nbase_url = \"https://ahg.codes\"\n",
        )
        .unwrap();
        std::fs::write(temp.path().join("auth.json"), "{}\n").unwrap();

        let mut profile = RelayProfile {
            relay_mode: crate::settings::RelayMode::PureApi,
            protocol: crate::settings::RelayProtocol::Responses,
            config_contents: "model_provider = \"ai\"\nmodel = \"gpt-image-2\"\n\n[model_providers.ai]\nname = \"ai\"\nwire_api = \"responses\"\nrequires_openai_auth = true\nbase_url = \"https://ahg.codes\"\n"
                .to_string(),
            auth_contents: "{}\n".to_string(),
            ..RelayProfile::default()
        };
        let mut common = String::new();

        backfill_relay_profile_from_home_with_common(temp.path(), &mut profile, &mut common)
            .unwrap();

        assert!(profile.config_contents.contains("model_provider = \"ai\""));
        assert!(profile.config_contents.contains("[model_providers.ai]"));
        assert!(!profile.config_contents.contains("[model_providers.custom]"));
    }

    #[test]
    fn live_probe_profile_never_falls_back_to_prewrite_endpoint_or_key() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("config.toml"),
            "model_provider = \"custom\"\n[model_providers.custom]\nwire_api = \"responses\"\n",
        )
        .unwrap();
        std::fs::write(temp.path().join("auth.json"), "{}\n").unwrap();
        let source = RelayProfile {
            relay_mode: crate::settings::RelayMode::PureApi,
            base_url: "https://prewrite.example/v1".to_string(),
            upstream_base_url: "https://prewrite-upstream.example/v1".to_string(),
            api_key: "sk-prewrite".to_string(),
            ..RelayProfile::default()
        };

        let persisted = relay_profile_from_live_for_probe(temp.path(), &source).unwrap();

        assert!(persisted.base_url.is_empty());
        assert!(persisted.upstream_base_url.is_empty());
        assert!(persisted.api_key.is_empty());
    }

    #[test]
    fn live_chat_probe_profile_uses_persisted_upstream_and_key() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("config.toml"),
            concat!(
                "model_provider = \"custom\"\n",
                "codex_plus_chat_base_url = \"https://persisted.example/v1\"\n",
                "[model_providers.custom]\n",
                "wire_api = \"responses\"\n",
                "base_url = \"http://127.0.0.1:57321/v1\"\n",
            ),
        )
        .unwrap();
        std::fs::write(
            temp.path().join("auth.json"),
            r#"{"OPENAI_API_KEY":"sk-persisted"}"#,
        )
        .unwrap();
        let source = RelayProfile {
            relay_mode: crate::settings::RelayMode::PureApi,
            protocol: crate::settings::RelayProtocol::ChatCompletions,
            base_url: "https://prewrite.example/v1".to_string(),
            upstream_base_url: "https://prewrite-upstream.example/v1".to_string(),
            api_key: "sk-prewrite".to_string(),
            ..RelayProfile::default()
        };

        let persisted = relay_profile_from_live_for_probe(temp.path(), &source).unwrap();

        assert_eq!(persisted.base_url, "https://persisted.example/v1");
        assert!(persisted.upstream_base_url.is_empty());
        assert_eq!(persisted.api_key, "sk-persisted");
    }

    #[test]
    fn mixed_api_key_matches_codex_provider_token_priority() {
        let profile = RelayProfile {
            relay_mode: crate::settings::RelayMode::MixedApi,
            config_contents: concat!(
                "model_provider = \"custom\"\n",
                "[model_providers.custom]\n",
                "base_url = \"https://relay.example/v1\"\n",
                "requires_openai_auth = true\n",
                "experimental_bearer_token = \"sk-provider\"\n",
            )
            .to_string(),
            auth_contents: concat!(
                "{\"OPENAI_API_KEY\":\"sk-stale-auth\",",
                "\"tokens\":{\"access_token\":\"official\"}}",
            )
            .to_string(),
            api_key: "sk-memory-fallback".to_string(),
            ..RelayProfile::default()
        };

        assert_eq!(relay_profile_api_key(&profile), "sk-provider");
    }

    #[test]
    fn live_mixed_probe_uses_persisted_provider_token_over_stale_auth_key() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("config.toml"),
            concat!(
                "model_provider = \"custom\"\n",
                "[model_providers.custom]\n",
                "base_url = \"https://persisted.example/v1\"\n",
                "requires_openai_auth = true\n",
                "experimental_bearer_token = \"sk-persisted-provider\"\n",
            ),
        )
        .unwrap();
        std::fs::write(
            temp.path().join("auth.json"),
            concat!(
                "{\"OPENAI_API_KEY\":\"sk-stale-auth\",",
                "\"tokens\":{\"access_token\":\"official\"}}",
            ),
        )
        .unwrap();
        let source = RelayProfile {
            relay_mode: crate::settings::RelayMode::MixedApi,
            base_url: "https://prewrite.example/v1".to_string(),
            api_key: "sk-prewrite".to_string(),
            ..RelayProfile::default()
        };

        let persisted = relay_profile_from_live_for_probe(temp.path(), &source).unwrap();

        assert_eq!(persisted.base_url, "https://persisted.example/v1");
        assert_eq!(persisted.api_key, "sk-persisted-provider");
    }

    #[test]
    fn relay_profile_model_prefers_config_then_field_then_empty() {
        // 1. 供應商測試的回退第一級：config.toml 的 model = 優先
        let from_config = RelayProfile {
            config_contents: "model = \"deepseek-v4-flash\"\nmodel_provider = \"custom\"\n"
                .to_string(),
            model: "should-not-be-used".to_string(),
            ..RelayProfile::default()
        };
        assert_eq!(relay_profile_model(&from_config), "deepseek-v4-flash");

        // 2. config 沒寫 model 時退回 profile.model 欄位
        let from_field = RelayProfile {
            config_contents: "model_provider = \"custom\"\n".to_string(),
            model: "deepseek-v4-pro".to_string(),
            ..RelayProfile::default()
        };
        assert_eq!(relay_profile_model(&from_field), "deepseek-v4-pro");

        // 3. 兩者皆空 → 空字串；呼叫端據此才回退到全域 relayTestModel
        let empty = RelayProfile {
            config_contents: String::new(),
            model: String::new(),
            ..RelayProfile::default()
        };
        assert!(relay_profile_model(&empty).trim().is_empty());
    }

    #[test]
    fn relay_probe_rejects_empty_or_incomplete_success_responses() {
        assert!(
            validate_relay_test_response(
                RelayProtocol::Responses,
                401,
                r#"{"error":{"message":"invalid key"}}"#,
            )
            .is_err()
        );
        assert!(validate_relay_test_response(RelayProtocol::Responses, 200, "").is_err());
        assert!(
            validate_relay_test_response(
                RelayProtocol::Responses,
                200,
                r#"{"status":"in_progress","output":[]}"#,
            )
            .is_err()
        );
        assert!(
            validate_relay_test_response(
                RelayProtocol::Responses,
                200,
                r#"{"status":"completed","output":[]}"#,
            )
            .is_err()
        );
    }

    #[test]
    fn relay_probe_accepts_completed_responses_and_chat_text() {
        validate_relay_test_response(
            RelayProtocol::Responses,
            200,
            r#"{"status":"completed","output":[{"content":[{"type":"output_text","text":"OK"}]}]}"#,
        )
        .unwrap();
        validate_relay_test_response(
            RelayProtocol::ChatCompletions,
            200,
            r#"{"choices":[{"message":{"content":"OK"}}]}"#,
        )
        .unwrap();
    }

    #[test]
    fn relay_stream_probe_requires_responses_completion_and_text() {
        let complete = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"OK\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[]}}\n\n"
        );
        validate_relay_stream_test_response(RelayProtocol::Responses, 200, complete).unwrap();

        let missing_completion =
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"OK\"}\n\n";
        let error =
            validate_relay_stream_test_response(RelayProtocol::Responses, 200, missing_completion)
                .unwrap_err();
        assert!(error.to_string().contains("response.completed"));

        let missing_text = "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[]}}\n\n";
        assert!(
            validate_relay_stream_test_response(RelayProtocol::Responses, 200, missing_text)
                .is_err()
        );
    }

    #[test]
    fn relay_stream_probe_rejects_failed_or_non_sse_success() {
        let failed =
            "data: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\"}}\n\n";
        assert!(
            validate_relay_stream_test_response(RelayProtocol::Responses, 200, failed).is_err()
        );
        assert!(
            validate_relay_stream_test_response(
                RelayProtocol::Responses,
                200,
                r#"{"status":"completed","output_text":"OK"}"#,
            )
            .is_err()
        );
    }

    #[test]
    fn relay_stream_probe_accepts_chat_text_with_done() {
        let complete = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"OK\"}}]}\n\n",
            "data: [DONE]\n\n"
        );
        validate_relay_stream_test_response(RelayProtocol::ChatCompletions, 200, complete).unwrap();
    }

    #[tokio::test]
    async fn relay_stream_probe_sends_real_stream_request_with_bearer() {
        let complete = concat!(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"OK\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[]}}\n\n"
        );
        let (base_url, server) = spawn_mock_relay(vec![MockRelayResponse {
            status: "200 OK",
            content_type: "text/event-stream",
            body: complete,
        }])
        .await;

        let result = test_relay_profile_stream(&mock_probe_profile(base_url), "mock-model")
            .await
            .unwrap();
        let requests = server.await.unwrap();

        assert_eq!(result.http_status, 200);
        assert!(requests[0].starts_with("POST /responses HTTP/1.1"));
        assert!(
            requests[0]
                .to_ascii_lowercase()
                .contains("authorization: bearer sk-mock-probe")
        );
        assert!(requests[0].contains(r#""stream":true"#));
        assert!(requests[0].contains(r#""model":"mock-model""#));
    }

    #[tokio::test]
    async fn relay_stream_probe_rejects_body_that_disconnects_before_completion() {
        let (base_url, server) = spawn_mock_relay(vec![MockRelayResponse {
            status: "200 OK",
            content_type: "text/event-stream",
            body: "data: {\"type\":\"response.output_text.delta\",\"delta\":\"OK\"}\n\n",
        }])
        .await;

        let error = test_relay_profile_stream(&mock_probe_profile(base_url), "mock-model")
            .await
            .unwrap_err();
        server.await.unwrap();

        assert!(error.to_string().contains("response.completed"));
    }

    #[tokio::test]
    async fn relay_stream_probe_rejects_http_200_non_sse_and_failed_event() {
        for body in [
            r#"{"status":"completed","output_text":"OK"}"#,
            "data: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\"}}\n\n",
        ] {
            let (base_url, server) = spawn_mock_relay(vec![MockRelayResponse {
                status: "200 OK",
                content_type: "application/json",
                body,
            }])
            .await;

            assert!(
                test_relay_profile_stream(&mock_probe_profile(base_url), "mock-model")
                    .await
                    .is_err()
            );
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn relay_stream_probe_retries_with_v1_after_404() {
        let complete = concat!(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"OK\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[]}}\n\n"
        );
        let (base_url, server) = spawn_mock_relay(vec![
            MockRelayResponse {
                status: "404 Not Found",
                content_type: "application/json",
                body: r#"{"error":"missing v1"}"#,
            },
            MockRelayResponse {
                status: "200 OK",
                content_type: "text/event-stream",
                body: complete,
            },
        ])
        .await;

        let result = test_relay_profile_stream(&mock_probe_profile(base_url), "mock-model")
            .await
            .unwrap();
        let requests = server.await.unwrap();

        assert!(requests[0].starts_with("POST /responses HTTP/1.1"));
        assert!(requests[1].starts_with("POST /v1/responses HTTP/1.1"));
        assert!(result.endpoint.ends_with("/v1/responses"));
        assert!(result.response_preview.contains("建议加上 /v1 前缀"));
    }
}

pub fn root_key_string(contents: &str, key: &str) -> Option<String> {
    root_key_value(contents, key).map(unquote_toml_string)
}

fn root_key_value<'a>(contents: &'a str, key: &str) -> Option<&'a str> {
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            return None;
        }
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        let Some((name, value)) = trimmed.split_once('=') else {
            continue;
        };
        if name.trim() == key {
            return Some(value);
        }
    }
    None
}

fn upsert_model_provider_config(
    contents: &str,
    base_url: &str,
    bearer_token: &str,
    requires_openai_auth: bool,
) -> anyhow::Result<String> {
    let mut doc = parse_toml_document(contents)?;
    let provider_id = active_or_default_provider_id(&doc);
    set_provider_id(&mut doc, &provider_id);
    for legacy_provider in LEGACY_RELAY_PROVIDERS {
        remove_provider_table(&mut doc, legacy_provider);
    }
    if provider_id != RELAY_PROVIDER {
        remove_provider_table(&mut doc, RELAY_PROVIDER);
    }

    let provider = ensure_provider_table(&mut doc, &provider_id)?;
    provider["name"] = toml_edit::value(provider_id.as_str());
    provider["wire_api"] = toml_edit::value("responses");
    if requires_openai_auth {
        provider["requires_openai_auth"] = toml_edit::value(true);
    } else {
        provider.remove("requires_openai_auth");
    }
    provider["base_url"] = toml_edit::value(base_url);
    provider["experimental_bearer_token"] = toml_edit::value(bearer_token);

    Ok(move_model_providers_before_profiles(
        &ensure_trailing_newline(doc.to_string()),
    ))
}

fn remove_table(contents: &str, table: &str) -> String {
    let header = format!("[{table}]");
    let mut lines = Vec::new();
    let mut skipping = false;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if trimmed == header {
                skipping = true;
                continue;
            }
            skipping = false;
        }
        if !skipping {
            lines.push(line.to_string());
        }
    }
    lines.join("\n")
}

fn remove_root_key(contents: &str, key: &str) -> String {
    let mut lines = Vec::new();
    let mut in_root = true;
    for line in contents.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            in_root = false;
        }
        if in_root && root_line_key(line) == Some(key) {
            continue;
        }
        lines.push(line.to_string());
    }
    lines.join("\n")
}

fn unquote_toml_string(value: &str) -> String {
    let value = value.trim();
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
        .unwrap_or(value)
        .to_string()
}

fn root_line_key(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if trimmed.starts_with('#') || trimmed.starts_with('[') {
        return None;
    }
    trimmed.split_once('=').map(|(key, _)| key.trim())
}
