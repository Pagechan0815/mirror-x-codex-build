use anyhow::Context;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct BackupStore {
    root: PathBuf,
}

#[derive(Debug)]
pub(crate) struct BackupDraft {
    store: BackupStore,
    token: String,
    sidecar_dir: PathBuf,
    committed: bool,
}

impl BackupStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn write_backup(
        &self,
        session_id: &str,
        source_db: &Path,
        tables: serde_json::Value,
    ) -> anyhow::Result<String> {
        self.ensure_root()?;
        let token = new_token();
        self.write_backup_with_token(&token, session_id, source_db, tables)?;
        Ok(token)
    }

    pub(crate) fn begin_draft(&self) -> anyhow::Result<BackupDraft> {
        self.ensure_root()?;
        let token = new_token();
        let sidecar_dir = self.root.join(format!("{token}.files"));
        fs::create_dir(&sidecar_dir).with_context(|| {
            format!(
                "failed to create backup sidecar directory {}",
                sidecar_dir.display()
            )
        })?;
        Ok(BackupDraft {
            store: self.clone(),
            token,
            sidecar_dir,
            committed: false,
        })
    }

    pub(crate) fn sidecar_path(&self, token: &str, file_name: &str) -> anyhow::Result<PathBuf> {
        self.validated_path_for(token)?;
        if file_name.is_empty()
            || matches!(file_name, "." | "..")
            || !file_name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        {
            anyhow::bail!("Invalid backup sidecar name");
        }
        Ok(self.root.join(format!("{token}.files")).join(file_name))
    }

    fn ensure_root(&self) -> anyhow::Result<()> {
        fs::create_dir_all(&self.root).with_context(|| {
            format!(
                "failed to create backup directory {}",
                self.root.to_string_lossy()
            )
        })
    }

    fn write_backup_with_token(
        &self,
        token: &str,
        session_id: &str,
        source_db: &Path,
        tables: serde_json::Value,
    ) -> anyhow::Result<()> {
        let payload = json!({
            "token": token,
            "session_id": session_id,
            "source_db": source_db.to_string_lossy(),
            "tables": tables,
        });
        let bytes = serde_json::to_vec_pretty(&payload)?;
        codex_plus_core::settings::atomic_write(&self.path_for(&token), &bytes)
            .context("failed to durably write session backup")?;
        self.read_backup(token)
            .context("failed to verify committed session backup")?;
        Ok(())
    }

    pub fn read_backup(&self, token: &str) -> anyhow::Result<serde_json::Value> {
        let path = self.validated_path_for(token)?;
        let text = fs::read_to_string(&path)
            .with_context(|| format!("Backup token not found: {token}"))?;
        let payload = serde_json::from_str::<serde_json::Value>(&text)?;
        if payload.get("token").and_then(serde_json::Value::as_str) != Some(token) {
            anyhow::bail!("Backup token does not match its payload");
        }
        if !payload
            .get("tables")
            .is_some_and(serde_json::Value::is_object)
        {
            anyhow::bail!("Backup payload is missing its table snapshot");
        }
        Ok(payload)
    }

    pub fn path_for(&self, token: &str) -> PathBuf {
        let safe: String = token
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_')
            .collect();
        self.root.join(format!("{safe}.json"))
    }

    fn validated_path_for(&self, token: &str) -> anyhow::Result<PathBuf> {
        if token.is_empty()
            || !token
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
        {
            anyhow::bail!("Invalid backup token");
        }
        Ok(self.path_for(token))
    }
}

impl BackupDraft {
    pub(crate) fn sidecar_dir(&self) -> &Path {
        &self.sidecar_dir
    }

    pub(crate) fn sidecar_path(&self, file_name: &str) -> anyhow::Result<PathBuf> {
        self.store.sidecar_path(&self.token, file_name)
    }

    pub(crate) fn commit(
        mut self,
        session_id: &str,
        source_db: &Path,
        tables: serde_json::Value,
    ) -> anyhow::Result<String> {
        self.store
            .write_backup_with_token(&self.token, session_id, source_db, tables)?;
        self.committed = true;
        Ok(self.token.clone())
    }
}

impl Drop for BackupDraft {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let _ = fs::remove_file(self.store.path_for(&self.token));
        let _ = fs::remove_dir_all(&self.sidecar_dir);
    }
}

fn new_token() -> String {
    let epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{epoch}-{}", Uuid::new_v4().simple())
}
