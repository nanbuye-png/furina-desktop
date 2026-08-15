use crate::config::SafeAppConfig;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovedApp {
    pub app_id: String,
    pub label: String,
    pub executable: String,
    pub args: Vec<String>,
    pub approved_at_ms: u128,
}

#[derive(Debug, Clone)]
pub struct AppApprovalStore {
    path: PathBuf,
    entries: Vec<ApprovedApp>,
}

impl AppApprovalStore {
    pub fn load(path: PathBuf) -> Self {
        let entries = fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<Vec<ApprovedApp>>(&text).ok())
            .unwrap_or_default();
        Self { path, entries }
    }

    pub fn is_approved(&self, app: &SafeAppConfig) -> bool {
        let Some(target) = normalized_target(app) else { return false; };
        self.entries.iter().any(|entry| {
            entry.app_id == app.id
                && entry.executable == target
                && entry.args == app.args
        })
    }

    pub fn approve(&mut self, app: &SafeAppConfig) -> anyhow::Result<()> {
        let target = normalized_target(app)
            .ok_or_else(|| anyhow::anyhow!("应用启动路径不存在：{}", app.executable))?;
        self.entries.retain(|entry| entry.app_id != app.id);
        self.entries.push(ApprovedApp {
            app_id: app.id.clone(),
            label: app.label.clone(),
            executable: target,
            args: app.args.clone(),
            approved_at_ms: now_ms(),
        });
        self.save()
    }

    pub fn launch(&self, app: &SafeAppConfig) -> anyhow::Result<()> {
        let target = normalized_target(app)
            .ok_or_else(|| anyhow::anyhow!("应用启动路径不存在：{}", app.executable))?;
        Command::new(&target)
            .args(&app.args)
            .spawn()
            .map(|_| ())
            .map_err(|error| anyhow::anyhow!("启动{}失败：{}", app.label, error))
    }

    #[cfg(test)]
    pub fn entries(&self) -> &[ApprovedApp] { &self.entries }

    fn save(&self) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_vec_pretty(&self.entries)?;
        let temp = self.path.with_extension("json.tmp");
        fs::write(&temp, data)?;
        match fs::rename(&temp, &self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                fs::remove_file(&self.path)?;
                fs::rename(temp, &self.path).map_err(Into::into)
            }
            Err(error) => Err(error.into()),
        }
    }
}

pub fn resolve_app_target(app: &SafeAppConfig) -> Option<SafeAppConfig> {
    let mut resolved = app.clone();
    if let Some(target) = normalized_path(app.executable.trim()) {
        resolved.executable = target;
        return Some(resolved);
    }

    let executable_name = executable_name_for(app)?;
    for candidate in discovery_candidates(app, &executable_name) {
        if let Some(target) = normalized_path(&candidate.to_string_lossy()) {
            resolved.executable = target;
            return Some(resolved);
        }
    }
    None
}

fn executable_name_for(app: &SafeAppConfig) -> Option<String> {
    let configured_name = app
        .executable
        .replace('\\', "/")
        .rsplit('/')
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if configured_name.to_ascii_lowercase().ends_with(".exe") {
        return Some(configured_name);
    }

    let key = format!("{}{}", app.id, app.label)
        .to_ascii_lowercase()
        .replace(['_', '-', ' ', '.'], "");
    let known_name = if key.contains("qqmusic") || key.contains("qq音乐") {
        Some("QQMusic.exe")
    } else if key.contains("qq") || key.contains("qqim") || key.contains("qq聊天") {
        Some("QQ.exe")
    } else if key.contains("wechat") || key.contains("微信") {
        Some("WeChat.exe")
    } else if key.contains("discord") {
        Some("Discord.exe")
    } else if key.contains("spotify") {
        Some("Spotify.exe")
    } else if key.contains("steam") {
        Some("steam.exe")
    } else if key.contains("chrome") {
        Some("chrome.exe")
    } else if key.contains("edge") {
        Some("msedge.exe")
    } else if key.contains("notepad") || key.contains("记事本") {
        Some("notepad.exe")
    } else {
        None
    };
    if let Some(name) = known_name {
        return Some(name.into());
    }

    let derived = app.id.trim().trim_end_matches(".exe");
    if derived.is_empty() || derived.len() > 96 {
        return None;
    }
    Some(format!("{derived}.exe"))
}

fn discovery_candidates(app: &SafeAppConfig, executable_name: &str) -> Vec<PathBuf> {
    let app_folder = executable_name.trim_end_matches(".exe");
    let mut candidates = Vec::new();
    if !app.executable.trim().is_empty() {
        candidates.push(PathBuf::from(app.executable.trim()));
    }

    if let Some(path) = env::var_os("PATH") {
        for root in env::split_paths(&path) {
            candidates.push(root.join(executable_name));
        }
    }

    if let Some(root) = env::var_os("WINDIR").map(PathBuf::from) {
        candidates.push(root.join("System32").join(executable_name));
        candidates.push(root.join(executable_name));
    }

    for variable in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA", "APPDATA"] {
        if let Some(root) = env::var_os(variable).map(PathBuf::from) {
            candidates.push(root.join("Tencent").join(app_folder).join(executable_name));
            candidates.push(root.join(app_folder).join(executable_name));
            candidates.push(root.join("Programs").join(app_folder).join(executable_name));
        }
    }

    #[cfg(windows)]
    for letter in b'C'..=b'Z' {
        let root = PathBuf::from(format!("{}:\\", letter as char));
        if !root.exists() {
            continue;
        }
        candidates.push(root.join(executable_name));
        candidates.push(root.join(app_folder).join(executable_name));
        candidates.push(root.join("Applications").join(app_folder).join(executable_name));
        candidates.push(root.join("Apps").join(app_folder).join(executable_name));
        candidates.push(root.join("Tencent").join(app_folder).join(executable_name));
        candidates.push(root.join("Tencent").join(executable_name));
        candidates.push(root.join(app_folder).join("Bin").join(executable_name));
        candidates.push(root.join("Program Files").join("Tencent").join(app_folder).join(executable_name));
        candidates.push(root.join("Program Files (x86)").join("Tencent").join(app_folder).join(executable_name));
    }

    candidates
}

fn normalized_target(app: &SafeAppConfig) -> Option<String> {
    normalized_path(app.executable.trim())
}

fn normalized_path(value: &str) -> Option<String> {
    let path = Path::new(value);
    if path.as_os_str().is_empty() || !path.is_file() {
        return None;
    }
    Some(path.canonicalize().unwrap_or_else(|_| path.to_path_buf()).to_string_lossy().to_string())
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(path: &Path) -> SafeAppConfig {
        SafeAppConfig {
            id: "test_app".into(),
            label: "测试应用".into(),
            executable: path.display().to_string(),
            args: vec!["--safe".into()],
            enabled: true,
        }
    }

    #[test]
    fn approval_persists_exact_target_and_args() {
        let root = env::temp_dir().join(format!("furina_apps_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let executable = root.join("app.exe");
        fs::write(&executable, b"test").unwrap();
        let path = root.join("approved_apps.json");
        let configured = app(&executable);
        let mut store = AppApprovalStore::load(path.clone());
        assert!(!store.is_approved(&configured));
        store.approve(&configured).unwrap();
        assert!(store.is_approved(&configured));
        let loaded = AppApprovalStore::load(path);
        assert!(loaded.is_approved(&configured));
        assert_eq!(loaded.entries().len(), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn changed_args_require_new_approval() {
        let root = env::temp_dir().join(format!("furina_apps_args_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let executable = root.join("app.exe");
        fs::write(&executable, b"test").unwrap();
        let path = root.join("approved_apps.json");
        let configured = app(&executable);
        let mut store = AppApprovalStore::load(path.clone());
        store.approve(&configured).unwrap();
        let mut changed = configured.clone();
        changed.args = vec!["--different".into()];
        assert!(!store.is_approved(&changed));
        store.approve(&changed).unwrap();
        let loaded = AppApprovalStore::load(path);
        assert!(loaded.is_approved(&changed));
        assert!(!loaded.is_approved(&configured));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resolver_accepts_an_existing_discovered_candidate() {
        let root = env::temp_dir().join(format!("furina_apps_resolve_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let executable = root.join("QQMusic.exe");
        fs::write(&executable, b"test").unwrap();
        let configured = SafeAppConfig {
            id: "qq_music".into(),
            label: "QQ音乐".into(),
            executable: executable.display().to_string(),
            args: Vec::new(),
            enabled: true,
        };
        let resolved = resolve_app_target(&configured).unwrap();
        assert!(Path::new(&resolved.executable).is_file());
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn resolver_finds_root_level_qq_executable_without_configured_path() {
        let root = PathBuf::from(format!("{}:\\", b'Z' as char));
        let executable = root.join("QQ.exe");
        if !root.exists() || !fs::write(&executable, b"test").is_ok() {
            return;
        }
        let configured = SafeAppConfig {
            id: "qq".into(),
            label: "QQ".into(),
            executable: String::new(),
            args: Vec::new(),
            enabled: true,
        };
        let resolved = resolve_app_target(&configured).unwrap();
        assert_eq!(Path::new(&resolved.executable), executable.canonicalize().unwrap());
        let _ = fs::remove_file(executable);
    }
}

