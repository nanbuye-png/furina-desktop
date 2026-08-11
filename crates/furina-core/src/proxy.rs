//! 系统代理发现：环境变量优先，其次 Windows 系统代理（注册表）。
//! 让 LLM / 联网搜索 / 视觉等 reqwest 客户端在国内代理环境下也能联网。

/// 返回应使用的 HTTP 代理地址（`http://host:port`），无代理时返回 None。
pub fn system_proxy() -> Option<String> {
    for var in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"] {
        if let Ok(v) = std::env::var(var) {
            let v = v.trim().to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    windows_system_proxy()
}

#[cfg(windows)]
fn windows_system_proxy() -> Option<String> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let settings = hkcu
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings")
        .ok()?;
    let enable: u32 = settings.get_value("ProxyEnable").unwrap_or(0);
    if enable == 0 {
        return None;
    }
    let server: String = settings.get_value("ProxyServer").ok()?;
    let server = normalize_proxy_server(server.trim());
    if server.is_empty() {
        None
    } else {
        Some(format!("http://{server}"))
    }
}

#[cfg(not(windows))]
fn windows_system_proxy() -> Option<String> {
    None
}

/// 处理形如 `host:port`、`http=host:port;https=host2:port2` 的代理服务器字符串。
fn normalize_proxy_server(s: &str) -> String {
    if s.contains('=') {
        for part in s.split(';') {
            if let Some((k, v)) = part.split_once('=') {
                if k.trim().eq_ignore_ascii_case("https") {
                    return v.trim().to_string();
                }
            }
        }
        for part in s.split(';') {
            if let Some((k, v)) = part.split_once('=') {
                if k.trim().eq_ignore_ascii_case("http") {
                    return v.trim().to_string();
                }
            }
        }
        s.split('=').next().map(|v| v.trim().to_string()).unwrap_or_default()
    } else {
        s.to_string()
    }
}

fn no_proxy_matches(host: &str) -> bool {
    let host = host.trim_matches(['[', ']']).to_ascii_lowercase();
    if host == "127.0.0.1" || host == "localhost" || host == "::1" {
        return true;
    }
    let Ok(value) = std::env::var("NO_PROXY").or_else(|_| std::env::var("no_proxy")) else {
        return false;
    };
    value.split(',').map(str::trim).filter(|item| !item.is_empty()).any(|item| {
        if item == "*" {
            return true;
        }
        let item = item.trim_start_matches('.').to_ascii_lowercase();
        let item = item.split(':').next().unwrap_or(&item);
        host == item || host.ends_with(&format!(".{item}"))
    })
}

/// 在 reqwest builder 上应用系统代理（如有）。
pub fn apply_system_proxy(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    match system_proxy() {
        Some(proxy) => {
            let proxy_url = proxy.clone();
            builder.proxy(reqwest::Proxy::custom(move |url| {
                let host = url.host_str().unwrap_or_default();
                if no_proxy_matches(host) {
                    None
                } else {
                    Some(proxy_url.clone())
                }
            }))
        }
        None => builder,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_plain_host() {
        assert_eq!(normalize_proxy_server("127.0.0.1:7897"), "127.0.0.1:7897");
    }

    #[test]
    fn normalize_multi_protocol() {
        let s = "http=127.0.0.1:7890;https=127.0.0.1:7891";
        assert_eq!(normalize_proxy_server(s), "127.0.0.1:7891");
    }

    #[test]
    fn env_proxy_takes_priority() {
        std::env::set_var("FURINA_TEST_PROXY", "http://127.0.0.1:9");
        // system_proxy 只读标准代理变量；这里验证标准变量读取
        std::env::set_var("HTTPS_PROXY", "http://127.0.0.1:9");
        assert_eq!(system_proxy().as_deref(), Some("http://127.0.0.1:9"));
        std::env::remove_var("HTTPS_PROXY");
        std::env::remove_var("FURINA_TEST_PROXY");
    }
}
