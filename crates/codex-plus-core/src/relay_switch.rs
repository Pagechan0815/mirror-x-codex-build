use std::path::Path;

use anyhow::Context;

use crate::relay_config::{
    backfill_relay_profile_from_home_with_common, capture_relay_live_snapshot,
    restore_relay_live_snapshot, try_relay_config_status_from_home,
};
use crate::settings::{BackendSettings, LaunchMode, RelayMode, RelayProfile, SettingsStore};

pub async fn verify_backend_settings_live(settings: &BackendSettings) -> anyhow::Result<()> {
    let active = settings.active_relay_profile();
    if active.relay_mode == RelayMode::Official && !active.official_mix_api_key {
        return Ok(());
    }
    if let Some(aggregate) = settings.active_aggregate_relay_profile() {
        if aggregate.members.is_empty() {
            anyhow::bail!("聚合供应商没有可用成员，未执行真实请求验证");
        }
        for member in aggregate.members {
            let profile = settings
                .relay_profiles
                .iter()
                .find(|profile| profile.id == member.relay_id)
                .with_context(|| format!("聚合成员 {} 不存在", member.relay_id))?;
            verify_profile_live(profile, &settings.relay_test_model).await?;
        }
        return Ok(());
    }
    verify_profile_live(&active, &settings.relay_test_model).await
}

pub async fn verify_backend_settings_from_home(
    home: &Path,
    settings: &BackendSettings,
) -> anyhow::Result<()> {
    let active = settings.active_relay_profile();
    if active.relay_mode == RelayMode::Official && !active.official_mix_api_key {
        return Ok(());
    }
    if settings.active_aggregate_relay_profile().is_some() {
        return verify_backend_settings_live(settings).await;
    }
    let persisted = crate::relay_config::relay_profile_from_live_for_probe(home, &active)
        .context("无法从写入后的 config.toml / auth.json 构造验证配置")?;
    verify_profile_live(&persisted, &settings.relay_test_model)
        .await
        .context("写入后真实请求复核失败")
}

pub async fn switch_relay_profile_in_home_verified(
    store: &SettingsStore,
    home: &Path,
    next_settings: BackendSettings,
    previous_active_relay_id: &str,
) -> anyhow::Result<RelaySwitchResult> {
    verify_backend_settings_live(&next_settings)
        .await
        .context("写入前真实请求验证失败；未修改 Codex 配置")?;
    let original_settings = store.load().context("读取切换前 Manager 设置失败")?;
    crate::codex_app_state::capture_app_state_snapshot(home)
        .context("创建 Codex 界面状态恢复快照失败；未修改供应商配置")?;
    let mut snapshot_profiles = original_settings.relay_profiles.clone();
    snapshot_profiles.extend(next_settings.relay_profiles.iter().cloned());
    let live_snapshot = capture_relay_live_snapshot(home, &snapshot_profiles)
        .context("创建写后验证恢复快照失败")?;

    let result =
        switch_relay_profile_in_home(store, home, next_settings, previous_active_relay_id)?;
    if let Err(error) = verify_backend_settings_from_home(home, &result.settings).await {
        let mut recovery_failures = Vec::new();
        if let Err(restore_error) = restore_relay_live_snapshot(&live_snapshot) {
            recovery_failures.push(format!("恢复 Codex live 文件失败：{restore_error:#}"));
        }
        if let Err(restore_error) = store.save(&original_settings) {
            recovery_failures.push(format!("恢复 Manager 设置失败：{restore_error:#}"));
        }
        if recovery_failures.is_empty() {
            anyhow::bail!("{error:#}；已恢复写入前状态");
        }
        anyhow::bail!(
            "{error:#}；自动恢复未完整成功：{}",
            recovery_failures.join("；")
        );
    }
    crate::codex_app_state::sync_app_state_after_provider_switch_nonfatal(
        home,
        "relay_switch.verified_after",
    );
    Ok(result)
}

async fn verify_profile_live(profile: &RelayProfile, fallback_model: &str) -> anyhow::Result<()> {
    validate_profile_probe_input(profile)?;
    let model = if !profile.test_model.trim().is_empty() {
        profile.test_model.trim().to_string()
    } else {
        let from_config = crate::relay_config::relay_profile_model(profile);
        if !from_config.trim().is_empty() {
            from_config
        } else {
            fallback_model.trim().to_string()
        }
    };
    if model.trim().is_empty() {
        anyhow::bail!("供应商没有可用于真实请求验证的测试模型");
    }
    crate::relay_config::test_relay_profile(profile, &model)
        .await
        .with_context(|| {
            format!(
                "供应商「{}」真实请求验证失败",
                if profile.name.trim().is_empty() {
                    profile.id.as_str()
                } else {
                    profile.name.as_str()
                }
            )
        })?;
    Ok(())
}

fn validate_profile_probe_input(profile: &RelayProfile) -> anyhow::Result<()> {
    if profile.relay_mode != RelayMode::Aggregate && !profile.auth_contents.trim().is_empty() {
        serde_json::from_str::<serde_json::Value>(&profile.auth_contents)
            .with_context(|| format!("供应商「{}」的 auth.json 不是有效 JSON", profile.name))?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelaySwitchResult {
    pub settings: BackendSettings,
    pub configured: bool,
    pub backup_path: Option<String>,
}

pub fn switch_relay_profile_in_home(
    store: &SettingsStore,
    home: &Path,
    next_settings: BackendSettings,
    previous_active_relay_id: &str,
) -> anyhow::Result<RelaySwitchResult> {
    let mut selected_settings = next_settings;
    if !selected_settings.relay_profiles_enabled {
        anyhow::bail!("供应商配置总开关已关闭，未写入 config.toml / auth.json。");
    }
    let original_settings = store.load().map_err(|error| {
        anyhow::anyhow!(
            "读取当前供应商设置失败：{error:#}；已停止切换，未改写 settings.json、config.toml 或 auth.json"
        )
    })?;
    if !previous_active_relay_id.trim().is_empty()
        && previous_active_relay_id != selected_settings.active_relay_id
    {
        backfill_profile_before_switch(home, &mut selected_settings, previous_active_relay_id)?;
    }

    selected_settings.launch_mode =
        launch_mode_for_relay_profile(&selected_settings.active_relay_profile());
    let planned_settings_bytes = serde_json::to_vec_pretty(&selected_settings)?.len() as u64;
    crate::mirror_access::ensure_storage_headroom(
        store.path(),
        planned_settings_bytes.saturating_mul(2),
        crate::mirror_access::MIN_SAFE_FREE_SPACE_BYTES,
    )?;
    crate::mirror_access::ensure_storage_headroom(
        home,
        planned_settings_bytes,
        crate::mirror_access::MIN_SAFE_FREE_SPACE_BYTES,
    )?;
    let live_snapshot = capture_relay_live_snapshot(home, &selected_settings.relay_profiles)
        .context("创建供应商切换恢复快照失败")?;
    store
        .save(&selected_settings)
        .context("保存供应商设置失败")?;
    let selected_settings = store.load().context("读取供应商设置失败")?;

    match apply_selected_relay_profile(home, &selected_settings) {
        Ok(result) => Ok(result),
        Err(error) => {
            let mut recovery_failures = Vec::new();
            if let Err(restore_error) = restore_relay_live_snapshot(&live_snapshot) {
                recovery_failures.push(format!("恢复 Codex live 文件失败：{restore_error:#}"));
            }
            if let Err(restore_error) = store.save(&original_settings) {
                recovery_failures.push(format!("恢复 Manager 设置失败：{restore_error:#}"));
            }
            if recovery_failures.is_empty() {
                Err(anyhow::anyhow!(
                    "供应商切换失败：{error:#}；已恢复切换前状态"
                ))
            } else {
                Err(anyhow::anyhow!(
                    "{error:#}；自动恢复未完整成功：{}",
                    recovery_failures.join("；")
                ))
            }
        }
    }
}

fn backfill_profile_before_switch(
    home: &Path,
    settings: &mut BackendSettings,
    previous_active_relay_id: &str,
) -> anyhow::Result<()> {
    let profile = settings
        .relay_profiles
        .iter_mut()
        .find(|profile| profile.id == previous_active_relay_id)
        .with_context(|| "当前供应商已不在配置列表中，已停止切换以避免覆盖用户改动。")?;
    backfill_relay_profile_from_home_with_common(
        home,
        profile,
        &mut settings.relay_context_config_contents,
    )
    .with_context(|| "回填当前供应商配置失败")
}

fn apply_selected_relay_profile(
    home: &Path,
    settings: &BackendSettings,
) -> anyhow::Result<RelaySwitchResult> {
    let relay = settings.active_relay_profile();
    let common_config = relay_combined_common_config(settings);
    let result = if relay.relay_mode == RelayMode::Official && !relay.official_mix_api_key {
        let auth_contents =
            (!relay.auth_contents.trim().is_empty()).then_some(relay.auth_contents.as_str());
        crate::relay_config::clear_relay_config_to_home_with_auth_and_computer_use_guard(
            home,
            auth_contents,
            settings.computer_use_guard_enabled,
        )?
    } else {
        validate_switch_profile_files(&relay)?;
        crate::relay_config::apply_relay_profile_to_home_with_switch_rules_and_computer_use_guard(
            home,
            &relay,
            &common_config,
            settings.computer_use_guard_enabled,
        )?
    };
    let status = try_relay_config_status_from_home(home)?;
    if relay.relay_mode == RelayMode::PureApi && !status.configured {
        anyhow::bail!(
            "纯 API 配置写入后未检测到完整 custom provider，请检查 config.toml 和供应商 API Key。"
        );
    }
    Ok(RelaySwitchResult {
        settings: settings.clone(),
        configured: status.configured,
        backup_path: result.backup_path,
    })
}

fn validate_switch_profile_files(profile: &crate::settings::RelayProfile) -> anyhow::Result<()> {
    if profile.relay_mode != RelayMode::Aggregate && profile.config_contents.trim().is_empty() {
        anyhow::bail!(
            "供应商「{}」缺少独立 config.toml，已停止切换，避免继续显示上一套配置文件。",
            if profile.name.trim().is_empty() {
                profile.id.as_str()
            } else {
                profile.name.as_str()
            }
        );
    }
    if profile.relay_mode == RelayMode::Official
        && serde_json::from_str::<serde_json::Value>(&profile.auth_contents)
            .ok()
            .and_then(|value| {
                value
                    .get("OPENAI_API_KEY")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .map(str::is_empty)
            })
            == Some(false)
    {
        anyhow::bail!(
            "官方混合 API 不应在 auth.json 中保存 OPENAI_API_KEY。请清理此供应商的 auth.json 后再切换。"
        );
    }
    validate_profile_probe_input(profile)?;
    Ok(())
}

fn launch_mode_for_relay_profile(profile: &crate::settings::RelayProfile) -> LaunchMode {
    if profile.relay_mode == RelayMode::PureApi {
        LaunchMode::Patch
    } else {
        LaunchMode::Relay
    }
}

fn relay_combined_common_config(settings: &BackendSettings) -> String {
    let sections = [
        settings.relay_common_config_contents.trim(),
        settings.relay_context_config_contents.trim(),
    ]
    .into_iter()
    .filter(|section| !section.is_empty())
    .collect::<Vec<_>>();
    if sections.is_empty() {
        String::new()
    } else {
        crate::relay_config::normalize_config_text(&format!("{}\n", sections.join("\n\n")))
    }
}
