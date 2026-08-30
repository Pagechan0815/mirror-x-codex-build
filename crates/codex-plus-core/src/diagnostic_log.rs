use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{Value, json};

static TEST_LOG_PATH: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();

#[derive(Debug, Clone, Serialize)]
struct DiagnosticRecord {
    timestamp_ms: u64,
    pid: u32,
    event: String,
    detail: Value,
}

pub fn append_diagnostic_log(event: &str, detail: impl Serialize) -> std::io::Result<()> {
    let path = diagnostic_log_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let detail = serde_json::to_value(detail).unwrap_or_else(|error| {
        json!({
            "serialization_error": error.to_string()
        })
    });
    let detail = redact_sensitive_values(detail);
    let record = DiagnosticRecord {
        timestamp_ms: now_ms(),
        pid: std::process::id(),
        event: event.to_string(),
        detail,
    };
    let line = serde_json::to_string(&record).unwrap_or_else(|error| {
        json!({
            "timestamp_ms": now_ms(),
            "pid": std::process::id(),
            "event": "diagnostic_log.serialization_failed",
            "detail": {
                "message": error.to_string()
            }
        })
        .to_string()
    });

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{line}")?;
    Ok(())
}

pub fn diagnostic_log_path() -> PathBuf {
    if let Some(lock) = TEST_LOG_PATH.get() {
        if let Ok(guard) = lock.lock() {
            if let Some(path) = &*guard {
                return path.clone();
            }
        }
    }
    crate::paths::default_diagnostic_log_path()
}

#[doc(hidden)]
pub fn set_diagnostic_log_path_for_tests(path: Option<PathBuf>) {
    let lock = TEST_LOG_PATH.get_or_init(|| Mutex::new(None));
    *lock.lock().expect("test log path lock poisoned") = path;
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn redact_sensitive_values(value: Value) -> Value {
    match value {
        Value::Object(mut object) => {
            for (key, item) in object.iter_mut() {
                let normalized = key.to_ascii_lowercase();
                if normalized.contains("api_key")
                    || normalized.contains("apikey")
                    || normalized.contains("auth_contents")
                    || normalized.contains("config_contents")
                    || normalized.contains("bearer_token")
                    || normalized == "token"
                    || normalized == "password"
                    || normalized == "secret"
                {
                    *item = Value::String("[REDACTED]".to_string());
                } else {
                    *item = redact_sensitive_values(item.take());
                }
            }
            Value::Object(object)
        }
        Value::Array(items) => {
            Value::Array(items.into_iter().map(redact_sensitive_values).collect())
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_sensitive_values_masks_nested_credentials() {
        let value = redact_sensitive_values(json!({
            "api_key": "sk-secret",
            "nested": {
                "auth_contents": "{\"OPENAI_API_KEY\":\"sk-secret\"}",
                "safe": "visible"
            },
            "items": [{"bearer_token": "secret"}]
        }));
        assert_eq!(value["api_key"], "[REDACTED]");
        assert_eq!(value["nested"]["auth_contents"], "[REDACTED]");
        assert_eq!(value["nested"]["safe"], "visible");
        assert_eq!(value["items"][0]["bearer_token"], "[REDACTED]");
    }
}
