//! Manager-side lifecycle for the mobile control host.
//!
//! The host lives here rather than in the launcher on purpose: enabling phone
//! control must not touch the Codex injection path, so a failure here can never
//! break a user's desktop Codex.

use std::sync::{Arc, Mutex, OnceLock};

use codex_plus_core::mobile_relay_host::{
    MobileRelayHostConfig, MobileRelayHostPhase, MobileRelayHostRuntime, MobileRelayHostStatus,
};
use codex_plus_core::settings::{BackendSettings, SettingsStore, default_mobile_control_relay_url};
use serde_json::{Value, json};

struct HostState {
    runtime: Option<MobileRelayHostRuntime>,
    generation: u64,
    room_id: String,
    relay_url: String,
    mobile_url: Option<String>,
    host_status: MobileRelayHostStatus,
}

fn stopped_status(message: &str) -> MobileRelayHostStatus {
    MobileRelayHostStatus {
        phase: MobileRelayHostPhase::Stopped,
        message: message.to_string(),
        session_id: None,
        relay_connected: false,
        codex_connected: false,
    }
}

fn state() -> &'static Mutex<HostState> {
    static STATE: OnceLock<Mutex<HostState>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(HostState {
            runtime: None,
            generation: 0,
            room_id: String::new(),
            relay_url: String::new(),
            mobile_url: None,
            host_status: stopped_status("desktop bridge stopped"),
        })
    })
}

pub fn load_settings() -> anyhow::Result<BackendSettings> {
    SettingsStore::default().load().map_err(|error| {
        anyhow::anyhow!("读取手机控制设置失败：{error:#}；未使用默认设置判断启用状态或恢复桌面桥接")
    })
}

/// The key the phone must type in. Mobile control reuses the relay key that is
/// already configured, so the user never manages a second credential.
pub fn active_api_key(settings: &BackendSettings) -> String {
    let profile_key = settings.active_relay_profile().api_key.trim().to_string();
    if !profile_key.is_empty() {
        return profile_key;
    }
    settings.relay_api_key.trim().to_string()
}

pub fn effective_relay_url(settings: &BackendSettings) -> String {
    let configured = settings.mobile_control_relay_url.trim();
    if configured.is_empty() {
        default_mobile_control_relay_url()
    } else {
        configured.to_string()
    }
}

fn is_running() -> bool {
    state()
        .lock()
        .map(|state| state.runtime.is_some())
        .unwrap_or(false)
}

fn current_room_id() -> String {
    state()
        .lock()
        .map(|state| state.room_id.clone())
        .unwrap_or_default()
}

fn current_mobile_url() -> Option<String> {
    state()
        .lock()
        .ok()
        .and_then(|state| state.mobile_url.clone())
}

fn current_host_status() -> MobileRelayHostStatus {
    state()
        .lock()
        .map(|state| state.host_status.clone())
        .unwrap_or_else(|_| stopped_status("desktop bridge status unavailable"))
}

fn update_host_status(generation: u64, status: MobileRelayHostStatus) {
    if let Ok(mut state) = state().lock() {
        if state.generation == generation {
            state.host_status = status;
        }
    }
}

/// Shows only the head and tail of the room id. It is derived from the API key,
/// so the full value should not be pasted into screenshots or support chats.
pub fn mask_room_id(room_id: &str) -> String {
    if room_id.len() <= 12 {
        return room_id.to_string();
    }
    format!("{}...{}", &room_id[..6], &room_id[room_id.len() - 4..])
}

pub fn qr_svg(payload: &str) -> Result<String, String> {
    use qrcode::render::svg;
    use qrcode::{EcLevel, QrCode};

    let code = QrCode::with_error_correction_level(payload.as_bytes(), EcLevel::M)
        .map_err(|error| format!("failed to generate qr svg: {error}"))?;
    Ok(code
        .render::<svg::Color<'_>>()
        .min_dimensions(240, 240)
        .dark_color(svg::Color("#0f172a"))
        .light_color(svg::Color("#ffffff"))
        .quiet_zone(true)
        .build())
}

/// Starts the host if the user enabled it and a key is present. Called on
/// manager startup and after the user flips the switch.
pub async fn start(settings: &BackendSettings) -> Result<Value, String> {
    let api_key = active_api_key(settings);
    if api_key.is_empty() {
        return Err("Please fill in a Mirror X key before enabling mobile control.".to_string());
    }
    let relay_url = effective_relay_url(settings);
    let config = MobileRelayHostConfig::from_api_key(&api_key, &relay_url)
        .map_err(|error| format!("mobile control config is invalid: {error}"))?;

    let room_id = config.room_id.clone();
    let mobile_url = config.mobile_url();
    let (previous_runtime, generation) = {
        let mut state = state()
            .lock()
            .map_err(|_| "mobile control state is unavailable".to_string())?;
        state.generation = state.generation.wrapping_add(1);
        let generation = state.generation;
        let previous_runtime = state.runtime.take();
        state.room_id.clear();
        state.mobile_url = None;
        state.host_status = MobileRelayHostStatus {
            phase: MobileRelayHostPhase::Starting,
            message: "stopping previous desktop bridge".to_string(),
            session_id: None,
            relay_connected: false,
            codex_connected: false,
        };
        (previous_runtime, generation)
    };
    if let Some(runtime) = previous_runtime {
        runtime.stop().await;
    }
    if state()
        .lock()
        .map(|state| state.generation != generation)
        .unwrap_or(true)
    {
        return Err("mobile control start was superseded by a newer request".to_string());
    }
    let reporter = Arc::new(move |status| update_host_status(generation, status));
    let mut runtime = Some(codex_plus_core::mobile_relay_host::spawn_with_reporter(
        config,
        Some(reporter),
    ));
    let accepted = state()
        .lock()
        .map(|mut state| {
            if state.generation != generation {
                return false;
            }
            state.runtime = runtime.take();
            state.room_id = room_id.clone();
            state.relay_url = relay_url.clone();
            state.mobile_url = Some(mobile_url.clone());
            state.host_status = MobileRelayHostStatus {
                phase: MobileRelayHostPhase::Starting,
                message: "desktop bridge starting".to_string(),
                session_id: None,
                relay_connected: false,
                codex_connected: false,
            };
            true
        })
        .unwrap_or(false);
    if !accepted {
        if let Some(runtime) = runtime {
            runtime.stop().await;
        }
        return Err("mobile control start was superseded by a newer request".to_string());
    }

    let _ = codex_plus_core::diagnostic_log::append_diagnostic_log(
        "mobile_control.started",
        json!({ "room": mask_room_id(&room_id), "relayUrl": relay_url }),
    );

    Ok(json!({
        "running": true,
        "roomId": room_id,
        "roomIdMasked": mask_room_id(&room_id),
        "relayUrl": relay_url,
        "mobileUrl": mobile_url,
    }))
}

pub fn stop() {
    let taken = state().lock().ok().and_then(|mut state| {
        state.generation = state.generation.wrapping_add(1);
        state.room_id.clear();
        state.mobile_url = None;
        state.host_status = stopped_status("desktop bridge stopped");
        state.runtime.take()
    });
    if let Some(runtime) = taken {
        std::thread::spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
            else {
                return;
            };
            rt.block_on(runtime.stop());
        });
        let _ = codex_plus_core::diagnostic_log::append_diagnostic_log(
            "mobile_control.stopped",
            json!({}),
        );
    }
}

pub async fn stop_async() {
    let taken = state().lock().ok().and_then(|mut state| {
        state.generation = state.generation.wrapping_add(1);
        state.room_id.clear();
        state.mobile_url = None;
        state.host_status = stopped_status("desktop bridge stopped");
        state.runtime.take()
    });
    if let Some(runtime) = taken {
        runtime.stop().await;
        let _ = codex_plus_core::diagnostic_log::append_diagnostic_log(
            "mobile_control.stopped",
            json!({}),
        );
    }
}

pub fn status() -> Value {
    let (settings, settings_error) = match load_settings() {
        Ok(settings) => (settings, None),
        Err(error) => (BackendSettings::default(), Some(error.to_string())),
    };
    let api_key = active_api_key(&settings);
    let relay_url = effective_relay_url(&settings);
    let running = is_running();
    let room_id = current_room_id();
    let runtime_status = current_host_status();

    let pairing = if api_key.is_empty() {
        None
    } else {
        MobileRelayHostConfig::from_api_key(&api_key, &relay_url)
            .ok()
            .map(|config| (config.room_id.clone(), config.mobile_url()))
    };

    let phase = if running {
        runtime_status.phase.clone()
    } else {
        MobileRelayHostPhase::Stopped
    };
    let message = if let Some(error) = &settings_error {
        error.clone()
    } else if running {
        runtime_status.message.clone()
    } else if settings.mobile_control_enabled && !api_key.is_empty() {
        "desktop bridge is not running".to_string()
    } else {
        "desktop bridge stopped".to_string()
    };

    json!({
        "enabled": settings.mobile_control_enabled,
        "running": running,
        "hasKey": !api_key.is_empty(),
        "relayUrl": relay_url,
        "roomId": room_id,
        "roomIdMasked": mask_room_id(&pairing.as_ref().map(|pair| pair.0.clone()).unwrap_or_default()),
        "mobileUrl": current_mobile_url().or_else(|| pairing.as_ref().map(|pair| pair.1.clone())),
        "phase": phase,
        "message": message,
        "sessionId": runtime_status.session_id,
        "relayConnected": runtime_status.relay_connected,
        "codexConnected": runtime_status.codex_connected,
        "settingsError": settings_error,
    })
}

/// Restores the host on manager startup so users do not have to re-arm it after
/// a reboot.
pub fn resume_on_startup() {
    let settings = match load_settings() {
        Ok(settings) => settings,
        Err(error) => {
            let _ = codex_plus_core::diagnostic_log::append_diagnostic_log(
                "mobile_control.resume_skipped_settings_error",
                json!({ "error": error.to_string() }),
            );
            return;
        }
    };
    if !settings.mobile_control_enabled {
        return;
    }
    tauri::async_runtime::spawn(async move {
        if let Err(error) = start(&settings).await {
            let _ = codex_plus_core::diagnostic_log::append_diagnostic_log(
                "mobile_control.resume_failed",
                json!({ "error": error }),
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_long_room_ids_only() {
        assert_eq!(mask_room_id("abcd"), "abcd");
        let masked = mask_room_id("0123456789abcdef0123456789abcdef");
        assert!(masked.starts_with("012345"));
        assert!(masked.ends_with("cdef"));
        assert!(masked.len() < 32);
    }

    #[test]
    fn qr_svg_contains_svg_root() {
        let svg = qr_svg("https://relay.example.club/relay/mobile#pair").unwrap();
        assert!(svg.contains("<svg"));
    }

    #[test]
    fn relay_url_falls_back_to_tls_default() {
        let mut settings = BackendSettings::default();
        settings.mobile_control_relay_url = "   ".to_string();
        assert!(effective_relay_url(&settings).starts_with("wss://"));
    }

    #[test]
    fn status_reports_corrupt_settings_instead_of_claiming_defaults_are_current() {
        let _lock = crate::settings_path_test_lock().lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let settings_path = temp.path().join("settings.json");
        let corrupt = b"{not valid settings";
        std::fs::write(&settings_path, corrupt).unwrap();
        let previous =
            codex_plus_core::paths::set_settings_path_for_tests(Some(settings_path.clone()));

        let payload = status();
        codex_plus_core::paths::set_settings_path_for_tests(previous);

        assert!(payload["settingsError"].as_str().is_some());
        assert!(
            payload["message"]
                .as_str()
                .is_some_and(|message| message.contains("未使用默认设置"))
        );
        assert_eq!(std::fs::read(settings_path).unwrap(), corrupt);
    }

    #[test]
    fn start_refuses_without_key() {
        let mut settings = BackendSettings::default();
        settings.relay_api_key = String::new();
        for profile in settings.relay_profiles.iter_mut() {
            profile.api_key = String::new();
        }
        assert!(tauri::async_runtime::block_on(start(&settings)).is_err());
    }

    #[test]
    fn start_from_plain_thread_enters_tauri_runtime() {
        let mut settings = BackendSettings::default();
        settings.relay_api_key = "sk-mobile-control-runtime-test".to_string();
        settings.mobile_control_relay_url = "ws://127.0.0.1:9".to_string();

        let result = std::thread::spawn(move || tauri::async_runtime::block_on(start(&settings)))
            .join()
            .expect("mobile control start must not panic outside a Tokio runtime");

        assert!(result.is_ok());
        tauri::async_runtime::block_on(stop_async());
    }
}
