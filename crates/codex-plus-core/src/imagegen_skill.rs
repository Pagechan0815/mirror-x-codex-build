use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const SKILL_NAME: &str = "jingzi-imagegen";
const MANAGED_MARKER: &str = ".mirror-x-managed.json";
const BASELINE_MANIFEST: &str = "imagegen-baseline.json";
const BASELINE_ROOT: &str = "imagegen-baseline";
const LEGACY_BASELINE_SCHEMA_VERSION: u32 = 1;
const BASELINE_SCHEMA_VERSION: u32 = 2;
const BASELINE_SKILL_DIR: &str = "skill";
const BASELINE_CONFIG_FILE: &str = "config.json";
const DEFAULT_BASE_URL: &str = "https://api.jingziai.club/v1";
const SOURCE_REPOSITORY: &str = "Pagechan0815/jingzi-imagegen-skill";
const SOURCE_COMMIT: &str = "104172aa6a74bbfd5c3cc6ab3f6d3f0c64dce052";

const SKILL_FILES: &[(&str, &[u8])] = &[
    (
        "LICENSE.txt",
        include_bytes!("../assets/jingzi-imagegen/LICENSE.txt"),
    ),
    (
        "SKILL.md",
        include_bytes!("../assets/jingzi-imagegen/SKILL.md"),
    ),
    (
        "agents/openai.yaml",
        include_bytes!("../assets/jingzi-imagegen/agents/openai.yaml"),
    ),
    (
        "assets/imagegen-small.svg",
        include_bytes!("../assets/jingzi-imagegen/assets/imagegen-small.svg"),
    ),
    (
        "assets/imagegen.png",
        include_bytes!("../assets/jingzi-imagegen/assets/imagegen.png"),
    ),
    (
        "references/key-registration.md",
        include_bytes!("../assets/jingzi-imagegen/references/key-registration.md"),
    ),
    (
        "references/prompting.md",
        include_bytes!("../assets/jingzi-imagegen/references/prompting.md"),
    ),
];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImagegenSkillStatus {
    pub enabled: bool,
    pub configured: bool,
    pub managed: bool,
    pub helper_available: bool,
    pub skill_available: bool,
    pub skill_path: String,
    pub config_path: String,
    pub source_commit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ImagegenBaseline {
    schema_version: u32,
    captured_at_ms: u64,
    #[serde(default)]
    codex_home: Option<String>,
    skill_existed: bool,
    config_existed: bool,
    #[serde(default)]
    skill_sha256: Option<String>,
    #[serde(default)]
    config_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ManagedMarker<'a> {
    product: &'a str,
    source_repository: &'a str,
    source_commit: &'a str,
}

#[derive(Debug, Deserialize)]
struct ImagegenConfig {
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    helper_path: Option<PathBuf>,
}

pub fn status(codex_home: &Path, state_dir: &Path) -> ImagegenSkillStatus {
    status_at(codex_home, state_dir, &config_path())
}

fn status_at(codex_home: &Path, state_dir: &Path, config_path: &Path) -> ImagegenSkillStatus {
    let skill_path = skill_path(codex_home);
    let managed = skill_path.join(MANAGED_MARKER).is_file();
    let configured = read_configured_key(config_path)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    let baseline_exists = state_dir.join(BASELINE_MANIFEST).is_file();
    let configured_helper = read_helper_path(config_path);
    let helper_available = configured_helper
        .as_ref()
        .map(|path| path.is_file())
        .unwrap_or(false);
    let skill_available = configured_helper
        .as_deref()
        .map(|helper_path| verify_managed_skill(&skill_path, helper_path).is_ok())
        .unwrap_or(false);
    ImagegenSkillStatus {
        enabled: managed && configured && helper_available && skill_available,
        configured,
        managed: managed || baseline_exists,
        helper_available,
        skill_available,
        skill_path: skill_path.to_string_lossy().to_string(),
        config_path: config_path.to_string_lossy().to_string(),
        source_commit: SOURCE_COMMIT.to_string(),
    }
}

pub async fn validate_saved_or_provided_key(api_key: Option<&str>) -> anyhow::Result<String> {
    validate_saved_or_provided_key_at(&config_path(), DEFAULT_BASE_URL, api_key).await
}

async fn validate_saved_or_provided_key_at(
    config_path: &Path,
    base_url: &str,
    api_key: Option<&str>,
) -> anyhow::Result<String> {
    let key = match api_key.map(str::trim).filter(|value| !value.is_empty()) {
        Some(key) => key.to_string(),
        None => {
            let config = read_imagegen_config(config_path).with_context(|| {
                format!(
                    "无法读取已保存的镜子AI Image Key：{}",
                    config_path.display()
                )
            })?;
            let key = config.api_key.trim();
            if key.is_empty() {
                bail!("已保存的镜子AI Image Key 为空，请重新填写并检查权限。");
            }
            key.to_string()
        }
    };
    let discovery = crate::mirror_access::discover_models_at(&key, base_url)
        .await
        .context("Image Key 模型权限检查失败")?;
    if !discovery
        .models
        .iter()
        .any(|model| model.id == "gpt-image-2")
    {
        bail!("该 Key 的模型列表中未发现 gpt-image-2，请在镜子AI获取生图分组 Key。");
    }
    Ok(key)
}

pub fn enable(codex_home: &Path, state_dir: &Path, api_key: Option<&str>) -> anyhow::Result<()> {
    let helper_path = bundled_helper_path();
    enable_at(codex_home, state_dir, &config_path(), &helper_path, api_key)
}

fn enable_at(
    codex_home: &Path,
    state_dir: &Path,
    config_path: &Path,
    helper_path: &Path,
    api_key: Option<&str>,
) -> anyhow::Result<()> {
    let key = api_key.map(str::trim).filter(|value| !value.is_empty());
    if key.is_none() && !config_has_key(config_path) {
        bail!("请填写并验证镜子AI Image Key。");
    }
    if !helper_path.is_file() {
        bail!(
            "安装包缺少生图执行器：{}。请重新安装完整版本。",
            helper_path.display()
        );
    }
    fs::create_dir_all(codex_home)?;
    fs::create_dir_all(state_dir)?;
    ensure_baseline(codex_home, state_dir, config_path)?;

    let stored_key = key
        .map(str::to_string)
        .or_else(|| read_configured_key(config_path))
        .unwrap_or_default();
    let previous_config = read_optional_file(config_path)?;
    if let Err(error) = write_config(config_path, &stored_key, helper_path)
        .and_then(|_| verify_config(config_path, &stored_key, helper_path))
    {
        return match restore_optional_file(config_path, previous_config.as_deref()) {
            Ok(()) => Err(error).context("生图配置写入失败，已恢复本次操作前配置"),
            Err(rollback_error) => Err(error).context(format!(
                "生图配置写入失败，且旧配置恢复失败：{rollback_error}"
            )),
        };
    }
    if let Err(error) = install_managed_skill(codex_home, helper_path) {
        return match restore_optional_file(config_path, previous_config.as_deref()) {
            Ok(()) => Err(error).context("生图 Skill 安装失败，已恢复本次操作前配置"),
            Err(rollback_error) => Err(error).context(format!(
                "生图 Skill 安装失败，且旧配置恢复失败：{rollback_error}"
            )),
        };
    }
    Ok(())
}

pub fn disable(codex_home: &Path, state_dir: &Path) -> anyhow::Result<()> {
    restore_baseline(codex_home, state_dir)
}

pub fn restore_baseline(codex_home: &Path, state_dir: &Path) -> anyhow::Result<()> {
    restore_baseline_at(codex_home, state_dir, &config_path())
}

pub(crate) fn ensure_restored_for_state_removal(
    codex_home: &Path,
    state_dir: &Path,
) -> anyhow::Result<()> {
    ensure_restored_for_state_removal_at(codex_home, state_dir, &config_path())
}

pub(crate) fn ensure_restored_for_state_removal_at(
    codex_home: &Path,
    state_dir: &Path,
    config_path: &Path,
) -> anyhow::Result<()> {
    let current_skill = skill_path(codex_home);
    let managed_marker = current_skill.join(MANAGED_MARKER);
    match fs::symlink_metadata(&managed_marker) {
        Ok(_) => bail!("生图 Skill 仍由 Mirror X Codex 托管，请先执行恢复。"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("无法检查生图托管标记 {}", managed_marker.display()));
        }
    }

    let manifest_path = state_dir.join(BASELINE_MANIFEST);
    let manifest_bytes = match fs::read(&manifest_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("无法读取生图 baseline {}", manifest_path.display()));
        }
    };
    let baseline = validate_baseline_bytes(state_dir, &manifest_bytes)
        .context("生图 baseline 无法校验，已保留恢复数据")?;
    let baseline =
        migrate_or_validate_imagegen_home_binding(codex_home, state_dir, &manifest_path, baseline)?;

    if baseline.skill_existed {
        let baseline_skill = state_dir.join(BASELINE_ROOT).join(BASELINE_SKILL_DIR);
        if !tree_contents_equal(&baseline_skill, &current_skill)
            .context("无法校验已恢复的生图 Skill")?
        {
            bail!("生图 Skill 尚未恢复到接管前内容，已保留 baseline。");
        }
    } else {
        match fs::symlink_metadata(&current_skill) {
            Ok(_) => bail!("接管前不存在生图 Skill，但当前目录仍存在，请先执行恢复。"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("无法检查生图 Skill {}", current_skill.display()));
            }
        }
    }

    let expected_config = if baseline.config_existed {
        Some(
            fs::read(state_dir.join(BASELINE_ROOT).join(BASELINE_CONFIG_FILE))
                .context("生图 baseline 缺少原始配置")?,
        )
    } else {
        None
    };
    let current_config = read_optional_file(config_path).context("无法读取当前生图配置")?;
    if current_config != expected_config {
        bail!("生图配置尚未恢复到接管前内容，已保留 baseline。");
    }
    Ok(())
}

fn restore_baseline_at(
    codex_home: &Path,
    state_dir: &Path,
    config_path: &Path,
) -> anyhow::Result<()> {
    let manifest_path = state_dir.join(BASELINE_MANIFEST);
    match fs::symlink_metadata(&manifest_path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
        Ok(_) => bail!(
            "生图 baseline manifest 不是普通文件：{}；已保留当前 Skill 和配置。",
            manifest_path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let managed_marker = skill_path(codex_home).join(MANAGED_MARKER);
            if fs::symlink_metadata(&managed_marker).is_ok() {
                bail!("生图 Skill 仍由 Mirror X Codex 托管，但恢复 baseline 缺失；已停止恢复。")
            }
            return Ok(());
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("无法检查生图 baseline {}", manifest_path.display()));
        }
    }
    let baseline_bytes = fs::read(&manifest_path)?;
    let baseline = validate_baseline_bytes(state_dir, &baseline_bytes)
        .context("生图功能 baseline 无法校验，已停止恢复")?;
    let baseline =
        migrate_or_validate_imagegen_home_binding(codex_home, state_dir, &manifest_path, baseline)?;

    let current_skill = skill_path(codex_home);
    let skills_root = codex_home.join("skills");
    fs::create_dir_all(&skills_root)?;
    let operation_id = uuid::Uuid::new_v4();
    let staging = skills_root.join(format!(".{SKILL_NAME}.restore-staging-{operation_id}"));
    let previous = skills_root.join(format!(".{SKILL_NAME}.restore-previous-{operation_id}"));
    if baseline.skill_existed {
        copy_tree(
            &state_dir.join(BASELINE_ROOT).join(BASELINE_SKILL_DIR),
            &staging,
        )?;
    }

    let current_config = config_path.to_path_buf();
    let previous_config = read_optional_file(&current_config)?;
    let baseline_config = if baseline.config_existed {
        Some(
            fs::read(state_dir.join(BASELINE_ROOT).join(BASELINE_CONFIG_FILE))
                .with_context(|| "生图 baseline 缺少原始配置")?,
        )
    } else {
        None
    };

    let previous_skill_existed = current_skill.exists();
    if previous_skill_existed {
        fs::rename(&current_skill, &previous).with_context(|| {
            format!(
                "无法暂存当前生图 Skill {} -> {}",
                current_skill.display(),
                previous.display()
            )
        })?;
    }
    let skill_restore = if baseline.skill_existed {
        fs::rename(&staging, &current_skill).with_context(|| "无法提交原始生图 Skill")
    } else {
        Ok(())
    };
    if let Err(error) = skill_restore {
        if previous_skill_existed {
            let _ = fs::rename(&previous, &current_skill);
        }
        return Err(error);
    }

    let config_restore = restore_optional_file(&current_config, baseline_config.as_deref())
        .and_then(|_| {
            if baseline.config_existed {
                restrict_config_permissions(&current_config)
            } else {
                Ok(())
            }
        });
    if let Err(error) = config_restore {
        let mut rollback_errors = Vec::new();
        if baseline.skill_existed
            && current_skill.exists()
            && let Err(rollback_error) = remove_tree_checked(&current_skill, &skills_root)
        {
            rollback_errors.push(rollback_error.to_string());
        }
        if previous_skill_existed && let Err(rollback_error) = fs::rename(&previous, &current_skill)
        {
            rollback_errors.push(rollback_error.to_string());
        }
        if let Err(rollback_error) =
            restore_optional_file(&current_config, previous_config.as_deref())
        {
            rollback_errors.push(rollback_error.to_string());
        }
        return if rollback_errors.is_empty() {
            Err(error).context("生图 baseline 恢复失败，已回到操作前状态")
        } else {
            Err(error).context(format!(
                "生图 baseline 恢复失败，且操作回滚不完整：{}",
                rollback_errors.join("；")
            ))
        };
    }
    if previous.exists() {
        let _ = remove_tree_checked(&previous, &skills_root);
    }
    Ok(())
}

fn ensure_baseline(codex_home: &Path, state_dir: &Path, config_path: &Path) -> anyhow::Result<()> {
    let manifest_path = state_dir.join(BASELINE_MANIFEST);
    match fs::symlink_metadata(&manifest_path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            let bytes = fs::read(&manifest_path)?;
            let baseline = validate_baseline_bytes(state_dir, &bytes)
                .context("现有生图 baseline 无法校验；未覆盖 Skill 或 Image Key")?;
            migrate_or_validate_imagegen_home_binding(
                codex_home,
                state_dir,
                &manifest_path,
                baseline,
            )?;
            return Ok(());
        }
        Ok(_) => bail!(
            "生图 baseline manifest 不是普通文件：{}；未覆盖 Skill 或 Image Key。",
            manifest_path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("无法检查生图 baseline {}", manifest_path.display()));
        }
    }

    let baseline_root = state_dir.join(BASELINE_ROOT);
    if baseline_root.exists() {
        remove_tree_checked(&baseline_root, state_dir)?;
    }
    fs::create_dir_all(&baseline_root)?;

    let current_skill = skill_path(codex_home);
    let current_config = config_path.to_path_buf();
    let skill_existed = current_skill.is_dir();
    let config_existed = current_config.is_file();
    if skill_existed {
        copy_tree(&current_skill, &baseline_root.join(BASELINE_SKILL_DIR))?;
    }
    if config_existed {
        fs::copy(&current_config, baseline_root.join(BASELINE_CONFIG_FILE))
            .with_context(|| "无法备份现有镜子AI生图配置")?;
    }

    let skill_sha256 = skill_existed
        .then(|| tree_sha256(&baseline_root.join(BASELINE_SKILL_DIR)))
        .transpose()?;
    let config_sha256 = config_existed
        .then(|| fs::read(baseline_root.join(BASELINE_CONFIG_FILE)).map(|bytes| sha256(&bytes)))
        .transpose()?;

    let baseline = ImagegenBaseline {
        schema_version: BASELINE_SCHEMA_VERSION,
        captured_at_ms: now_ms(),
        codex_home: Some(crate::codex_home::codex_home_identity(codex_home)?),
        skill_existed,
        config_existed,
        skill_sha256,
        config_sha256,
    };
    crate::settings::atomic_write(&manifest_path, &serde_json::to_vec_pretty(&baseline)?)
}

fn validate_baseline_bytes(
    state_dir: &Path,
    manifest_bytes: &[u8],
) -> anyhow::Result<ImagegenBaseline> {
    let baseline: ImagegenBaseline =
        serde_json::from_slice(manifest_bytes).context("生图 baseline 无法解析")?;
    if !matches!(
        baseline.schema_version,
        LEGACY_BASELINE_SCHEMA_VERSION | BASELINE_SCHEMA_VERSION
    ) {
        bail!("生图 baseline 版本不受支持。");
    }
    if baseline.schema_version == BASELINE_SCHEMA_VERSION && baseline.codex_home.is_none() {
        bail!("生图 baseline 缺少 CODEX_HOME 绑定。");
    }

    let baseline_root = state_dir.join(BASELINE_ROOT);
    if baseline.skill_existed {
        let skill = baseline_root.join(BASELINE_SKILL_DIR);
        let metadata = fs::symlink_metadata(&skill)
            .with_context(|| format!("生图 baseline 缺少原始 Skill：{}", skill.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!(
                "生图 baseline 的原始 Skill 不是普通目录：{}",
                skill.display()
            );
        }
        if let Some(expected) = baseline.skill_sha256.as_deref() {
            let actual = tree_sha256(&skill)?;
            if actual != expected {
                bail!("生图 baseline 的原始 Skill 校验失败。");
            }
        }
    } else if baseline.skill_sha256.is_some() {
        bail!("生图 baseline 的 Skill 存在状态与校验值不一致。");
    }

    if baseline.config_existed {
        let config = baseline_root.join(BASELINE_CONFIG_FILE);
        let metadata = fs::symlink_metadata(&config)
            .with_context(|| format!("生图 baseline 缺少原始配置：{}", config.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!("生图 baseline 的原始配置不是普通文件：{}", config.display());
        }
        if let Some(expected) = baseline.config_sha256.as_deref() {
            let actual = sha256(&fs::read(&config)?);
            if actual != expected {
                bail!("生图 baseline 的原始配置校验失败。");
            }
        }
    } else if baseline.config_sha256.is_some() {
        bail!("生图 baseline 的配置存在状态与校验值不一致。");
    }
    Ok(baseline)
}

fn validate_imagegen_home_binding(
    codex_home: &Path,
    baseline: &ImagegenBaseline,
) -> anyhow::Result<String> {
    let current = crate::codex_home::codex_home_identity(codex_home)?;
    if let Some(expected) = baseline.codex_home.as_deref()
        && expected != current
    {
        bail!(
            "当前 CODEX_HOME 与生图 baseline 绑定目录不一致（绑定：{expected}；当前：{current}）。已停止恢复，两个目录均未修改。"
        );
    }
    Ok(current)
}

fn migrate_or_validate_imagegen_home_binding(
    codex_home: &Path,
    state_dir: &Path,
    manifest_path: &Path,
    mut baseline: ImagegenBaseline,
) -> anyhow::Result<ImagegenBaseline> {
    let current = validate_imagegen_home_binding(codex_home, &baseline)?;
    if baseline.codex_home.is_some() {
        return Ok(baseline);
    }

    let current_skill = skill_path(codex_home);
    let managed = current_skill.join(MANAGED_MARKER).is_file();
    let matches_original = if baseline.skill_existed {
        tree_contents_equal(
            &state_dir.join(BASELINE_ROOT).join(BASELINE_SKILL_DIR),
            &current_skill,
        )?
    } else {
        !current_skill.exists()
    };
    if !managed && !matches_original {
        bail!(
            "旧版生图 baseline 未记录 CODEX_HOME，且当前 Skill 目录无法与其对应。已停止恢复并保留 baseline。"
        );
    }

    baseline.schema_version = BASELINE_SCHEMA_VERSION;
    baseline.codex_home = Some(current);
    crate::settings::atomic_write(manifest_path, &serde_json::to_vec_pretty(&baseline)?)?;
    Ok(baseline)
}

fn tree_sha256(root: &Path) -> anyhow::Result<String> {
    let metadata = fs::symlink_metadata(root)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("无法校验非普通目录：{}", root.display());
    }
    let mut hasher = Sha256::new();
    hash_tree_entries(root, root, &mut hasher)?;
    Ok(format!("{:x}", hasher.finalize()))
}

fn hash_tree_entries(root: &Path, directory: &Path, hasher: &mut Sha256) -> anyhow::Result<()> {
    let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            bail!("生图 baseline 包含符号链接：{}", path.display());
        }
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        hasher.update((relative.len() as u64).to_le_bytes());
        hasher.update(relative.as_bytes());
        if metadata.is_dir() {
            hasher.update(b"dir");
            hash_tree_entries(root, &path, hasher)?;
        } else if metadata.is_file() {
            hasher.update(b"file");
            let bytes = fs::read(&path)?;
            hasher.update((bytes.len() as u64).to_le_bytes());
            hasher.update(bytes);
        } else {
            bail!("生图 baseline 包含不支持的文件类型：{}", path.display());
        }
    }
    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn install_managed_skill(codex_home: &Path, helper_path: &Path) -> anyhow::Result<()> {
    let skills_root = codex_home.join("skills");
    fs::create_dir_all(&skills_root)?;
    let destination = skill_path(codex_home);
    let staging = skills_root.join(format!(
        ".{SKILL_NAME}.mirror-x-staging-{}",
        std::process::id()
    ));
    let previous = skills_root.join(format!(
        ".{SKILL_NAME}.mirror-x-previous-{}",
        std::process::id()
    ));

    if staging.exists() {
        remove_tree_checked(&staging, &skills_root)?;
    }
    if previous.exists() {
        remove_tree_checked(&previous, &skills_root)?;
    }
    fs::create_dir_all(&staging)?;
    for (relative, contents) in SKILL_FILES {
        let path = staging.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        crate::settings::atomic_write(&path, contents)?;
    }
    let marker = ManagedMarker {
        product: "mirror x codex",
        source_repository: SOURCE_REPOSITORY,
        source_commit: SOURCE_COMMIT,
    };
    crate::settings::atomic_write(
        &staging.join(MANAGED_MARKER),
        &serde_json::to_vec_pretty(&marker)?,
    )?;
    write_helper_wrappers(&staging, helper_path)?;
    verify_managed_skill(&staging, helper_path)?;

    if destination.exists() {
        fs::rename(&destination, &previous).with_context(|| {
            format!(
                "无法暂存现有 Skill {} -> {}",
                destination.display(),
                previous.display()
            )
        })?;
    }
    if let Err(error) = fs::rename(&staging, &destination) {
        if previous.exists() {
            let _ = fs::rename(&previous, &destination);
        }
        return Err(error).with_context(|| "无法安装镜子AI生图 Skill");
    }
    if let Err(error) = verify_managed_skill(&destination, helper_path) {
        let mut rollback_errors = Vec::new();
        if let Err(rollback_error) = remove_tree_checked(&destination, &skills_root) {
            rollback_errors.push(rollback_error.to_string());
        }
        if previous.exists()
            && let Err(rollback_error) = fs::rename(&previous, &destination)
        {
            rollback_errors.push(rollback_error.to_string());
        }
        return if rollback_errors.is_empty() {
            Err(error).context("生图 Skill 落盘校验失败，已恢复本次操作前 Skill")
        } else {
            Err(error).context(format!(
                "生图 Skill 落盘校验失败，且旧 Skill 恢复不完整：{}",
                rollback_errors.join("；")
            ))
        };
    }
    if previous.exists() {
        // The new destination is already verified and committed. A locked
        // cleanup directory should not turn a successful install into a
        // reported failure; the next run retries this exact managed path.
        let _ = remove_tree_checked(&previous, &skills_root);
    }
    Ok(())
}

fn read_optional_file(path: &Path) -> anyhow::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn restore_optional_file(path: &Path, contents: Option<&[u8]>) -> anyhow::Result<()> {
    match contents {
        Some(bytes) => crate::settings::atomic_write(path, bytes),
        None if path.exists() => {
            fs::remove_file(path)?;
            Ok(())
        }
        None => Ok(()),
    }
}

fn verify_managed_skill(skill: &Path, helper_path: &Path) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(skill)
        .with_context(|| format!("生图 Skill 目录不存在：{}", skill.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("生图 Skill 不是普通目录：{}", skill.display());
    }
    for (relative, expected) in SKILL_FILES {
        verify_regular_file_contents(&skill.join(relative), expected)
            .with_context(|| format!("生图 Skill 文件校验失败：{relative}"))?;
    }
    let marker_path = skill.join(MANAGED_MARKER);
    let marker: serde_json::Value = serde_json::from_slice(
        &read_regular_file(&marker_path)
            .with_context(|| format!("无法读取生图 Skill 托管标记：{}", marker_path.display()))?,
    )
    .context("生图 Skill 托管标记不是有效 JSON")?;
    if marker.get("product").and_then(serde_json::Value::as_str) != Some("mirror x codex")
        || marker
            .get("source_repository")
            .and_then(serde_json::Value::as_str)
            != Some(SOURCE_REPOSITORY)
        || marker
            .get("source_commit")
            .and_then(serde_json::Value::as_str)
            != Some(SOURCE_COMMIT)
    {
        bail!("生图 Skill 托管标记与当前安装包不一致");
    }
    verify_regular_file_contents(
        &skill.join("scripts/jingzi-imagegen.cmd"),
        &windows_wrapper_bytes(),
    )
    .context("生图 Skill Windows wrapper 校验失败")?;
    verify_regular_file_contents(
        &skill.join("scripts/jingzi-imagegen"),
        &unix_wrapper_bytes(helper_path),
    )
    .context("生图 Skill Unix wrapper 校验失败")?;
    verify_regular_file_contents(
        &skill.join("scripts/jingzi-imagegen.ps1"),
        &windows_powershell_wrapper_bytes(helper_path),
    )
    .context("生图 Skill PowerShell wrapper 校验失败")?;
    let helper_metadata =
        fs::symlink_metadata(helper_path).context("无法读取安装包中的生图执行器")?;
    if helper_metadata.file_type().is_symlink() || !helper_metadata.is_file() {
        bail!(
            "安装包中的生图执行器不是普通文件：{}",
            helper_path.display()
        );
    }
    Ok(())
}

fn read_regular_file(path: &Path) -> anyhow::Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("不是普通文件：{}", path.display());
    }
    fs::read(path).map_err(Into::into)
}

fn verify_regular_file_contents(path: &Path, expected: &[u8]) -> anyhow::Result<()> {
    let actual = read_regular_file(path)?;
    if actual != expected {
        bail!("文件内容与安装包不一致：{}", path.display());
    }
    Ok(())
}

fn write_config(path: &Path, api_key: &str, helper_path: &Path) -> anyhow::Result<()> {
    let payload = serde_json::json!({
        "api_key": api_key,
        "base_url": DEFAULT_BASE_URL,
        "helper_path": helper_path,
    });
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    crate::settings::atomic_write(path, &(serde_json::to_vec_pretty(&payload)?))?;
    restrict_config_permissions(path)
}

fn verify_config(path: &Path, api_key: &str, helper_path: &Path) -> anyhow::Result<()> {
    let config = read_imagegen_config(path).context("生图配置落盘后无法读取")?;
    if config.api_key != api_key {
        bail!("生图配置落盘后的 Image Key 与本次验证值不一致");
    }
    if config.base_url.as_deref() != Some(DEFAULT_BASE_URL) {
        bail!("生图配置落盘后的 Base URL 不正确");
    }
    if config.helper_path.as_deref() != Some(helper_path) {
        bail!("生图配置落盘后的 Helper 路径不正确");
    }
    if !helper_path.is_file() {
        bail!("生图配置引用的 Helper 不存在：{}", helper_path.display());
    }
    Ok(())
}

fn config_has_key(path: &Path) -> bool {
    read_configured_key(path)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

fn read_configured_key(path: &Path) -> Option<String> {
    read_imagegen_config(path).ok().map(|config| config.api_key)
}

fn read_helper_path(path: &Path) -> Option<PathBuf> {
    read_imagegen_config(path).ok()?.helper_path
}

fn read_imagegen_config(path: &Path) -> anyhow::Result<ImagegenConfig> {
    let bytes =
        read_regular_file(path).with_context(|| format!("无法读取生图配置：{}", path.display()))?;
    serde_json::from_slice(&bytes).context("生图配置不是有效 JSON")
}

fn bundled_helper_path() -> PathBuf {
    let executable_name = if cfg!(windows) {
        "mirror-x-imagegen.exe"
    } else {
        "mirror-x-imagegen"
    };
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join(executable_name)))
        .unwrap_or_else(|| PathBuf::from(executable_name))
}

fn write_helper_wrappers(staging: &Path, helper_path: &Path) -> anyhow::Result<()> {
    let scripts = staging.join("scripts");
    fs::create_dir_all(&scripts)?;
    crate::settings::atomic_write(
        &scripts.join("jingzi-imagegen.cmd"),
        &windows_wrapper_bytes(),
    )?;
    crate::settings::atomic_write(
        &scripts.join("jingzi-imagegen.ps1"),
        &windows_powershell_wrapper_bytes(helper_path),
    )?;

    let unix_wrapper = scripts.join("jingzi-imagegen");
    crate::settings::atomic_write(&unix_wrapper, &unix_wrapper_bytes(helper_path))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&unix_wrapper, fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

fn windows_wrapper_bytes() -> Vec<u8> {
    b"@echo off\r\npowershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"%~dp0jingzi-imagegen.ps1\" %*\r\n".to_vec()
}

fn windows_powershell_wrapper_bytes(helper_path: &Path) -> Vec<u8> {
    let escaped = helper_path.to_string_lossy().replace('\'', "''");
    let script =
        format!("$ErrorActionPreference = 'Stop'\r\n& '{escaped}' @args\r\nexit $LASTEXITCODE\r\n");
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(script.as_bytes());
    bytes
}

fn unix_wrapper_bytes(helper_path: &Path) -> Vec<u8> {
    let escaped = helper_path.to_string_lossy().replace('\'', "'\\''");
    format!("#!/bin/sh\nexec '{escaped}' \"$@\"\n").into_bytes()
}

fn skill_path(codex_home: &Path) -> PathBuf {
    codex_home.join("skills").join(SKILL_NAME)
}

fn config_path() -> PathBuf {
    if let Some(path) = std::env::var_os("JINGZI_IMAGEGEN_CONFIG")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
    {
        return path;
    }
    directories::BaseDirs::new()
        .map(|dirs| {
            dirs.home_dir()
                .join(".config")
                .join("jingzi-imagegen")
                .join("config.json")
        })
        .unwrap_or_else(|| {
            PathBuf::from(".config")
                .join("jingzi-imagegen")
                .join("config.json")
        })
}

fn copy_tree(source: &Path, destination: &Path) -> anyhow::Result<()> {
    if !source.is_dir() {
        bail!("目录不存在：{}", source.display());
    }
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = destination.join(entry.file_name());
        if file_type.is_symlink() {
            bail!("为安全起见，不复制符号链接：{}", entry.path().display());
        }
        if file_type.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if file_type.is_file() {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

fn tree_contents_equal(left: &Path, right: &Path) -> anyhow::Result<bool> {
    let left_metadata = match fs::symlink_metadata(left) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| format!("无法读取目录 {}", left.display()));
        }
    };
    let right_metadata = match fs::symlink_metadata(right) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| format!("无法读取目录 {}", right.display()));
        }
    };
    if left_metadata.file_type().is_symlink() || right_metadata.file_type().is_symlink() {
        return Ok(false);
    }
    if left_metadata.is_file() || right_metadata.is_file() {
        return Ok(left_metadata.is_file()
            && right_metadata.is_file()
            && fs::read(left)? == fs::read(right)?);
    }
    if !left_metadata.is_dir() || !right_metadata.is_dir() {
        return Ok(false);
    }

    let mut left_entries = fs::read_dir(left)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<Vec<_>, _>>()?;
    let mut right_entries = fs::read_dir(right)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<Vec<_>, _>>()?;
    left_entries.sort();
    right_entries.sort();
    if left_entries != right_entries {
        return Ok(false);
    }
    for name in left_entries {
        if !tree_contents_equal(&left.join(&name), &right.join(name))? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn remove_tree_checked(path: &Path, allowed_parent: &Path) -> anyhow::Result<()> {
    let resolved_parent = allowed_parent
        .canonicalize()
        .with_context(|| format!("无法解析目录 {}", allowed_parent.display()))?;
    let resolved_path = path
        .canonicalize()
        .with_context(|| format!("无法解析目录 {}", path.display()))?;
    if resolved_path.parent() != Some(resolved_parent.as_path()) {
        bail!("拒绝移除预期目录之外的路径：{}", resolved_path.display());
    }
    fs::remove_dir_all(&resolved_path)
        .with_context(|| format!("无法移除目录 {}", resolved_path.display()))
}

#[cfg(unix)]
fn restrict_config_permissions(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict_config_permissions(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub async fn run_cli(args: &[String]) -> anyhow::Result<()> {
    if args
        .iter()
        .map(String::as_str)
        .eq(["register-key", "--stdin"])
    {
        let mut key = String::new();
        std::io::stdin()
            .read_to_string(&mut key)
            .context("无法从标准输入读取 Image Key")?;
        return register_key_at(
            &config_path(),
            &bundled_helper_path(),
            DEFAULT_BASE_URL,
            key.trim(),
        )
        .await;
    }
    let request = parse_cli_args(args)?;
    let config_path = config_path();
    run_request_with_config(request, &config_path).await
}

async fn register_key_at(
    config_path: &Path,
    helper_path: &Path,
    base_url: &str,
    api_key: &str,
) -> anyhow::Result<()> {
    let api_key = api_key.trim();
    if api_key.is_empty() {
        bail!("Image Key 不能为空");
    }
    if !helper_path.is_file() {
        bail!("生图执行器不存在，无法注册 Image Key");
    }
    let validated = validate_saved_or_provided_key_at(config_path, base_url, Some(api_key)).await?;
    let previous = read_optional_file(config_path)?;
    if let Err(error) = write_config(config_path, &validated, helper_path)
        .and_then(|_| verify_config(config_path, &validated, helper_path))
    {
        return match restore_optional_file(config_path, previous.as_deref()) {
            Ok(()) => Err(error).context("Image Key 注册失败，已恢复原配置"),
            Err(rollback_error) => Err(error).context(format!(
                "Image Key 注册失败，且原配置恢复失败：{rollback_error}"
            )),
        };
    }
    println!("镜子AI Image Key 已注册。");
    Ok(())
}

async fn run_request_with_config(request: CliRequest, config_path: &Path) -> anyhow::Result<()> {
    let config: serde_json::Value = serde_json::from_slice(
        &fs::read(config_path)
            .with_context(|| "镜子AI生图尚未配置，请先在 mirror x codex 中启用生图功能")?,
    )
    .with_context(|| "镜子AI生图配置无法解析")?;
    let api_key = config
        .get("api_key")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("镜子AI Image Key 未配置"))?;
    let base_url = config
        .get("base_url")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_BASE_URL)
        .trim_end_matches('/');

    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(20))
        .timeout(std::time::Duration::from_secs(240))
        .build()?;
    let response = client
        .post(format!("{base_url}/images/generations"))
        .bearer_auth(api_key)
        .json(&serde_json::json!({
            "model": "gpt-image-2",
            "prompt": request.prompt,
            "n": request.count,
            "size": request.size,
        }))
        .send()
        .await
        .with_context(|| "无法连接镜子AI生图服务")?;
    let status = response.status();
    let body = response.bytes().await?;
    if !status.is_success() {
        let preview = String::from_utf8_lossy(&body);
        let preview = preview.chars().take(500).collect::<String>();
        bail!("镜子AI生图请求失败（HTTP {status}）：{preview}");
    }
    let payload: serde_json::Value =
        serde_json::from_slice(&body).with_context(|| "镜子AI生图响应不是有效 JSON")?;
    let items = payload
        .get("data")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("镜子AI生图响应缺少 data 数组"))?;
    if items.len() < request.count {
        bail!(
            "镜子AI生图返回数量不足：请求 {} 张，实际 {} 张",
            request.count,
            items.len()
        );
    }

    let output_paths = output_paths(&request.output, request.count);
    for (item, output_path) in items.iter().zip(output_paths.iter()) {
        if output_path.exists() && !request.force {
            bail!(
                "输出文件已存在：{}。如需覆盖请添加 --force。",
                output_path.display()
            );
        }
        let bytes = image_bytes(&client, item, base_url).await?;
        if bytes.is_empty() {
            bail!("镜子AI生图返回了空图片数据，已拒绝写入空文件");
        }
        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent)?;
        }
        crate::settings::atomic_write(output_path, &bytes)?;
        println!("{}", output_path.display());
    }
    Ok(())
}

#[derive(Debug)]
struct CliRequest {
    prompt: String,
    output: PathBuf,
    size: String,
    count: usize,
    force: bool,
}

fn parse_cli_args(args: &[String]) -> anyhow::Result<CliRequest> {
    if args.first().map(String::as_str) != Some("generate") {
        bail!(
            "用法：mirror-x-imagegen generate --prompt <描述> --out <图片路径> [--size 1024x1024] [--n 1] [--force]"
        );
    }
    let mut prompt = None;
    let mut output = None;
    let mut size = "1024x1024".to_string();
    let mut count = 1usize;
    let mut force = false;
    let mut index = 1usize;
    while index < args.len() {
        match args[index].as_str() {
            "--prompt" => {
                index += 1;
                prompt = args.get(index).cloned();
            }
            "--out" => {
                index += 1;
                output = args.get(index).map(PathBuf::from);
            }
            "--size" => {
                index += 1;
                size = args
                    .get(index)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("--size 缺少值"))?;
            }
            "--n" => {
                index += 1;
                count = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--n 缺少值"))?
                    .parse()
                    .with_context(|| "--n 必须是整数")?;
            }
            "--force" => force = true,
            unknown => bail!("未知参数：{unknown}"),
        }
        index += 1;
    }
    let prompt = prompt
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("--prompt 不能为空"))?;
    let output = output.ok_or_else(|| anyhow::anyhow!("必须提供 --out"))?;
    if !(1..=10).contains(&count) {
        bail!("--n 必须在 1 到 10 之间");
    }
    Ok(CliRequest {
        prompt,
        output,
        size,
        count,
        force,
    })
}

fn output_paths(output: &Path, count: usize) -> Vec<PathBuf> {
    if count == 1 {
        return vec![output.to_path_buf()];
    }
    let parent = output.parent().unwrap_or_else(|| Path::new(""));
    let stem = output
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("image");
    let extension = output
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("png");
    (1..=count)
        .map(|index| parent.join(format!("{stem}-{index}.{extension}")))
        .collect()
}

async fn image_bytes(
    client: &reqwest::Client,
    item: &serde_json::Value,
    base_url: &str,
) -> anyhow::Result<Vec<u8>> {
    if let Some(value) = item
        .get("b64_json")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(value)
            .with_context(|| "镜子AI返回的 b64_json 无法解码");
        return bytes.and_then(non_empty_image_bytes);
    }
    let url = item
        .get("url")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("图片响应既没有非空 b64_json 也没有临时 URL"))?;
    let parsed = reqwest::Url::parse(url).with_context(|| "临时图片 URL 无效")?;
    let configured_base =
        reqwest::Url::parse(base_url).with_context(|| "镜子AI生图 base_url 无效")?;
    let same_origin = parsed.scheme() == configured_base.scheme()
        && parsed.host_str() == configured_base.host_str()
        && parsed.port_or_known_default() == configured_base.port_or_known_default();
    let trusted_default =
        parsed.scheme() == "https" && parsed.host_str() == Some("api.jingziai.club");
    if !same_origin && !trusted_default {
        bail!("拒绝下载非镜子AI域名的临时图片 URL");
    }
    let response = client
        .get(parsed)
        .header(reqwest::header::ACCEPT, "image/*")
        .send()
        .await
        .with_context(|| "下载临时图片失败")?;
    if !response.status().is_success() {
        bail!("下载临时图片失败（HTTP {}）", response.status());
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !content_type.starts_with("image/") {
        bail!("临时图片返回了非图片内容：{content_type}");
    }
    non_empty_image_bytes(response.bytes().await?.to_vec())
}

fn non_empty_image_bytes(bytes: Vec<u8>) -> anyhow::Result<Vec<u8>> {
    if bytes.is_empty() {
        bail!("镜子AI生图返回了空图片数据");
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enable_and_restore_preserve_existing_skill_and_config() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex");
        let state = temp.path().join("state");
        let config = temp.path().join("config").join("config.json");
        let helper = temp.path().join(if cfg!(windows) {
            "mirror-x-imagegen.exe"
        } else {
            "mirror-x-imagegen"
        });
        fs::write(&helper, "helper").unwrap();
        let original_skill = home.join("skills").join(SKILL_NAME);
        fs::create_dir_all(&original_skill).unwrap();
        fs::write(original_skill.join("user.txt"), "original").unwrap();

        fs::create_dir_all(config.parent().unwrap()).unwrap();
        fs::write(&config, r#"{"api_key":"original"}"#).unwrap();

        enable_at(&home, &state, &config, &helper, Some("sk-image-test")).unwrap();
        assert!(original_skill.join(MANAGED_MARKER).is_file());
        assert!(
            original_skill
                .join("references/key-registration.md")
                .is_file()
        );
        assert!(!original_skill.join("scripts/image_gen.py").exists());
        assert!(!original_skill.join("scripts/configure.py").exists());
        assert!(!original_skill.join("scripts/remove_chroma_key.py").exists());
        assert!(!original_skill.join("references/cli.md").exists());
        assert!(!original_skill.join("references/image-api.md").exists());
        assert!(!original_skill.join("references/codex-network.md").exists());
        assert!(!original_skill.join("references/sample-prompts.md").exists());
        let installed_instructions = fs::read_to_string(original_skill.join("SKILL.md")).unwrap();
        let installed_prompting =
            fs::read_to_string(original_skill.join("references/prompting.md")).unwrap();
        for forbidden in ["PowerShell", "scripts/", "<skill-dir>"] {
            assert!(!installed_instructions.contains(forbidden));
            assert!(!installed_prompting.contains(forbidden));
        }
        assert_eq!(
            read_configured_key(&config).as_deref(),
            Some("sk-image-test")
        );

        restore_baseline_at(&home, &state, &config).unwrap();
        assert_eq!(
            fs::read_to_string(original_skill.join("user.txt")).unwrap(),
            "original"
        );
        assert_eq!(read_configured_key(&config).as_deref(), Some("original"));
    }

    #[test]
    fn managed_helper_stays_outside_skill_tree_and_unicode_path_uses_powershell() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("中文用户").join("codex");
        let state = temp.path().join("state");
        let config = temp.path().join("config").join("config.json");
        let helper_dir = temp.path().join("中文安装目录");
        fs::create_dir_all(&helper_dir).unwrap();
        let helper = helper_dir.join(if cfg!(windows) {
            "mirror-x-imagegen.exe"
        } else {
            "mirror-x-imagegen"
        });
        fs::write(&helper, b"helper-binary").unwrap();

        enable_at(&home, &state, &config, &helper, Some("sk-image-test")).unwrap();

        let scripts = skill_path(&home).join("scripts");
        let windows_wrapper = fs::read(scripts.join("jingzi-imagegen.cmd")).unwrap();
        assert!(windows_wrapper.is_ascii());
        assert_eq!(windows_wrapper, windows_wrapper_bytes());
        assert!(!String::from_utf8_lossy(&windows_wrapper).contains("中文"));
        let powershell_wrapper = fs::read(scripts.join("jingzi-imagegen.ps1")).unwrap();
        assert!(powershell_wrapper.starts_with(&[0xEF, 0xBB, 0xBF]));
        assert_eq!(
            powershell_wrapper,
            windows_powershell_wrapper_bytes(&helper)
        );
        assert!(String::from_utf8_lossy(&powershell_wrapper).contains("中文安装目录"));
        assert!(!scripts.join("mirror-x-imagegen.exe").exists());
        assert!(!scripts.join("mirror-x-imagegen").exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_wrapper_executes_external_helper() {
        let temp = tempfile::tempdir().unwrap();
        let system_root = std::env::var_os("SystemRoot").unwrap();
        let helper = PathBuf::from(system_root).join("System32").join("cmd.exe");
        write_helper_wrappers(temp.path(), &helper).unwrap();

        let status = std::process::Command::new("cmd.exe")
            .arg("/c")
            .arg(temp.path().join("scripts/jingzi-imagegen.cmd"))
            .args(["/c", "exit", "0"])
            .status()
            .unwrap();

        assert!(status.success());
    }

    #[test]
    fn enable_refuses_a_tampered_baseline_before_replacing_current_files() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex");
        let state = temp.path().join("state");
        let config = temp.path().join("config").join("config.json");
        let helper = temp.path().join(if cfg!(windows) {
            "mirror-x-imagegen.exe"
        } else {
            "mirror-x-imagegen"
        });
        fs::write(&helper, "helper").unwrap();
        let original_skill = home.join("skills").join(SKILL_NAME);
        fs::create_dir_all(&original_skill).unwrap();
        fs::write(original_skill.join("user.txt"), "original").unwrap();
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        fs::write(&config, r#"{"api_key":"original"}"#).unwrap();

        enable_at(&home, &state, &config, &helper, Some("sk-first")).unwrap();
        let current_skill = fs::read(original_skill.join("SKILL.md")).unwrap();
        fs::write(
            state
                .join(BASELINE_ROOT)
                .join(BASELINE_SKILL_DIR)
                .join("user.txt"),
            "tampered",
        )
        .unwrap();

        let error = enable_at(&home, &state, &config, &helper, Some("sk-second")).unwrap_err();
        let error = format!("{error:#}");

        assert!(error.contains("baseline"));
        assert!(error.contains("校验失败"));
        assert_eq!(read_configured_key(&config).as_deref(), Some("sk-first"));
        assert_eq!(
            fs::read(original_skill.join("SKILL.md")).unwrap(),
            current_skill
        );
    }

    #[test]
    fn owned_state_removal_requires_imagegen_baseline_restore() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex");
        let state = temp.path().join("state");
        let config = temp.path().join("config").join("config.json");
        let helper = temp.path().join(if cfg!(windows) {
            "mirror-x-imagegen.exe"
        } else {
            "mirror-x-imagegen"
        });
        fs::write(&helper, "helper").unwrap();
        let original_skill = home.join("skills").join(SKILL_NAME);
        fs::create_dir_all(&original_skill).unwrap();
        fs::write(original_skill.join("user.txt"), "original").unwrap();
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        fs::write(&config, r#"{"api_key":"original"}"#).unwrap();

        enable_at(&home, &state, &config, &helper, Some("sk-image-test")).unwrap();
        assert!(
            ensure_restored_for_state_removal_at(&home, &state, &config)
                .unwrap_err()
                .to_string()
                .contains("仍由")
        );

        restore_baseline_at(&home, &state, &config).unwrap();
        ensure_restored_for_state_removal_at(&home, &state, &config).unwrap();

        fs::write(&config, r#"{"api_key":"changed-after-restore"}"#).unwrap();
        assert!(
            ensure_restored_for_state_removal_at(&home, &state, &config)
                .unwrap_err()
                .to_string()
                .contains("尚未恢复")
        );
    }

    #[test]
    fn owned_state_removal_refuses_managed_marker_without_baseline() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex");
        let state = temp.path().join("state");
        let config = temp.path().join("config.json");
        let skill = home.join("skills").join(SKILL_NAME);
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join(MANAGED_MARKER), "{}").unwrap();

        assert!(
            ensure_restored_for_state_removal_at(&home, &state, &config)
                .unwrap_err()
                .to_string()
                .contains("仍由")
        );
    }

    #[test]
    fn restore_refuses_managed_skill_when_baseline_is_missing() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex");
        let state = temp.path().join("state");
        let config = temp.path().join("config.json");
        let skill = home.join("skills").join(SKILL_NAME);
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join(MANAGED_MARKER), "{}").unwrap();

        let error = restore_baseline_at(&home, &state, &config)
            .unwrap_err()
            .to_string();

        assert!(error.contains("baseline 缺失"));
        assert!(skill.exists());
    }

    #[test]
    fn restore_refuses_non_file_baseline_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex");
        let state = temp.path().join("state");
        let config = temp.path().join("config.json");
        fs::create_dir_all(state.join(BASELINE_MANIFEST)).unwrap();

        let error = restore_baseline_at(&home, &state, &config)
            .unwrap_err()
            .to_string();

        assert!(error.contains("不是普通文件"));
    }

    #[test]
    fn enable_requires_a_key_when_no_existing_config_is_available() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex");
        let state = temp.path().join("state");
        let config = temp.path().join("config").join("config.json");
        let helper = temp.path().join("mirror-x-imagegen");
        fs::write(&helper, "helper").unwrap();

        let error = enable_at(&home, &state, &config, &helper, None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("Image Key"));
    }

    async fn models_server(payload: serde_json::Value) -> (String, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0u8; 8192];
            let read = socket.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.starts_with("GET /v1/models "));
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer sk-saved-image")
            );
            let body = payload.to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        (format!("http://{address}/v1"), server)
    }

    #[tokio::test]
    async fn saved_image_key_is_revalidated_before_reuse() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config.json");
        fs::write(&config, r#"{"api_key":"sk-saved-image"}"#).unwrap();
        let (base_url, server) = models_server(serde_json::json!({
            "data": [{ "id": "gpt-image-2" }]
        }))
        .await;

        let key = validate_saved_or_provided_key_at(&config, &base_url, None)
            .await
            .unwrap();

        server.await.unwrap();
        assert_eq!(key, "sk-saved-image");
    }

    #[tokio::test]
    async fn saved_image_key_without_image_model_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config.json");
        fs::write(&config, r#"{"api_key":"sk-saved-image"}"#).unwrap();
        let (base_url, server) = models_server(serde_json::json!({
            "data": [{ "id": "gpt-5.5" }]
        }))
        .await;

        let error = validate_saved_or_provided_key_at(&config, &base_url, None)
            .await
            .unwrap_err()
            .to_string();

        server.await.unwrap();
        assert!(error.contains("gpt-image-2"));
    }

    #[tokio::test]
    async fn ai_key_registration_validates_then_writes_only_independent_config() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("imagegen").join("config.json");
        let helper = temp.path().join(if cfg!(windows) {
            "mirror-x-imagegen.exe"
        } else {
            "mirror-x-imagegen"
        });
        fs::write(&helper, b"helper").unwrap();
        let (base_url, server) = models_server(serde_json::json!({
            "data": [{ "id": "gpt-image-2" }]
        }))
        .await;

        register_key_at(&config, &helper, &base_url, "sk-saved-image")
            .await
            .unwrap();

        server.await.unwrap();
        let saved = read_imagegen_config(&config).unwrap();
        assert_eq!(saved.api_key, "sk-saved-image");
        assert_eq!(saved.base_url.as_deref(), Some(DEFAULT_BASE_URL));
        assert_eq!(saved.helper_path.as_deref(), Some(helper.as_path()));
        assert!(!temp.path().join("config.toml").exists());
    }

    #[test]
    fn status_disables_a_managed_skill_when_required_files_are_damaged() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex");
        let state = temp.path().join("state");
        let config = temp.path().join("config").join("config.json");
        let helper = temp.path().join(if cfg!(windows) {
            "mirror-x-imagegen.exe"
        } else {
            "mirror-x-imagegen"
        });
        fs::write(&helper, "helper").unwrap();
        enable_at(&home, &state, &config, &helper, Some("sk-image-test")).unwrap();
        let healthy = status_at(&home, &state, &config);
        assert!(healthy.enabled);
        assert!(healthy.skill_available);

        fs::write(
            home.join("skills").join(SKILL_NAME).join("SKILL.md"),
            "damaged",
        )
        .unwrap();
        let damaged = status_at(&home, &state, &config);

        assert!(damaged.configured);
        assert!(damaged.helper_available);
        assert!(!damaged.skill_available);
        assert!(!damaged.enabled);
    }

    #[tokio::test]
    async fn bundled_helper_generates_from_b64_response_without_python() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0u8; 8192];
            let read = socket.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer sk-image-test")
            );
            assert!(request.contains("\"model\":\"gpt-image-2\""));
            let encoded = base64::engine::general_purpose::STANDARD.encode(b"mirror-image-bytes");
            let body = serde_json::json!({ "data": [{ "b64_json": encoded }] }).to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });

        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config.json");
        let output = temp.path().join("generated.png");
        fs::write(
            &config,
            serde_json::to_vec(&serde_json::json!({
                "api_key": "sk-image-test",
                "base_url": format!("http://{address}/v1"),
            }))
            .unwrap(),
        )
        .unwrap();
        let request = CliRequest {
            prompt: "一只猫".to_string(),
            output: output.clone(),
            size: "1024x1024".to_string(),
            count: 1,
            force: false,
        };
        run_request_with_config(request, &config).await.unwrap();
        server.await.unwrap();
        assert_eq!(fs::read(output).unwrap(), b"mirror-image-bytes");
    }

    #[tokio::test]
    async fn bundled_helper_falls_back_to_url_when_b64_json_is_empty() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for request_index in 0..2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = vec![0u8; 8192];
                let read = socket.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..read]);
                if request_index == 0 {
                    assert!(request.starts_with("POST /v1/images/generations "));
                    let body = serde_json::json!({
                        "data": [{
                            "b64_json": "",
                            "url": format!("http://{address}/generated.png")
                        }]
                    })
                    .to_string();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    socket.write_all(response.as_bytes()).await.unwrap();
                } else {
                    assert!(request.starts_with("GET /generated.png "));
                    let image = b"mirror-image-from-url";
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        image.len()
                    );
                    socket.write_all(response.as_bytes()).await.unwrap();
                    socket.write_all(image).await.unwrap();
                }
            }
        });

        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config.json");
        let output = temp.path().join("generated.png");
        fs::write(
            &config,
            serde_json::to_vec(&serde_json::json!({
                "api_key": "sk-image-test",
                "base_url": format!("http://{address}/v1"),
            }))
            .unwrap(),
        )
        .unwrap();
        let request = CliRequest {
            prompt: "一只猫".to_string(),
            output: output.clone(),
            size: "1024x1024".to_string(),
            count: 1,
            force: false,
        };

        run_request_with_config(request, &config).await.unwrap();
        server.await.unwrap();
        assert_eq!(fs::read(output).unwrap(), b"mirror-image-from-url");
    }

    #[tokio::test]
    async fn bundled_helper_rejects_empty_download_without_writing_output() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for request_index in 0..2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = vec![0u8; 8192];
                let read = socket.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..read]);
                if request_index == 0 {
                    assert!(request.starts_with("POST /v1/images/generations "));
                    let body = serde_json::json!({
                        "data": [{
                            "b64_json": "   ",
                            "url": format!("http://{address}/empty.png")
                        }]
                    })
                    .to_string();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    socket.write_all(response.as_bytes()).await.unwrap();
                } else {
                    assert!(request.starts_with("GET /empty.png "));
                    socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .await
                        .unwrap();
                }
            }
        });

        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config.json");
        let output = temp.path().join("generated.png");
        fs::write(
            &config,
            serde_json::to_vec(&serde_json::json!({
                "api_key": "sk-image-test",
                "base_url": format!("http://{address}/v1"),
            }))
            .unwrap(),
        )
        .unwrap();
        let request = CliRequest {
            prompt: "一只猫".to_string(),
            output: output.clone(),
            size: "1024x1024".to_string(),
            count: 1,
            force: false,
        };

        let error = run_request_with_config(request, &config)
            .await
            .unwrap_err()
            .to_string();
        server.await.unwrap();
        assert!(error.contains("空图片数据"));
        assert!(!output.exists());
    }

    #[test]
    fn imagegen_restore_refuses_a_different_codex_home_without_writes() {
        let temp = tempfile::tempdir().unwrap();
        let home_a = temp.path().join("codex-a");
        let home_b = temp.path().join("codex-b");
        let state = temp.path().join("state");
        let config = temp.path().join("imagegen-config.json");
        fs::create_dir_all(&home_a).unwrap();
        fs::create_dir_all(&home_b).unwrap();
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(skill_path(&home_a)).unwrap();
        fs::write(skill_path(&home_a).join("original.txt"), b"home-a").unwrap();
        fs::write(&config, b"home-a-config").unwrap();
        ensure_baseline(&home_a, &state, &config).unwrap();

        fs::create_dir_all(skill_path(&home_b)).unwrap();
        let home_b_skill = b"keep-home-b";
        let home_b_config = b"keep-home-b-config";
        fs::write(skill_path(&home_b).join("keep.txt"), home_b_skill).unwrap();
        fs::write(&config, home_b_config).unwrap();

        let error = restore_baseline_at(&home_b, &state, &config).unwrap_err();

        assert!(error.to_string().contains("CODEX_HOME"), "{error:#}");
        assert_eq!(
            fs::read(skill_path(&home_b).join("keep.txt")).unwrap(),
            home_b_skill
        );
        assert_eq!(fs::read(&config).unwrap(), home_b_config);
    }
}
