use crate::runtime::atomic_write;
use serde::Serialize;
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use anyhow::Context as _;

pub const MAX_AVATAR_BYTES: u64 = 256 * 1024 * 1024;
static AVATAR_IMPORT_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyCandidate {
    pub root: String,
    pub has_config: bool,
    pub has_secrets: bool,
    pub has_memory: bool,
    pub has_avatar: bool,
}

pub fn detect_legacy_roots(current_resource: &Path, current_data: &Path) -> Vec<LegacyCandidate> {
    let mut roots = BTreeSet::new();
    for key in ["FURINA_DESKTOP_ROOT", "FURINA_AGENT_ROOT"] {
        if let Ok(value) = env::var(key) { add_candidate_path(&mut roots, PathBuf::from(value)); }
    }
    if let Ok(mut current) = env::current_dir() {
        loop {
            add_candidate_path(&mut roots, current.clone());
            if !current.pop() { break; }
        }
    }
    #[cfg(windows)]
    for drive in b'C'..=b'Z' {
        let drive_root = PathBuf::from(format!("{}:/", drive as char));
        if !drive_root.is_dir() { continue; }
        for name in ["project", "projects", "source", "src", "dev", "repos"] {
            let root = drive_root.join(name);
            if root.is_dir() { scan_conventional_root(&root, 0, 3, &mut roots); }
        }
    }
    roots
        .into_iter()
        .filter(|root| root != current_resource && root != current_data)
        .filter(|root| is_desktop_root(root))
        .map(|root| LegacyCandidate {
            has_config: root.join(".furina/config.yaml").is_file(),
            has_secrets: root.join(".furina/secrets.env").is_file(),
            has_memory: has_memory(&root.join(".furina/memory")),
            has_avatar: root.join(".furina/avatar/Furina.vrm").is_file(),
            root: root.display().to_string(),
        })
        .collect()
}

fn add_candidate_path(roots: &mut BTreeSet<PathBuf>, path: PathBuf) {
    if let Ok(path) = path.canonicalize() { roots.insert(path); }
}

fn scan_conventional_root(root: &Path, depth: usize, max_depth: usize, roots: &mut BTreeSet<PathBuf>) {
    if is_desktop_root(root) { add_candidate_path(roots, root.to_path_buf()); }
    if depth >= max_depth { return; }
    let Ok(entries) = fs::read_dir(root) else { return; };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() { continue; }
        let name = path.file_name().and_then(|value| value.to_str()).unwrap_or("");
        if name.starts_with('.') || matches!(name, "target" | "node_modules" | "Windows" | "Program Files" | "ProgramData") { continue; }
        scan_conventional_root(&path, depth + 1, max_depth, roots);
    }
}

pub fn is_desktop_root(root: &Path) -> bool {
    root.join(".furina/config.yaml").is_file()
        && root.join("persona/furina.yaml").is_file()
        && root.join("python/furina_tools/server.py").is_file()
}

pub fn migrate_legacy_data(source: &Path, destination: &Path) -> anyhow::Result<serde_json::Value> {
    let source = source.canonicalize()?;
    if !is_desktop_root(&source) { anyhow::bail!("所选目录不是 Furina Desktop 数据源"); }
    if destination.join(".furina/migration.json").exists() { anyhow::bail!("该安装目录已经完成过迁移"); }
    if destination_has_user_data(destination) { anyhow::bail!("目标目录已经包含用户数据，请改用手动导入"); }

    let source_furina = source.join(".furina");
    let target_furina = destination.join(".furina");
    fs::create_dir_all(&target_furina)?;
    let mut copied = Vec::new();
    for name in ["config.yaml", "secrets.env"] {
        let from = source_furina.join(name);
        if from.is_file() { copy_file(&from, &target_furina.join(name))?; copied.push(name.to_string()); }
    }
    let source_memory = source_furina.join("memory");
    let target_memory = target_furina.join("memory");
    if source_memory.is_dir() {
        fs::create_dir_all(&target_memory)?;
        for entry in fs::read_dir(&source_memory)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() || path.file_name().and_then(|value| value.to_str()) == Some("instance.lock") { continue; }
            copy_file(&path, &target_memory.join(entry.file_name()))?;
        }
        copied.push("memory".into());
    }
    let avatar = source_furina.join("avatar/Furina.vrm");
    if avatar.is_file() {
        validate_vrm_file(&avatar)?;
        fs::create_dir_all(target_furina.join("avatar"))?;
        copy_file(&avatar, &target_furina.join("avatar/Furina.vrm"))?;
        copied.push("avatar/Furina.vrm".into());
    }
    let marker = serde_json::json!({
        "source": source.display().to_string(),
        "migratedAt": now_ms(),
        "copied": copied,
    });
    atomic_write(&target_furina.join("migration.json"), serde_json::to_vec_pretty(&marker)?.as_slice())?;
    Ok(marker)
}

pub fn import_avatar(source: &Path, data_root: &Path) -> anyhow::Result<u64> {
    let _guard = AVATAR_IMPORT_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| anyhow::anyhow!("Avatar 导入锁已损坏"))?;
    let metadata = validate_vrm_file(source)
        .with_context(|| format!("无法读取所选 Avatar：{}", source.display()))?;
    let avatar_dir = data_root.join(".furina/avatar");
    fs::create_dir_all(&avatar_dir)?;
    let target = avatar_dir.join("Furina.vrm");
    let source_canonical = source.canonicalize()?;
    if target.is_file() && target.canonicalize()? == source_canonical {
        return Ok(metadata.len());
    }
    let temp = avatar_dir.join(format!(
        ".Furina.{}.{}.tmp.vrm",
        std::process::id(),
        now_ms()
    ));
    fs::copy(source, &temp)
        .with_context(|| format!("无法复制 Avatar 到临时文件：{}", temp.display()))?;
    if let Err(error) = validate_vrm_file(&temp) {
        let _ = fs::remove_file(&temp);
        return Err(error.context("临时 Avatar 校验失败"));
    }
    let backup = avatar_dir.join("Furina.vrm.backup");
    if target.is_file() {
        if backup.exists() {
            fs::remove_file(&backup)
                .with_context(|| format!("无法删除旧 Avatar 备份：{}", backup.display()))?;
        }
        fs::copy(&target, &backup)
            .with_context(|| format!("无法备份当前 Avatar：{}", backup.display()))?;
        fs::remove_file(&target)
            .with_context(|| format!("无法替换当前 Avatar：{}", target.display()))?;
    }
    if let Err(error) = fs::rename(&temp, &target) {
        let _ = fs::remove_file(&temp);
        if backup.is_file() { let _ = fs::copy(&backup, &target); }
        return Err(error).with_context(|| format!(
            "无法启用新 Avatar：{} → {}",
            temp.display(),
            target.display()
        ));
    }
    Ok(metadata.len())
}

pub fn validate_vrm_file(path: &Path) -> anyhow::Result<fs::Metadata> {
    if path.extension().and_then(|value| value.to_str()).map(|value| value.eq_ignore_ascii_case("vrm")) != Some(true) {
        anyhow::bail!("Avatar 资产必须使用 .vrm 扩展名");
    }
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() { anyhow::bail!("Avatar 资产不是普通文件"); }
    if metadata.len() == 0 || metadata.len() > MAX_AVATAR_BYTES { anyhow::bail!("Avatar 资产大小无效或超过 256 MiB"); }

    let mut file = fs::File::open(path)?;
    let mut header = [0_u8; 20];
    file.read_exact(&mut header).map_err(|_| anyhow::anyhow!("Avatar 资产缺少完整 glTF 头"))?;
    if &header[0..4] != b"glTF" || u32::from_le_bytes(header[4..8].try_into().unwrap()) != 2 {
        anyhow::bail!("Avatar 资产不是有效的 VRM/glTF 2.0 文件");
    }
    let declared_len = u32::from_le_bytes(header[8..12].try_into().unwrap()) as u64;
    let json_len = u32::from_le_bytes(header[12..16].try_into().unwrap()) as usize;
    let json_kind = u32::from_le_bytes(header[16..20].try_into().unwrap());
    if declared_len != metadata.len() || json_kind != 0x4E4F534A || json_len == 0 || json_len as u64 > metadata.len().saturating_sub(20) {
        anyhow::bail!("Avatar 资产的 glTF JSON 区块无效");
    }
    let mut json_bytes = vec![0_u8; json_len];
    file.read_exact(&mut json_bytes)?;
    while matches!(json_bytes.last(), Some(b' ' | 0)) { json_bytes.pop(); }
    let document: serde_json::Value = serde_json::from_slice(&json_bytes).map_err(|error| anyhow::anyhow!("Avatar glTF JSON 解析失败: {error}"))?;
    let extensions_used = document.get("extensionsUsed").and_then(|value| value.as_array()).into_iter().flatten()
        .filter_map(|value| value.as_str());
    let has_vrm_extension = extensions_used.clone().any(|name| matches!(name, "VRM" | "VRMC_vrm"))
        || document.get("extensions").and_then(|value| value.as_object())
            .map(|extensions| extensions.contains_key("VRM") || extensions.contains_key("VRMC_vrm"))
            .unwrap_or(false);
    if !has_vrm_extension { anyhow::bail!("所选 glTF 文件不包含 VRM 扩展"); }
    Ok(metadata)
}

fn destination_has_user_data(destination: &Path) -> bool {
    let furina = destination.join(".furina");
    furina.join("secrets.env").is_file()
        || furina.join("avatar/Furina.vrm").is_file()
        || has_memory(&furina.join("memory"))
}

fn has_memory(path: &Path) -> bool {
    fs::read_dir(path).ok().into_iter().flatten().flatten().any(|entry| {
        let path = entry.path();
        path.is_file()
            && path.file_name().and_then(|value| value.to_str()) != Some("instance.lock")
            && path.metadata().map(|metadata| metadata.len() > 0).unwrap_or(false)
    })
}

fn copy_file(source: &Path, destination: &Path) -> anyhow::Result<()> {
    if let Some(parent) = destination.parent() { fs::create_dir_all(parent)?; }
    fs::copy(source, destination)?;
    Ok(())
}

fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("furina-{name}-{}", std::process::id()))
    }

    fn create_legacy(root: &Path) {
        fs::create_dir_all(root.join(".furina/memory")).unwrap();
        fs::create_dir_all(root.join("persona")).unwrap();
        fs::create_dir_all(root.join("python/furina_tools")).unwrap();
        fs::write(root.join(".furina/config.yaml"), "persona: furina\n").unwrap();
        fs::write(root.join(".furina/secrets.env"), "FURINA_API_KEY=test\n").unwrap();
        fs::write(root.join(".furina/memory/emotion.json"), "{}").unwrap();
        fs::write(root.join(".furina/memory/instance.lock"), "lock").unwrap();
        fs::write(root.join("persona/furina.yaml"), "dialogue_style: test\n").unwrap();
        fs::write(root.join("python/furina_tools/server.py"), "").unwrap();
    }

    fn write_glb(path: &Path, json: &str) {
        let mut json = json.as_bytes().to_vec();
        while json.len() % 4 != 0 { json.push(b' '); }
        let total = 20 + json.len();
        let mut bytes = Vec::with_capacity(total);
        bytes.extend_from_slice(b"glTF");
        bytes.extend_from_slice(&2_u32.to_le_bytes());
        bytes.extend_from_slice(&(total as u32).to_le_bytes());
        bytes.extend_from_slice(&(json.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&0x4E4F534A_u32.to_le_bytes());
        bytes.extend_from_slice(&json);
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn validates_vrm_extension_in_glb() {
        let root = temp_root("vrm-validation");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let valid = root.join("valid.vrm");
        let invalid = root.join("invalid.vrm");
        write_glb(&valid, r#"{"asset":{"version":"2.0"},"extensionsUsed":["VRMC_vrm"]}"#);
        write_glb(&invalid, r#"{"asset":{"version":"2.0"}}"#);
        assert!(validate_vrm_file(&valid).is_ok());
        assert!(validate_vrm_file(&invalid).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn imports_avatar_and_keeps_backup_when_replacing() {
        let source = temp_root("avatar-import-source");
        let destination = temp_root("avatar-import-destination");
        let _ = fs::remove_dir_all(&source);
        let _ = fs::remove_dir_all(&destination);
        fs::create_dir_all(&source).unwrap();
        write_glb(&source.join("source.vrm"), r#"{"asset":{"version":"2.0"},"extensionsUsed":["VRM"]}"#);
        let first_size = import_avatar(&source.join("source.vrm"), &destination).unwrap();
        assert!(first_size > 0);
        assert!(destination.join(".furina/avatar/Furina.vrm").is_file());
        write_glb(&source.join("replacement.vrm"), r#"{"asset":{"version":"2.0"},"extensionsUsed":["VRMC_vrm"]}"#);
        import_avatar(&source.join("replacement.vrm"), &destination).unwrap();
        assert!(destination.join(".furina/avatar/Furina.vrm").is_file());
        assert!(destination.join(".furina/avatar/Furina.vrm.backup").is_file());
        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(destination);
    }

    #[test]
    fn failed_avatar_import_leaves_existing_model_untouched() {
        let source = temp_root("avatar-import-failure-source");
        let destination = temp_root("avatar-import-failure-destination");
        let _ = fs::remove_dir_all(&source);
        let _ = fs::remove_dir_all(&destination);
        fs::create_dir_all(&source).unwrap();
        let source_file = source.join("invalid.vrm");
        fs::write(&source_file, b"not a vrm").unwrap();
        let avatar_dir = destination.join(".furina/avatar");
        fs::create_dir_all(&avatar_dir).unwrap();
        let existing = avatar_dir.join("Furina.vrm");
        fs::write(&existing, b"existing avatar").unwrap();
        assert!(import_avatar(&source_file, &destination).is_err());
        assert_eq!(fs::read(&existing).unwrap(), b"existing avatar");
        assert!(!avatar_dir.join("Furina.vrm.backup").exists());
        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(destination);
    }

    #[test]
    fn concurrent_avatar_imports_are_serialized() {
        let source = temp_root("avatar-import-concurrent-source");
        let destination = temp_root("avatar-import-concurrent-destination");
        let _ = fs::remove_dir_all(&source);
        let _ = fs::remove_dir_all(&destination);
        fs::create_dir_all(&source).unwrap();
        let first = source.join("first.vrm");
        let second = source.join("second.vrm");
        write_glb(&first, r#"{"asset":{"version":"2.0"},"extensionsUsed":["VRM"],"extras":{"source":"first"}}"#);
        write_glb(&second, r#"{"asset":{"version":"2.0"},"extensionsUsed":["VRMC_vrm"],"extras":{"source":"second"}}"#);
        let destination_a = destination.clone();
        let destination_b = destination.clone();
        let first_task = std::thread::spawn(move || import_avatar(&first, &destination_a));
        let second_task = std::thread::spawn(move || import_avatar(&second, &destination_b));
        assert!(first_task.join().unwrap().is_ok());
        assert!(second_task.join().unwrap().is_ok());
        assert!(destination.join(".furina/avatar/Furina.vrm").is_file());
        assert!(destination.join(".furina/avatar/Furina.vrm.backup").is_file());
        assert!(!fs::read_dir(destination.join(".furina/avatar")).unwrap().flatten().any(|entry| entry.file_name().to_string_lossy().contains(".tmp.vrm")));
        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(destination);
    }

    #[test]
    fn importing_current_avatar_is_a_safe_noop() {
        let destination = temp_root("avatar-import-same-path");
        let _ = fs::remove_dir_all(&destination);
        let avatar_dir = destination.join(".furina/avatar");
        fs::create_dir_all(&avatar_dir).unwrap();
        let current = avatar_dir.join("Furina.vrm");
        write_glb(&current, r#"{"asset":{"version":"2.0"},"extensionsUsed":["VRMC_vrm"]}"#);
        let before = fs::read(&current).unwrap();

        let size = import_avatar(&current, &destination).unwrap();

        assert_eq!(size, before.len() as u64);
        assert_eq!(fs::read(&current).unwrap(), before);
        assert!(!avatar_dir.join("Furina.vrm.backup").exists());
        let _ = fs::remove_dir_all(destination);
    }

    #[test]
    #[ignore]
    fn real_avatar_import_manual() {
        let source = std::env::var("FURINA_TEST_AVATAR_SOURCE").expect("设置 FURINA_TEST_AVATAR_SOURCE");
        let destination = temp_root("avatar-import-real");
        let _ = fs::remove_dir_all(&destination);
        let size = import_avatar(Path::new(&source), &destination).unwrap();
        assert!(size > 20);
        assert_eq!(fs::metadata(destination.join(".furina/avatar/Furina.vrm")).unwrap().len(), size);
        let _ = fs::remove_dir_all(destination);
    }

    #[test]
    fn migration_copies_user_data_but_not_lock() {
        let source = temp_root("migration-source");
        let destination = temp_root("migration-destination");
        let _ = fs::remove_dir_all(&source);
        let _ = fs::remove_dir_all(&destination);
        create_legacy(&source);
        migrate_legacy_data(&source, &destination).unwrap();
        assert!(destination.join(".furina/config.yaml").is_file());
        assert!(destination.join(".furina/secrets.env").is_file());
        assert!(destination.join(".furina/memory/emotion.json").is_file());
        assert!(!destination.join(".furina/memory/instance.lock").exists());
        assert!(source.join(".furina/secrets.env").is_file());
        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(destination);
    }

    #[test]
    fn migration_rejects_existing_user_data() {
        let source = temp_root("migration-conflict-source");
        let destination = temp_root("migration-conflict-destination");
        let _ = fs::remove_dir_all(&source);
        let _ = fs::remove_dir_all(&destination);
        create_legacy(&source);
        fs::create_dir_all(destination.join(".furina/memory")).unwrap();
        fs::write(destination.join(".furina/memory/emotion.json"), "{}").unwrap();
        assert!(migrate_legacy_data(&source, &destination).is_err());
        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(destination);
    }
}
