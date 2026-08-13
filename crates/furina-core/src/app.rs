//! 应用装配层：仓库根目录定位、密钥注入、单实例锁、LLM/Agent 组装。
//!
//! CLI 与桌面版共用这套逻辑，保证两端的运行环境（secrets.env、单实例锁、
//! `.furina/` 状态目录、人格系统提示）完全一致。

use crate::agent::{Agent, Approver, PromptContextProvider};
use crate::config::Config;
use crate::interaction::HttpInteractionAnalyzer;
use crate::llm::{DeepSeekClient, LlmClient};
use crate::sidecar::{EventSink, Sidecar, SidecarLaunch};
use crate::web_cache::WebCache;
use furina_proto::Event;
use furina_soul::Soul;
use serde::Deserialize;
use std::env;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub struct RuntimePaths {
    pub resource_root: PathBuf,
    pub data_root: PathBuf,
    pub workspace_root: PathBuf,
    pub sidecar: SidecarLaunch,
}

impl RuntimePaths {
    pub fn config_path(&self) -> PathBuf { self.data_root.join(".furina/config.yaml") }
    pub fn secrets_path(&self) -> PathBuf { self.data_root.join(".furina/secrets.env") }
    pub fn soul_dir(&self) -> PathBuf { self.data_root.join(".furina/memory") }
    pub fn voice_dir(&self) -> PathBuf { self.data_root.join(".furina/voice") }
    pub fn web_cache_dir(&self) -> PathBuf { self.data_root.join(".furina/web_cache") }
    pub fn avatar_dir(&self) -> PathBuf { self.data_root.join(".furina/avatar") }
}

pub fn soul_dir(root: &Path) -> PathBuf {
    root.join(".furina/memory")
}

/// 写会话（chat/TUI/run/desktop）的单实例锁：防止多窗口并发覆盖灵魂状态。
#[derive(Debug)]
pub struct InstanceLock {
    _file: File,
}

impl InstanceLock {
    pub fn acquire(soul_dir: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(soul_dir)?;
        let path = soul_dir.join("instance.lock");
        let file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)?;
        use fs2::FileExt;
        file.try_lock_exclusive().map_err(|_| {
            anyhow::anyhow!(
                "检测到已有 Furina 实例在运行（锁文件 {}）。为避免记忆互相覆盖，请先退出其他窗口（chat/TUI/run/desktop）再启动。",
                path.display()
            )
        })?;
        Ok(Self { _file: file })
    }
}

/// 解析 .env 风格文件文本（KEY=VALUE，支持 # 注释与 export 前缀、可选的引号）。
pub fn parse_secrets_text(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim();
        let Some(eq) = line.find('=') else {
            continue;
        };
        let key = line[..eq].trim();
        let mut value = line[eq + 1..].trim().to_string();
        let quoted = value.len() >= 2
            && ((value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\'')));
        if quoted {
            value = value[1..value.len() - 1].to_string();
        } else if let Some(ci) = value.find(" #") {
            value.truncate(ci);
            value = value.trim().to_string();
        }
        if !key.is_empty() && !value.is_empty() {
            out.push((key.to_string(), value));
        }
    }
    out
}

/// 启动时读取 `.furina/secrets.env` 并注入环境（已存在的环境变量不覆盖）。
pub fn load_secrets_env(root: &Path) {
    load_secrets_env_with(root, false);
}

pub fn reload_secrets_env(root: &Path) {
    load_secrets_env_with(root, true);
}

fn load_secrets_env_with(root: &Path, overwrite: bool) {
    let path = root.join(".furina/secrets.env");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    for (key, value) in parse_secrets_text(&text) {
        if overwrite || env::var_os(&key).is_none() {
            let _ = env::set_var(key, value);
        }
    }
}

pub fn find_repo_root() -> Option<PathBuf> {
    if let Ok(root) = env::var("FURINA_DESKTOP_ROOT").or_else(|_| env::var("FURINA_AGENT_ROOT")) {
        let p = PathBuf::from(root);
        if is_repo_root(&p) {
            return Some(p);
        }
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(mut dir) = exe.parent().map(|p| p.to_path_buf()) {
            dir.pop(); // target/<profile> -> target
            dir.pop(); // target -> <root>
            if is_repo_root(&dir) {
                return Some(dir);
            }
        }
    }
    let mut dir = env::current_dir().ok()?;
    loop {
        if is_repo_root(&dir) {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn is_repo_root(p: &Path) -> bool {
    p.join("python").join("furina_tools").is_dir() && p.join("persona").is_dir()
}

pub fn find_python() -> String {
    env::var("FURINA_PYTHON").unwrap_or_else(|_| "python".to_string())
}


/// 按提供方配置构造 OpenAI 兼容客户端（key 从环境变量读取）。
pub fn build_interaction_analyzer(cfg: &Config) -> anyhow::Result<Option<HttpInteractionAnalyzer>> {
    let classifier = &cfg.emotion_classifier;
    if !classifier.enabled {
        return Ok(None);
    }
    let provider = cfg
        .provider(classifier.provider_id.trim())
        .ok_or_else(|| anyhow::anyhow!("未找到情绪分类器提供方: {}", classifier.provider_id))?;
    let key = env::var(&provider.api_key_env).map_err(|_| {
        anyhow::anyhow!("情绪分类器缺少环境变量 {}", provider.api_key_env)
    })?;
    HttpInteractionAnalyzer::new(provider, key, classifier).map(Some)
}

pub fn build_llm(cfg: &Config) -> anyhow::Result<Box<dyn LlmClient>> {
    let provider = cfg.active_provider()?;
    let key = env::var(&provider.api_key_env).map_err(|_| {
        anyhow::anyhow!(
            "缺少环境变量 {}（提供方 {}）。可在 .furina/secrets.env 中添加 {}=你的key",
            provider.api_key_env,
            provider.label,
            provider.api_key_env
        )
    })?;
    let client = DeepSeekClient::new(
        provider.base_url.clone(),
        key,
        provider.model.clone(),
        cfg.llm.temperature,
    )?;
    Ok(Box::new(client))
}

#[derive(Deserialize)]
struct PersonaMeta {
    #[serde(default)]
    persona_version: u32,
    #[serde(default)]
    dialogue_style: String,
}

/// 读取人格 yaml 的 dialogue_style 并注入 system_prompt.md 的 {persona_style}。
pub fn build_system_prompt(root: &Path, persona: &str) -> anyhow::Result<String> {
    let persona_path = root.join("persona").join(format!("{persona}.yaml"));
    let meta: PersonaMeta = serde_yaml::from_str(&std::fs::read_to_string(&persona_path)?)?;
    if meta.persona_version >= 2 && meta.dialogue_style.trim().is_empty() {
        anyhow::bail!("persona v3 缺少 dialogue_style：{}", persona_path.display());
    }
    let prompt = std::fs::read_to_string(root.join("persona/system_prompt.md"))?;
    Ok(prompt.replace("{persona_style}", &meta.dialogue_style))
}

/// 把灵魂引擎适配成核心的 PromptContextProvider（事件观察已由核心转发）。
pub struct SoulProvider(pub Arc<Mutex<Soul>>);

impl PromptContextProvider for SoulProvider {
    fn observe_user_text(&self, text: &str) {
        self.0.lock().unwrap().observe_text(text);
    }

    fn observe_event(&self, event: &Event) {
        self.0.lock().unwrap().observe_event(event);
    }

    fn observe_trigger_id(&self, trigger_id: &str) {
        self.0.lock().unwrap().observe_trigger_id(trigger_id);
    }

    fn context_block(&self) -> String {
        self.0.lock().unwrap().context_block()
    }

    fn context_block_for(&self, mode: &str) -> String {
        self.0.lock().unwrap().context_block_for(mode)
    }
}

/// 组装一个完整 Agent（LLM + 侧车 + 人格注入 + 网页缓存）。
pub async fn build_agent(
    paths: &RuntimePaths,
    persona: &str,
    soul: Arc<Mutex<Soul>>,
    sink: Arc<dyn EventSink>,
    approver: Box<dyn Approver>,
) -> anyhow::Result<Agent> {
    let cfg = Config::load(&paths.config_path())?;
    let system_prompt = build_system_prompt(&paths.resource_root, persona)?;
    let llm = build_llm(&cfg)?;
    let interaction_analyzer = build_interaction_analyzer(&cfg).ok().flatten();
    let interaction_analyzer_timeout_ms = cfg.emotion_classifier.timeout_ms;
    let sidecar = Sidecar::spawn(
        &paths.sidecar,
        &paths.workspace_root.display().to_string(),
        sink.clone(),
    )
    .await?;
    let mut agent = Agent::new(
        cfg,
        paths.workspace_root.clone(),
        sidecar,
        llm,
        sink,
        approver,
        system_prompt,
    );
    // Soul 私有边界：人格配置 / 记忆 / 密钥目录对 LLM 工具不可读写。
    agent.set_private_paths(vec![paths.resource_root.join("persona"), paths.data_root.join(".furina")]);
    agent.set_prompt_context(Box::new(SoulProvider(soul)));
    if let Some(interaction_analyzer) = interaction_analyzer {
        agent.set_interaction_analyzer(Box::new(interaction_analyzer), interaction_analyzer_timeout_ms);
    }
    agent.set_web_cache(WebCache::open(&paths.web_cache_dir()));
    Ok(agent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soul_dir_and_repo_root_detection() {
        let root = PathBuf::from("D:\\project\\Furina_Agent");
        assert_eq!(soul_dir(&root), root.join(".furina/memory"));
        assert!(
            find_repo_root().is_some(),
            "应从可执行文件位置推断出仓库根目录"
        );
    }

    #[test]
    fn secrets_parser_strips_inline_comments() {
        let pairs = parse_secrets_text(
            "# 注释\nKEY=abc # 行内\nQUOTED=\"a # b\"\nHASH=abc#def\nexport E=xyz\n",
        );
        let map: std::collections::HashMap<_, _> = pairs.into_iter().collect();
        assert_eq!(map.get("KEY").map(String::as_str), Some("abc"));
        assert_eq!(map.get("QUOTED").map(String::as_str), Some("a # b"));
        assert_eq!(map.get("HASH").map(String::as_str), Some("abc#def"));
        assert_eq!(map.get("E").map(String::as_str), Some("xyz"));
    }

    #[test]
    fn persona_v3_prompt_contains_reality_length_and_task_safety_rules() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let text = std::fs::read_to_string(root.join("persona/furina.yaml")).unwrap();
        let meta: PersonaMeta = serde_yaml::from_str(&text).unwrap();
        assert_eq!(meta.persona_version, 3);

        let prompt = build_system_prompt(&root, "furina").unwrap();
        assert!(prompt.contains("清楚当前身处现实世界"));
        assert!(prompt.contains("普通交流通常简洁"));
        assert!(prompt.contains("没有可靠输入时，我不会声称看见、触碰或闻到现实事物"));
        assert!(prompt.contains("技术任务必须事实准确、参数准确、结果可核验"));
        assert!(prompt.contains("删除、批量删除、递归删除、清空目录和破坏性覆盖始终禁止"));
        assert!(!prompt.contains("喜欢把对话当演出"));
    }
}
