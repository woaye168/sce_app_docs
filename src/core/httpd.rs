//! 内嵌静态 HTTP 服务（tiny_http）：serve vitepress build 产物。
//! 端口由 exe 自己持有——停止 = 关 listener，关 app = 端口随进程消失，机制上无孤儿。
//!
//! - Host 白名单：开关打开时，非 IP/localhost 的域名必须在名单内，否则 403（对齐 vite allowedHosts 语义）
//! - 缓存策略：/assets/* 是 hash 命名产物 → immutable 一年；其余 no-cache
//! - 路径映射："/" → index.html；目录 → index.html；无扩展名 → 尝试 .html（cleanUrls: false 约定）
//! - 服务根是 Arc<RwLock<PathBuf>>：构建完成后原子翻转，重建期间旧内容不掉线

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// 内嵌 HTTP 服务句柄（drop 即停）
pub struct Httpd {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    pub port: u16,
}

impl Drop for Httpd {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// 启动服务。绑定失败（端口被占）立即报错——不调换端口，不静默漂移。
/// 用 std TcpListener 先 bind（Windows 默认独占），再交给 tiny_http——
/// 直接用 tiny_http::Server::http 会带 SO_REUSEADDR 语义，端口被占也能挤进去（劫持）。
/// lan 模式绑 0.0.0.0 前先探测 127.0.0.1：Windows 下 wildcard bind 与具体地址 bind 可共存
/// （具体地址优先抢流量），不探测会出现「以为在服务其实流量去别家」的漂移。
pub fn start(
    root: Arc<RwLock<PathBuf>>,
    port: u16,
    lan: bool,
    allowed_hosts: Vec<String>,
) -> Result<Httpd, String> {
    let addr = if lan { format!("0.0.0.0:{port}") } else { format!("127.0.0.1:{port}") };
    if lan {
        // 探测具体地址是否已被占（wildcard bind 查不出这种冲突）
        if let Err(e) = std::net::TcpListener::bind(("127.0.0.1", port)) {
            return Err(format!("端口 {port} 被占用或绑定失败: {e}"));
        }
    }
    let listener = std::net::TcpListener::bind(&addr)
        .map_err(|e| format!("端口 {port} 被占用或绑定失败: {e}"))?;
    let server = tiny_http::Server::from_listener(listener, None)
        .map_err(|e| format!("端口 {port} 服务初始化失败: {e}"))?;
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let handle = std::thread::spawn(move || loop {
        if stop2.load(Ordering::Relaxed) {
            return;
        }
        let Ok(Some(req)) = server.recv_timeout(Duration::from_millis(200)) else {
            continue;
        };
        handle_request(req, &root, &allowed_hosts);
    });
    Ok(Httpd { stop, handle: Some(handle), port })
}

/// Host 准入：白名单为空 → 全放行；非空 → localhost/IP 直连放行，域名必须在名单
fn host_allowed(host_header: Option<&str>, allowed: &[String]) -> bool {
    if allowed.is_empty() {
        return true;
    }
    let Some(h) = host_header else { return true }; // HTTP/1.0 无 Host 头，放行
    // 剥端口：IPv6 是 [::1]:port 格式，先取括号内；IPv4/域名按 : 切
    let host = if h.starts_with('[') {
        h[1..].split(']').next().unwrap_or(h)
    } else {
        h.split(':').next().unwrap_or(h)
    }.trim();
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    if host.parse::<IpAddr>().is_ok() {
        return true;
    }
    allowed.iter().any(|a| a.eq_ignore_ascii_case(host))
}

fn handle_request(req: tiny_http::Request, root: &Arc<RwLock<PathBuf>>, allowed: &[String]) {
    // Host 白名单校验
    let host = req.headers().iter().find(|h| h.field.equiv("Host")).map(|h| h.value.to_string());
    if !host_allowed(host.as_deref(), allowed) {
        let shown = host.as_deref().unwrap_or("");
        let _ = req.respond(tiny_http::Response::from_string(
            format!("Blocked request. This host (\"{shown}\") is not allowed."),
        ).with_status_code(403));
        return;
    }
    let url = req.url().split('?').next().unwrap_or("/").to_string();
    let Some(rel) = resolve_path(&url) else {
        let _ = req.respond(tiny_http::Response::from_string("403 Forbidden").with_status_code(403));
        return;
    };
    let base = root.read().map(|r| r.clone()).unwrap_or_default();
    let mut file = base.join(&rel);
    // 无扩展名回退 .html（sidebar 链接不带扩展名，cleanUrls: false 下硬刷新要能命中）
    if !file.is_file() && !rel.contains('.') {
        let alt = base.join(format!("{rel}.html"));
        if alt.is_file() {
            file = alt;
        }
    }
    if !file.is_file() {
        let _ = req.respond(tiny_http::Response::from_string("404 Not Found").with_status_code(404));
        return;
    }
    let data = match std::fs::read(&file) {
        Ok(d) => d,
        Err(_) => {
            let _ = req.respond(tiny_http::Response::from_string("404 Not Found").with_status_code(404));
            return;
        }
    };
    let cache = if rel.starts_with("assets/") {
        "max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let mime = mime_of(&rel);
    let resp = tiny_http::Response::from_data(data)
        .with_header(tiny_http::Header::from_bytes("Content-Type", mime).unwrap())
        .with_header(tiny_http::Header::from_bytes("Cache-Control", cache).unwrap());
    let _ = req.respond(resp);
}

/// URL → 相对路径（防穿越；百分号解码支持中文文件名）
/// "/" → "index.html"；"/a/" → "a/index.html"；"/a/b" → "a/b" 或回退 "a/b.html"
fn resolve_path(url: &str) -> Option<String> {
    let decoded = percent_decode(url);
    let trimmed = decoded.trim_start_matches('/');
    if trimmed.split('/').any(|seg| seg == "..") {
        return None;
    }
    if trimmed.is_empty() {
        return Some("index.html".into());
    }
    if trimmed.ends_with('/') {
        return Some(format!("{trimmed}index.html"));
    }
    Some(trimmed.to_string())
}

/// 简易百分号解码（%XX → byte，按 UTF-8 处理；非法序列原样保留）
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

fn mime_of(rel: &str) -> &'static str {
    let ext = rel.rsplit('.').next().unwrap_or("");
    match ext {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "txt" | "md" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hosts(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn host_rules() {
        // 空白名单全放行
        assert!(host_allowed(Some("evil.com"), &[]));
        assert!(host_allowed(None, &[]));
        // 开白名单：localhost/IP 放行，名单内放行，其余拦截
        let wl = hosts(&["docs.example.com"]);
        assert!(host_allowed(Some("localhost"), &wl));
        assert!(host_allowed(Some("localhost:18753"), &wl));
        assert!(host_allowed(Some("127.0.0.1:18753"), &wl));
        assert!(host_allowed(Some("192.168.31.10:18753"), &wl));
        assert!(host_allowed(Some("[::1]:18753"), &wl));
        assert!(host_allowed(Some("docs.example.com"), &wl));
        assert!(!host_allowed(Some("evil.com"), &wl));
        assert!(!host_allowed(Some("sub.docs.example.com"), &wl)); // 不含子域
    }

    #[test]
    fn path_resolve() {
        assert_eq!(resolve_path("/"), Some("index.html".into()));
        assert_eq!(resolve_path("/api/"), Some("api/index.html".into()));
        assert_eq!(resolve_path("/api/client/rpc"), Some("api/client/rpc".into()));
        assert_eq!(resolve_path("/assets/app.abc123.js"), Some("assets/app.abc123.js".into()));
        assert_eq!(resolve_path("/%E4%B8%AD%E6%96%87/"), Some("中文/index.html".into()));
        assert_eq!(resolve_path("/../etc/passwd"), None);
        assert_eq!(resolve_path("/a/../../b"), None);
    }

    #[test]
    fn mime_basic() {
        assert!(mime_of("a/b.html").starts_with("text/html"));
        assert_eq!(mime_of("assets/x.js"), "application/javascript; charset=utf-8");
        assert_eq!(mime_of("f.ico"), "image/x-icon");
    }

    #[test]
    fn port_conflict_errors_not_drifts() {
        // 占住端口，start 必须报错而不是换端口
        let hold = std::net::TcpListener::bind(("127.0.0.1", 18399)).unwrap();
        let root = Arc::new(RwLock::new(PathBuf::new()));
        let r = start(root, 18399, false, vec![]);
        assert!(r.is_err());
        let msg = r.err().unwrap();
        assert!(msg.contains("被占用"), "报错应说明占用：{msg}");
        drop(hold);
    }
}
