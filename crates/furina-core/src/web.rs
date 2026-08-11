//! 联网搜索（需审批）：web.search / web.open。
//! 后端自备：Tavily / Bing / SearXNG；全部走 reqwest，Python 侧车零改动。

use crate::config::WebConfig;
use std::time::Duration;

pub const OPEN_MAX_CHARS: usize = 6_000;
const DDG_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36 FurinaAgent/0.1";

#[derive(Debug, Clone)]
pub struct WebResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

pub struct WebClient {
    backend: String,
    api_key: Option<String>,
    endpoint: String,
    max_results: usize,
    fallback_backend: Option<String>,
    fallback_endpoint: Option<String>,
    http: reqwest::Client,
}

/// 各后端的默认端点。
fn default_endpoint_for(backend: &str) -> anyhow::Result<String> {
    match backend {
        "tavily" => Ok("https://api.tavily.com/search".into()),
        "bing" => Ok("https://api.bing.microsoft.com/v7.0/search".into()),
        "binghtml" => Ok("https://www.bing.com/search".into()),
        "duckduckgo" => Ok("https://html.duckduckgo.com/html/".into()),
        "sogou" => Ok("https://www.sogou.com/web".into()),
        "searxng" => anyhow::bail!("SearXNG 需要配置 web.endpoint（你的实例地址，如 https://your-instance/search）"),
        other => anyhow::bail!("不支持的搜索后端: {other}"),
    }
}

impl WebClient {
    pub fn from_config(cfg: &WebConfig) -> anyhow::Result<Self> {
        let backend = cfg.search_backend.trim().to_lowercase();
        if backend.is_empty() || backend == "none" {
            anyhow::bail!("联网搜索未配置（config: web.search_backend = none）");
        }
        let api_key = if cfg.api_key_env.trim().is_empty() {
            None
        } else {
            let key = std::env::var(&cfg.api_key_env)
                .map_err(|_| anyhow::anyhow!("缺少环境变量 {}（搜索后端 {backend}）", cfg.api_key_env))?;
            Some(key)
        };
        let endpoint = if cfg.endpoint.trim().is_empty() {
            default_endpoint_for(&backend)?
        } else {
            cfg.endpoint.clone()
        };
        let fallback_backend = if cfg.fallback_backend.trim().is_empty() {
            None
        } else {
            let fb = cfg.fallback_backend.trim().to_lowercase();
            if fb != backend {
                // 提前校验回退后端可用（端点可解析）。
                if cfg.fallback_endpoint.trim().is_empty() {
                    let _ = default_endpoint_for(&fb)?;
                }
            }
            Some(fb)
        };
        let fallback_endpoint = if cfg.fallback_endpoint.trim().is_empty() {
            None
        } else {
            Some(cfg.fallback_endpoint.trim().to_string())
        };
        let http = crate::proxy::apply_system_proxy(
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(20)),
        )
        .build()?;
        Ok(Self {
            backend,
            api_key,
            endpoint,
            max_results: cfg.max_results.max(1).min(10),
            fallback_backend,
            fallback_endpoint,
            http,
        })
    }

    pub async fn search(&self, query: &str) -> anyhow::Result<Vec<WebResult>> {
        match self.search_one(&self.backend, &self.endpoint, query).await {
            Ok(r) => Ok(r),
            Err(primary_err) => {
                let Some(fb) = &self.fallback_backend else {
                    return Err(primary_err);
                };
                if fb == &self.backend {
                    return Err(primary_err);
                }
                let fep = match &self.fallback_endpoint {
                    Some(ep) => ep.clone(),
                    None => default_endpoint_for(fb)?,
                };
                match self.search_one(fb, &fep, query).await {
                    Ok(r) => Ok(r),
                    Err(fb_err) => Err(anyhow::anyhow!(
                        "主后端 {} 失败：{primary_err}；回退后端 {fb} 也失败：{fb_err}",
                        self.backend
                    )),
                }
            }
        }
    }

    async fn search_one(&self, backend: &str, endpoint: &str, query: &str) -> anyhow::Result<Vec<WebResult>> {
        let results = match backend {
            "tavily" => self.search_tavily(endpoint, query).await?,
            "bing" => self.search_bing(endpoint, query).await?,
            "binghtml" => self.search_bing_html(endpoint, query).await?,
            "duckduckgo" => self.search_duckduckgo(endpoint, query).await?,
            "sogou" => self.search_sogou(endpoint, query).await?,
            "searxng" => self.search_searxng(endpoint, query).await?,
            other => anyhow::bail!("不支持的搜索后端: {other}"),
        };
        Ok(results.into_iter().take(self.max_results).collect())
    }

    /// 打开网页并返回清洗后的纯文本（截断）。
    pub async fn open(&self, url: &str) -> anyhow::Result<String> {
        if !valid_web_url(url) {
            anyhow::bail!("仅支持 http/https 链接");
        }
        let resp = self.http.get(url).send().await?;
        if !resp.status().is_success() {
            anyhow::bail!("HTTP {}", resp.status());
        }
        let bytes = resp.bytes().await?;
        let text = String::from_utf8_lossy(&bytes);
        let title = extract_title(&text);
        let plain = strip_html(&text);
        let mut out = plain;
        if !title.is_empty() {
            out = format!("标题：{title}\n\n{out}");
        }
        Ok(out.chars().take(OPEN_MAX_CHARS).collect())
    }

    async fn search_tavily(&self, endpoint: &str, query: &str) -> anyhow::Result<Vec<WebResult>> {
        let key = self.api_key.clone().ok_or_else(|| anyhow::anyhow!("Tavily 需要 API key"))?;
        let body = serde_json::json!({
            "api_key": key,
            "query": query,
            "max_results": self.max_results,
        });
        let resp = self.http.post(endpoint).json(&body).send().await?;
        if !resp.status().is_success() {
            anyhow::bail!("Tavily HTTP {}", resp.status());
        }
        Ok(parse_tavily(&resp.text().await?))
    }

    async fn search_bing(&self, endpoint: &str, query: &str) -> anyhow::Result<Vec<WebResult>> {
        let key = self.api_key.clone().ok_or_else(|| anyhow::anyhow!("Bing 需要 API key"))?;
        let resp = self
            .http
            .get(endpoint)
            .query(&[("q", query), ("count", &self.max_results.to_string())])
            .header("Ocp-Apim-Subscription-Key", key)
            .send()
            .await?;
        if !resp.status().is_success() {
            anyhow::bail!("Bing HTTP {}", resp.status());
        }
        Ok(parse_bing(&resp.text().await?))
    }

    async fn search_duckduckgo(&self, endpoint: &str, query: &str) -> anyhow::Result<Vec<WebResult>> {
        let resp = self
            .http
            .get(endpoint)
            .query(&[("q", query)])
            .header("User-Agent", DDG_USER_AGENT)
            .send()
            .await?;
        if !resp.status().is_success() {
            anyhow::bail!("DuckDuckGo HTTP {}", resp.status());
        }
        Ok(parse_duckduckgo(&resp.text().await?))
    }

    /// Bing 网页搜索（免 key，国内直连可达）。
    async fn search_bing_html(&self, endpoint: &str, query: &str) -> anyhow::Result<Vec<WebResult>> {
        let resp = self
            .http
            .get(endpoint)
            .query(&[("q", query)])
            .header("User-Agent", DDG_USER_AGENT)
            .send()
            .await?;
        if !resp.status().is_success() {
            anyhow::bail!("Bing HTTP {}", resp.status());
        }
        Ok(parse_bing_html(&resp.text().await?))
    }

    async fn search_sogou(&self, endpoint: &str, query: &str) -> anyhow::Result<Vec<WebResult>> {
        let resp = self
            .http
            .get(endpoint)
            .query(&[("query", query)])
            .header("User-Agent", DDG_USER_AGENT)
            .send()
            .await?;
        if !resp.status().is_success() {
            anyhow::bail!("Sogou HTTP {}", resp.status());
        }
        Ok(parse_sogou(&resp.text().await?))
    }

    async fn search_searxng(&self, endpoint: &str, query: &str) -> anyhow::Result<Vec<WebResult>> {
        let resp = self
            .http
            .get(endpoint)
            .query(&[("q", query), ("format", "json")])
            .send()
            .await?;
        if !resp.status().is_success() {
            anyhow::bail!("SearXNG HTTP {}", resp.status());
        }
        Ok(parse_searxng(&resp.text().await?))
    }
}

/// 仅允许 http/https 链接。
pub fn valid_web_url(s: &str) -> bool {
    let t = s.trim();
    (t.starts_with("http://") || t.starts_with("https://")) && !t.contains(char::is_control)
}

pub fn parse_tavily(json: &str) -> Vec<WebResult> {
    parse_results(json, &["results"])
}

pub fn parse_bing(json: &str) -> Vec<WebResult> {
    parse_results(json, &["webPages", "value"])
}

pub fn parse_searxng(json: &str) -> Vec<WebResult> {
    parse_results(json, &["results"])
}

/// 解析 DuckDuckGo HTML 结果页（免 key 后端，非官方接口）。
pub fn parse_duckduckgo(html: &str) -> Vec<WebResult> {
    const MAX: usize = 30;
    let mut out = Vec::new();
    let mut rest = html;
    while out.len() < MAX {
        let Some(rel) = rest.find("class=\"result__a\"") else {
            break;
        };
        let block_start = rest[..rel].rfind('<').unwrap_or(rel);
        let tail = &rest[block_start..];
        let Some(href_pos) = tail.find("href=\"") else {
            break;
        };
        let href_start = href_pos + 6;
        let Some(href_len) = tail[href_start..].find('"') else {
            break;
        };
        let href = &tail[href_start..href_start + href_len];
        let Some(title_gt) = tail.find('>') else {
            break;
        };
        let Some(title_end) = tail[title_gt + 1..].find("</a>") else {
            break;
        };
        let title = decode_html(&strip_html(&tail[title_gt + 1..title_gt + 1 + title_end]));
        let url = ddg_real_url(href);
        let snippet = tail
            .find("class=\"result__snippet\"")
            .and_then(|sp| {
                let st = &tail[sp..];
                let gt = st.find('>')?;
                let end = st[gt + 1..].find("</a>")?;
                Some(decode_html(&strip_html(&st[gt + 1..gt + 1 + end])))
            })
            .unwrap_or_default();
        out.push(WebResult {
            title,
            url,
            snippet: snippet.chars().take(300).collect(),
        });
        rest = &tail[title_gt + 1 + title_end + 4..];
    }
    out
}

/// 解析 Bing 搜索结果 HTML（免 key 后端，`b_algo` 结果块）。
pub fn parse_bing_html(html: &str) -> Vec<WebResult> {
    const MAX: usize = 30;
    let mut out = Vec::new();
    let mut rest = html;
    while out.len() < MAX {
        let Some(alg) = rest.find("class=\"b_algo\"") else {
            break;
        };
        let block_start = rest[..alg].rfind('<').unwrap_or(alg);
        let tail = &rest[block_start..];
        let Some(h2) = tail.find("<h2") else {
            break;
        };
        let h2_tail = &tail[h2..];
        let Some(href_pos) = h2_tail.find("href=\"") else {
            break;
        };
        let href_start = href_pos + 6;
        let Some(href_len) = h2_tail[href_start..].find('"') else {
            break;
        };
        let href = &h2_tail[href_start..href_start + href_len];
        let Some(gt) = h2_tail.find('>') else {
            break;
        };
        let Some(close) = h2_tail[gt + 1..].find("</a>") else {
            break;
        };
        let title = decode_html(&strip_html(&h2_tail[gt + 1..gt + 1 + close]));
        let snippet = find_bing_snippet(tail);
        out.push(WebResult {
            title,
            url: bing_real_url(href),
            snippet: snippet.chars().take(300).collect(),
        });
        rest = &h2_tail[gt + 1 + close + 4..];
    }
    out
}

fn find_bing_snippet(block: &str) -> String {
    let Some(p) = block.find("<p") else {
        return String::new();
    };
    let pt = &block[p..];
    let Some(gt) = pt.find('>') else {
        return String::new();
    };
    let Some(end) = pt[gt + 1..].find("</p>") else {
        return String::new();
    };
    decode_html(&strip_html(&pt[gt + 1..gt + 1 + end]))
}

/// 还原 Bing `/ck/a` 重定向里的真实 URL（base64url 解码 `u` 参数）。
fn bing_real_url(href: &str) -> String {
    if href.contains("bing.com/ck/a") {
        if let Some(u) = href.split("u=").nth(1) {
            let u = u.split('&').next().unwrap_or("");
            use base64::Engine;
            if let Ok(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(u) {
                if let Ok(s) = String::from_utf8(bytes) {
                    return s;
                }
            }
        }
    }
    href.to_string()
}

/// 解析搜狗搜索结果 HTML（免 key，国内直连可达）。
pub fn parse_sogou(html: &str) -> Vec<WebResult> {
    const MAX: usize = 30;
    let mut out = Vec::new();
    let mut rest = html;
    while out.len() < MAX {
        let Some(h3) = rest.find("<h3") else {
            break;
        };
        let block_start = rest[..h3].rfind('<').unwrap_or(h3);
        let tail = &rest[block_start..];
        let Some(href_pos) = tail.find("href=\"") else {
            break;
        };
        let href_start = href_pos + 6;
        let Some(href_len) = tail[href_start..].find('"') else {
            break;
        };
        let href = decode_html(&tail[href_start..href_start + href_len]);
        let Some(gt) = tail.find('>') else {
            break;
        };
        let Some(close) = tail[gt + 1..].find("</a>") else {
            break;
        };
        let title = decode_html(&strip_html(&tail[gt + 1..gt + 1 + close]));
        let after = gt + 1 + close + 4;
        if title.trim().is_empty() {
            rest = &tail[after.min(tail.len())..];
            continue;
        }
        let url = if href.starts_with("/link?") {
            format!("https://www.sogou.com{href}")
        } else {
            href
        };
        let snippet = find_sogou_snippet(tail);
        out.push(WebResult {
            title,
            url,
            snippet: snippet.chars().take(300).collect(),
        });
        rest = &tail[after.min(tail.len())..];
    }
    out
}

fn find_sogou_snippet(block: &str) -> String {
    let window: String = block.chars().take(2500).collect();
    for marker in ["star-wiki", "fz-mid", "space-txt"] {
        if let Some(m) = window.find(marker) {
            let t = &window[m..];
            if let Some(gt) = t.find('>') {
                let content = &t[gt + 1..];
                if let Some(end) = content.find("</") {
                    let s = strip_html(&content[..end]).trim().to_string();
                    if !s.is_empty() {
                        return s;
                    }
                }
            }
        }
    }
    String::new()
}

/// 把 DuckDuckGo 的重定向链接还原为真实 URL（%XX 解码）。
fn ddg_real_url(href: &str) -> String {
    if href.starts_with("//duckduckgo.com/l/?uddg=") {
        if let Some(v) = href.split("uddg=").nth(1) {
            let v = v.split('&').next().unwrap_or("");
            return percent_decode(v);
        }
    }
    href.to_string()
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex_val(b[i + 1]), hex_val(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// 简单 HTML 实体解码（&amp; 等）。
fn decode_html(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

/// 提取 `<title>` 内容（清洗后，截断 200 字符）。
pub fn extract_title(html: &str) -> String {
    let lower = html.to_lowercase();
    let Some(start) = lower.find("<title>") else {
        return String::new();
    };
    let content = &html[start + 7..];
    let Some(end) = content.find("</title>") else {
        return String::new();
    };
    decode_html(&strip_html(&content[..end])).chars().take(200).collect()
}

fn parse_results(json: &str, path: &[&str]) -> Vec<WebResult> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let mut cur = v;
    for key in path {
        cur = match cur.get(key) {
            Some(x) => x.clone(),
            None => return Vec::new(),
        };
    }
    let Some(arr) = cur.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|item| {
            let title = item
                .get("title")
                .or_else(|| item.get("name"))
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let url = item.get("url").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let snippet = item
                .get("content")
                .or_else(|| item.get("snippet"))
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            if title.is_empty() && url.is_empty() {
                return None;
            }
            Some(WebResult {
                title,
                url,
                snippet: snippet.chars().take(300).collect(),
            })
        })
        .collect()
}

/// 轻量 HTML 清洗：移除 script/style 与所有标签，折叠空白（无第三方依赖）。
pub fn strip_html(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    loop {
        let Some(lt) = rest.find('<') else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..lt]);
        let tail = &rest[lt..];
        let lower = tail.to_lowercase();
        let mut skip_len: Option<usize> = None;
        for tag in ["script", "style", "nav", "aside"] {
            if lower.starts_with(&format!("<{tag}")) {
                let after_open = lower.find('>').map(|p| p + 1).unwrap_or(0);
                let content = &tail[after_open.min(tail.len())..];
                let close_len = tag.len() + 3; // "</tag>"
                skip_len = Some(after_open + match content.to_lowercase().find("</").map(|p| p + close_len) {
                    Some(end) => end.min(content.len()),
                    None => content.len(),
                });
                break;
            }
        }
        if let Some(n) = skip_len {
            rest = &tail[n.min(tail.len())..];
        } else if let Some(p) = tail.find('>') {
            rest = &tail[p + 1..];
        } else {
            break;
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_whitelist() {
        assert!(valid_web_url("https://example.com/a?q=1"));
        assert!(valid_web_url("http://example.com"));
        assert!(!valid_web_url("file:///C:/secret"));
        assert!(!valid_web_url("javascript:alert(1)"));
        assert!(!valid_web_url("ftp://example.com"));
    }

    #[test]
    fn parse_tavily_json() {
        let json = r#"{"results":[{"title":"T","url":"https://t","content":"snippet"}]}"#;
        let r = parse_tavily(json);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].title, "T");
        assert_eq!(r[0].snippet, "snippet");
    }

    #[test]
    fn parse_bing_json() {
        let json = r#"{"webPages":{"value":[{"name":"N","url":"https://n","snippet":"s"}]}}"#;
        let r = parse_bing(json);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].title, "N");
    }

    #[test]
    fn parse_searxng_json() {
        let json = r#"{"results":[{"title":"S","url":"https://s","content":"c"}]}"#;
        let r = parse_searxng(json);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].url, "https://s");
    }

    #[test]
    fn parse_bad_json_empty() {
        assert!(parse_tavily("not json").is_empty());
    }

    #[test]
    fn strip_html_removes_scripts_tags_and_collapses() {
        let html = "<html><head><style>.x{}</style></head><body><p>你好 <b>世界</b></p><script>alert(1)</script>end</body></html>";
        let plain = strip_html(html);
        assert!(!plain.contains("<"));
        assert!(!plain.contains("alert"));
        assert!(plain.contains("你好 世界"));
        assert!(plain.contains("end"));
    }

    #[test]
    fn strip_html_no_tag_untouched() {
        assert_eq!(strip_html("plain text"), "plain text");
    }

    #[test]
    fn parse_duckduckgo_html() {
        let html = r#"
<div class="result results_links results_links_deep web-result">
  <h2 class="result__title">
    <a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpage%3Fa%3D1%26b%3D2">Example &amp; Co</a>
  </h2>
  <a class="result__snippet" href="//duckduckgo.com/l/?uddg=...">这是 <b>摘要</b> 文本</a>
</div>
<div class="result">
  <a rel="nofollow" class="result__a" href="https://plain.example.com/x">Plain Link</a>
</div>
"#;
        let r = parse_duckduckgo(html);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].title, "Example & Co", "实体应解码");
        assert_eq!(r[0].url, "https://example.com/page?a=1&b=2", "uddg 重定向应还原");
        assert!(r[0].snippet.contains("摘要"), "snippet: {}", r[0].snippet);
        assert_eq!(r[1].url, "https://plain.example.com/x");
    }

    #[test]
    fn from_config_duckduckgo_needs_no_key() {
        let cfg = WebConfig {
            search_backend: "duckduckgo".into(),
            api_key_env: String::new(),
            endpoint: String::new(),
            max_results: 5,
            fallback_backend: String::new(),
            fallback_endpoint: String::new(),
        };
        let c = WebClient::from_config(&cfg).unwrap();
        assert!(c.api_key.is_none());
        assert_eq!(c.endpoint, "https://html.duckduckgo.com/html/");
    }

    #[test]
    fn parse_bing_html_results() {
        let html = r#"
<li class="b_algo">
  <h2><a href="https://example.com/page">Example Title</a></h2>
  <div class="b_caption"><p>这是摘要 &amp; 内容</p></div>
</li>
<li class="b_algo">
  <h2><a href="https://www.bing.com/ck/a?u=aHR0cHM6Ly9yLmV4YW1wbGUuY29tLw">Redirected</a></h2>
</li>
"#;
        let r = parse_bing_html(html);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].title, "Example Title");
        assert_eq!(r[0].url, "https://example.com/page");
        assert!(r[0].snippet.contains("摘要"), "snippet: {}", r[0].snippet);
        assert_eq!(r[1].url, "https://r.example.com/", "ck/a 应解码为真实 URL: {}", r[1].url);
    }

    #[test]
    fn parse_sogou_results() {
        let html = r#"
<div class="vrwrap">
  <h3 class="vr-title"><a href="https://rust-lang.org">Rust 官网</a></h3>
  <div class="text-layout"><p class="star-wiki">系统编程语言 &amp; 内存安全</p></div>
</div>
<div class="vrwrap">
  <h3 class="vr-title"><a href="/link?url=abc">跳转结果</a></h3>
</div>
"#;
        let r = parse_sogou(html);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].title, "Rust 官网");
        assert_eq!(r[0].url, "https://rust-lang.org");
        assert!(r[0].snippet.contains("内存安全"), "snippet: {}", r[0].snippet);
        assert_eq!(r[1].url, "https://www.sogou.com/link?url=abc", "相对跳转应补全: {}", r[1].url);
    }

    fn spawn_mock_n(status: u16, body: String, n: usize) -> String {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = listener.local_addr().unwrap();
                tx.send(addr.to_string()).unwrap();
                for _ in 0..n {
                    let (mut sock, _) = listener.accept().await.unwrap();
                    let mut buf = [0u8; 8192];
                    use tokio::io::AsyncReadExt;
                    let _ = sock.read(&mut buf).await;
                    let reason = if status == 200 { "OK" } else { "Error" };
                    let resp = format!(
                        "HTTP/1.1 {status} {reason}\r\ncontent-type: text/html\r\ncontent-length: {}\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    use tokio::io::AsyncWriteExt;
                    let _ = sock.write_all(resp.as_bytes()).await;
                }
            });
        });
        rx.recv().unwrap()
    }

    #[tokio::test]
    async fn search_falls_back_when_primary_fails() {
        let primary = spawn_mock_n(500, "boom".into(), 1);
        let sogou_html = r#"<div class="vrwrap"><h3><a href="https://fallback.example">Fallback Result</a></h3></div>"#.to_string();
        let fallback = spawn_mock_n(200, sogou_html, 1);
        let cfg = WebConfig {
            search_backend: "tavily".into(),
            api_key_env: "FURINA_WEB_FB_KEY".into(),
            endpoint: format!("http://{primary}"),
            max_results: 5,
            fallback_backend: "sogou".into(),
            fallback_endpoint: format!("http://{fallback}"),
        };
        std::env::set_var("FURINA_WEB_FB_KEY", "k");
        let client = WebClient::from_config(&cfg).unwrap();
        let r = client.search("rust").await.unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].title, "Fallback Result");
        std::env::remove_var("FURINA_WEB_FB_KEY");
    }

    #[test]
    fn extract_title_finds_title() {
        let html = "<html><head><title> 标题 &amp; 副标题 </title></head><body>x</body></html>";
        assert_eq!(extract_title(html), "标题 & 副标题");
        assert_eq!(extract_title("<body>no title</body>"), "");
    }

    #[test]
    fn strip_html_skips_nav_and_aside() {
        let html = "<nav>导航链接</nav><p>正文内容</p><aside>侧栏</aside>end";
        let plain = strip_html(html);
        assert!(plain.contains("正文内容"));
        assert!(!plain.contains("导航链接"));
        assert!(!plain.contains("侧栏"));
    }
}
