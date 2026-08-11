//! 网页缓存（Web Intelligence Phase 2）：把读过的网页与搜索结果缓存在本地
//! JSONL（`<root>/.furina/web_cache/cache.jsonl`），供 `/网页` 检索与后续
//! Web Query Layer 使用。零第三方依赖，延续灵魂记忆的存储风格。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 单条缓存：网页正文（截断）或搜索结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebCacheEntry {
    pub url: String,
    pub title: String,
    pub snippet: String,
    pub content: String,
    pub fetched_at_ms: u128,
}

/// 缓存上限（条数），超出裁剪最旧的。
pub const CACHE_LIMIT: usize = 500;

pub struct WebCache {
    dir: PathBuf,
}

impl WebCache {
    pub fn open(dir: &Path) -> Self {
        Self { dir: dir.to_path_buf() }
    }

    fn path(&self) -> PathBuf {
        self.dir.join("cache.jsonl")
    }

    fn load(&self) -> Vec<WebCacheEntry> {
        let Ok(text) = std::fs::read_to_string(self.path()) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for line in text.lines() {
            if let Ok(e) = serde_json::from_str::<WebCacheEntry>(line) {
                out.push(e);
            }
        }
        out
    }

    /// 写入一条缓存（同 URL 更新，超上限裁剪最旧）。
    pub fn put(&self, entry: WebCacheEntry) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let mut entries = self.load();
        entries.retain(|e| e.url != entry.url);
        entries.push(entry);
        entries.sort_by_key(|e| std::cmp::Reverse(e.fetched_at_ms));
        entries.truncate(CACHE_LIMIT);
        let mut out = String::new();
        for e in &entries {
            out.push_str(&serde_json::to_string(e)?);
            out.push('\n');
        }
        std::fs::write(self.path(), out)?;
        Ok(())
    }

    pub fn get(&self, url: &str) -> Option<WebCacheEntry> {
        self.load().into_iter().find(|e| e.url == url)
    }

    /// 最近 k 条（按抓取时间倒序）。
    pub fn recent(&self, k: usize) -> Vec<WebCacheEntry> {
        let mut v = self.load();
        v.sort_by_key(|e| std::cmp::Reverse(e.fetched_at_ms));
        v.into_iter().take(k).collect()
    }

    /// 简单关键词检索（url/title/snippet/content 包含匹配，Phase 3 再上索引）。
    pub fn search(&self, query: &str) -> Vec<WebCacheEntry> {
        let q = query.to_lowercase();
        let mut v: Vec<_> = self
            .load()
            .into_iter()
            .filter(|e| {
                e.url.to_lowercase().contains(&q)
                    || e.title.to_lowercase().contains(&q)
                    || e.snippet.to_lowercase().contains(&q)
                    || e.content.to_lowercase().contains(&q)
            })
            .collect();
        v.sort_by_key(|e| std::cmp::Reverse(e.fetched_at_ms));
        v
    }

    pub fn count(&self) -> usize {
        self.load().len()
    }

    /// 清理超过保留天数未更新的条目，返回删除条数。
    pub fn clean(&self, now_ms: u128, retention_days: u64) -> usize {
        let cutoff = now_ms.saturating_sub(retention_days.max(1) as u128 * 86_400_000);
        let before_len = self.count();
        let kept: Vec<_> = self
            .load()
            .into_iter()
            .filter(|e| e.fetched_at_ms >= cutoff)
            .collect();
        if kept.len() == before_len {
            return 0;
        }
        let mut out = String::new();
        for e in &kept {
            out.push_str(&serde_json::to_string(e).unwrap_or_default());
            out.push('\n');
        }
        let _ = std::fs::write(self.path(), out);
        before_len - kept.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("furina_webcache_{}_{}", std::process::id(), tag))
    }

    fn entry(url: &str, title: &str, ts: u128) -> WebCacheEntry {
        WebCacheEntry {
            url: url.into(),
            title: title.into(),
            snippet: String::new(),
            content: format!("内容 {title}"),
            fetched_at_ms: ts,
        }
    }

    #[test]
    fn put_get_search_recent() {
        let dir = tmp_dir("a");
        let c = WebCache::open(&dir);
        c.put(entry("https://a.example", "Rust 官网", 100)).unwrap();
        c.put(entry("https://b.example", "Python 教程", 200)).unwrap();
        assert_eq!(c.count(), 2);
        assert_eq!(c.get("https://a.example").unwrap().title, "Rust 官网");
        let hits = c.search("rust");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].url, "https://a.example");
        let recent = c.recent(1);
        assert_eq!(recent[0].url, "https://b.example");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn put_same_url_updates() {
        let dir = tmp_dir("b");
        let c = WebCache::open(&dir);
        c.put(entry("https://a.example", "旧标题", 100)).unwrap();
        c.put(entry("https://a.example", "新标题", 200)).unwrap();
        assert_eq!(c.count(), 1, "同 URL 应更新而非新增");
        assert_eq!(c.get("https://a.example").unwrap().title, "新标题");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_trims_oldest() {
        let dir = tmp_dir("c");
        let c = WebCache::open(&dir);
        for i in 0..CACHE_LIMIT + 20 {
            c.put(entry(&format!("https://x{i}.example"), "t", i as u128)).unwrap();
        }
        assert_eq!(c.count(), CACHE_LIMIT);
        assert!(c.get("https://x0.example").is_none(), "最旧应被裁剪");
        assert!(
            c.get(&format!("https://x{}.example", CACHE_LIMIT + 19)).is_some(),
            "最新条目应保留"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clean_removes_expired_only() {
        let dir = tmp_dir("clean");
        let c = WebCache::open(&dir);
        let now = 1_000_000_000_000u128;
        c.put(entry("https://old.example", "旧", now - 4 * 86_400_000)).unwrap();
        c.put(entry("https://recent.example", "新", now - 86_400_000)).unwrap();
        assert_eq!(c.clean(now, 3), 1, "应清理超过 3 天的条目");
        assert!(c.get("https://old.example").is_none());
        assert!(c.get("https://recent.example").is_some());
        assert_eq!(c.clean(now, 3), 0, "再次清理应无事可做");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clean_empty_cache_safe() {
        let dir = tmp_dir("cleane");
        let c = WebCache::open(&dir);
        assert_eq!(c.clean(1_000_000_000_000, 3), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
