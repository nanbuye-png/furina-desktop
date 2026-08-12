use furina_core::app::{self, RuntimePaths};
use furina_core::config::Config;
use furina_core::sidecar::SidecarLaunch;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use tauri::Manager;

#[derive(Clone, Debug, Serialize)]
pub struct RuntimeInfo {
    pub mode: String,
    pub resource_root: String,
    pub data_root: String,
    pub workspace_root: String,
    pub sidecar: String,
    pub legacy_root_env: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct DesktopPreferences {
    #[serde(default)]
    pub workspace: String,
    #[serde(default)]
    pub setup_completed: bool,
}

pub struct ResolvedRuntime {
    pub paths: RuntimePaths,
    pub info: RuntimeInfo,
}

pub fn resolve(app_handle: &tauri::AppHandle) -> anyhow::Result<ResolvedRuntime> {
    let exe = env::current_exe()?;
    let exe_dir = exe.parent().unwrap_or(Path::new(".")).to_path_buf();
    let portable = exe_dir.join("portable.flag").is_file();
    let explicit_root = env::var("FURINA_DESKTOP_ROOT").ok().map(PathBuf::from);
    let legacy_root = env::var("FURINA_AGENT_ROOT").ok().map(PathBuf::from);
    let debug_root = if cfg!(debug_assertions) {
        exe_dir.parent().and_then(Path::parent).map(Path::to_path_buf)
            .filter(|root| root.join("python/furina_tools").is_dir() && root.join("persona").is_dir())
    } else { None };
    let repo_root = if portable {
        None
    } else if cfg!(debug_assertions) {
        explicit_root.clone().or(debug_root)
    } else {
        explicit_root.clone().or_else(|| legacy_root.clone())
    };
    let (mode, resource_root, data_root, sidecar) = if let Some(root) = repo_root {
        (
            "development".to_string(),
            root.clone(),
            root.clone(),
            SidecarLaunch::Python {
                executable: app::find_python(),
                python_path: root.join("python"),
            },
        )
    } else {
        let bundled_resource_root = app_handle.path().resource_dir()?;
        let resource_root = select_resource_root(&exe_dir, &bundled_resource_root, portable);
        let data_root = select_data_root(
            &exe_dir,
            &app_handle.path().app_data_dir()?,
            portable,
        );
        let sidecar_path = select_sidecar_path(&exe_dir, &resource_root, portable);
        let sidecar = if sidecar_path.is_file() {
            SidecarLaunch::Executable(sidecar_path)
        } else {
            SidecarLaunch::Disabled(exe.clone())
        };
        (
            if portable { "portable" } else { "installed" }.to_string(),
            resource_root,
            data_root,
            sidecar,
        )
    };

    seed_data(&resource_root, &data_root)?;
    let preferences = load_preferences(&data_root);
    let workspace_root = env::var("FURINA_WORKSPACE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| (!preferences.workspace.trim().is_empty()).then(|| PathBuf::from(preferences.workspace)))
        .unwrap_or_else(|| {
            if mode == "development" {
                data_root.clone()
            } else {
                app_handle
                    .path()
                    .document_dir()
                    .map(|path| path.join("Furina Workspace"))
                    .unwrap_or_else(|_| data_root.join("workspace"))
            }
        });
    fs::create_dir_all(&workspace_root)?;

    let paths = RuntimePaths {
        resource_root: resource_root.clone(),
        data_root: data_root.clone(),
        workspace_root: workspace_root.clone(),
        sidecar: sidecar.clone(),
    };
    let info = RuntimeInfo {
        mode,
        resource_root: resource_root.display().to_string(),
        data_root: data_root.display().to_string(),
        workspace_root: workspace_root.display().to_string(),
        sidecar: sidecar.description(),
        legacy_root_env: explicit_root.is_none() && legacy_root.is_some(),
    };
    Ok(ResolvedRuntime { paths, info })
}

pub fn select_resource_root(exe_dir: &Path, bundled_resource_root: &Path, portable: bool) -> PathBuf {
    if portable { exe_dir.join("resources") } else { bundled_resource_root.to_path_buf() }
}

pub fn select_sidecar_path(exe_dir: &Path, resource_root: &Path, portable: bool) -> PathBuf {
    if portable { exe_dir.join("bin/furina-sidecar.exe") } else { resource_root.join("bin/furina-sidecar.exe") }
}

pub fn select_data_root(exe_dir: &Path, app_data_dir: &Path, portable: bool) -> PathBuf {
    if portable { exe_dir.join("data") } else { app_data_dir.to_path_buf() }
}

pub fn preferences_path(data_root: &Path) -> PathBuf {
    data_root.join(".furina/desktop.json")
}

pub fn load_preferences(data_root: &Path) -> DesktopPreferences {
    fs::read_to_string(preferences_path(data_root))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save_preferences(data_root: &Path, preferences: &DesktopPreferences) -> anyhow::Result<()> {
    let path = preferences_path(data_root);
    if let Some(parent) = path.parent() { fs::create_dir_all(parent)?; }
    atomic_write(&path, serde_json::to_vec_pretty(preferences)?.as_slice())
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = path.parent().ok_or_else(|| anyhow::anyhow!("路径缺少父目录"))?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".{}.tmp", path.file_name().unwrap_or_default().to_string_lossy()));
    fs::write(&temp, bytes)?;
    if path.exists() { fs::remove_file(path)?; }
    fs::rename(temp, path)?;
    Ok(())
}

fn seed_data(resource_root: &Path, data_root: &Path) -> anyhow::Result<()> {
    let furina_dir = data_root.join(".furina");
    fs::create_dir_all(furina_dir.join("memory"))?;
    fs::create_dir_all(furina_dir.join("avatar"))?;
    fs::create_dir_all(furina_dir.join("voice"))?;
    fs::create_dir_all(furina_dir.join("web_cache"))?;
    let config_path = furina_dir.join("config.yaml");
    if !config_path.exists() {
        let candidates = [
            resource_root.join("defaults/config.yaml"),
            resource_root.join(".furina/config.yaml"),
        ];
        if let Some(source) = candidates.iter().find(|path| path.is_file()) {
            fs::copy(source, &config_path)?;
        } else {
            atomic_write(&config_path, serde_yaml::to_string(&Config::default())?.as_bytes())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_app_data_for_installed_mode() {
        assert_eq!(
            select_data_root(Path::new("C:/Furina"), Path::new("C:/Users/test/AppData/Furina"), false),
            PathBuf::from("C:/Users/test/AppData/Furina")
        );
    }

    #[test]
    fn selects_adjacent_data_for_portable_mode() {
        assert_eq!(
            select_data_root(Path::new("D:/Portable/Furina"), Path::new("C:/ignored"), true),
            PathBuf::from("D:/Portable/Furina/data")
        );
    }

    #[test]
    fn selects_portable_resources_and_sidecar() {
        let exe_dir = Path::new("D:/Portable/Furina");
        let resource_root = select_resource_root(exe_dir, Path::new("C:/ignored/resources"), true);
        assert_eq!(resource_root, PathBuf::from("D:/Portable/Furina/resources"));
        assert_eq!(
            select_sidecar_path(exe_dir, &resource_root, true),
            PathBuf::from("D:/Portable/Furina/bin/furina-sidecar.exe")
        );
    }

    #[test]
    fn atomic_write_replaces_existing_content() {
        let root = std::env::temp_dir().join(format!("furina-runtime-{}", std::process::id()));
        let path = root.join("config.json");
        atomic_write(&path, b"old").unwrap();
        atomic_write(&path, b"new").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        let _ = fs::remove_dir_all(root);
    }
}
