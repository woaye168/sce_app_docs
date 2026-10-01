//! 内嵌静态 HTTP 服务（tiny_http）：serve vitepress build 产物。
//! 端口由 exe 自己持有——停止 = 关 listener，关 app = 端口随进程消失，机制上无孤儿。
//!
//! - Host 白名单：开关打开时，非 IP/localhost 的域名必须在名单内，否则 403（对齐 vite allowedHosts 语义）
//! - 缓存策略：/assets/* 是 hash 命名产物 → immutable 一年；其余 no-cache
//! - 路径映射："/" → index.html；目录 → index.html；无扩展名 → 尝试 .html（cleanUrls: false 约定）
//! - 服务根是 Arc<RwLock<PathBuf>>：构建完成后原子翻转，重建期间旧内容不掉线

use crate::core::indexer::{self, SearchCtx, SharedIndexStatus};
use crate::core::{ask, llm, mcp};
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

/// API 共享状态（/_api/* 路由用）
pub struct ApiState {
    /// 索引状态（/_api/index_status）
    pub status: SharedIndexStatus,
    /// 检索上下文（模型懒加载；Mutex 串行化，文档站 QPS 低无瓶颈）
    pub search: Mutex<SearchCtx>,
    /// rerank 开关
    pub use_rerank: bool,
    /// LLM 配置（/_api/ask；空 = 未配置）
    pub llm: llm::LlmConfig,
    /// 最大工具调用轮数
    pub max_rounds: usize,
    /// MCP 会话管理（/_mcp）
    pub mcp_sessions: Mutex<mcp::SessionMgr>,
}

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
    api: Option<Arc<ApiState>>,
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
        handle_request(req, &root, &allowed_hosts, &api);
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

fn handle_request(req: tiny_http::Request, root: &Arc<RwLock<PathBuf>>, allowed: &[String], api: &Option<Arc<ApiState>>) {
    // Host 白名单校验
    let host = req.headers().iter().find(|h| h.field.equiv("Host")).map(|h| h.value.to_string());
    if !host_allowed(host.as_deref(), allowed) {
        let shown = host.as_deref().unwrap_or("");
        let _ = req.respond(tiny_http::Response::from_string(
            format!("Blocked request. This host (\"{shown}\") is not allowed."),
        ).with_status_code(403));
        return;
    }
    let full_url = req.url().to_string();
    let url = req.url().split('?').next().unwrap_or("/").to_string();
    // MCP 端点（/_mcp；同样避开文档路由）
    if url == "/_mcp" {
        route_mcp(req, api);
        return;
    }
    // API 路由（/_api/ 前缀：站点文档源占用了 /api/，必须避开）
    if url.starts_with("/_api/") {
        route_api(req, &url, &full_url, api);
        return;
    }
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

/// JSON 响应助手
fn respond_json(req: tiny_http::Request, status: u16, body: String) {
    let resp = tiny_http::Response::from_string(body)
        .with_status_code(status)
        .with_header(tiny_http::Header::from_bytes("Content-Type", "application/json; charset=utf-8").unwrap());
    let _ = req.respond(resp);
}

/// API 路由：/_api/index_status、/_api/search?q=&k=
fn route_api(req: tiny_http::Request, path: &str, full_url: &str, api: &Option<Arc<ApiState>>) {
    let Some(api) = api else {
        respond_json(req, 503, r#"{"error":"API 未启用"}"#.into());
        return;
    };
    match path {
        "/_api/index_status" => {
            let s = api.status.read().map(|s| serde_json::to_string(&*s).unwrap_or_default()).unwrap_or_default();
            respond_json(req, 200, s);
        }
        "/_api/llm_status" => {
            let configured = !api.llm.base_url.is_empty() && !api.llm.api_key.is_empty() && !api.llm.model.is_empty();
            respond_json(req, 200, serde_json::json!({"configured": configured, "model": api.llm.model}).to_string());
        }
        "/_api/models" => {
            // 代理中转站 /models（设置页下拉 + 聊天面板模型切换共用）
            match crate::core::llm::fetch_models(&api.llm) {
                Ok(models) => respond_json(req, 200, serde_json::json!({"models": models}).to_string()),
                Err(e) => respond_json(req, 400, serde_json::json!({"error": e}).to_string()),
            }
        }
        "/_api/search" => {
            let q = query_param(full_url, "q").unwrap_or_default();
            let k: usize = query_param(full_url, "k").and_then(|v| v.parse().ok()).unwrap_or(5);
            if q.is_empty() {
                respond_json(req, 400, r#"{"error":"缺少 q 参数"}"#.into());
                return;
            }
            let mut ctx = match api.search.lock() {
                Ok(c) => c,
                Err(_) => {
                    respond_json(req, 500, r#"{"error":"检索上下文锁失败"}"#.into());
                    return;
                }
            };
            match indexer::search_with_kb(&mut ctx, &q, k, api.use_rerank) {
                Ok(hits) => respond_json(req, 200, serde_json::to_string(&hits).unwrap_or_default()),
                Err(e) => respond_json(req, 500, serde_json::to_string(&serde_json::json!({"error": e})).unwrap_or_default()),
            }
        }
        "/_api/ask" => route_ask(req, api),
        _ => respond_json(req, 404, r#"{"error":"未知 API"}"#.into()),
    }
}

/// /_mcp 路由（Streamable HTTP）：POST JSON-RPC；GET 服务端推送流不支持 → 405（协议合规）
fn route_mcp(mut req: tiny_http::Request, api: &Option<Arc<ApiState>>) {
    use std::io::Read;
    let Some(api) = api else {
        respond_json(req, 503, r#"{"error":"API 未启用"}"#.into());
        return;
    };
    if req.method().as_str() == "GET" {
        let _ = req.respond(tiny_http::Response::empty(405));
        return;
    }
    let session = req.headers().iter().find(|h| h.field.equiv("Mcp-Session-Id")).map(|h| h.value.to_string());
    let want_sse = req
        .headers()
        .iter()
        .find(|h| h.field.equiv("Accept"))
        .map(|h| h.value.as_str().contains("text/event-stream"))
        .unwrap_or(false);
    let mut body = String::new();
    if req.as_reader().read_to_string(&mut body).is_err() {
        respond_json(req, 400, r#"{"error":"读 body 失败"}"#.into());
        return;
    }
    let msg: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => {
            respond_json(req, 400, r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"JSON 解析失败"}}"#.into());
            return;
        }
    };
    tracing::info!("mcp: {} (session={})", msg.get("method").and_then(|m| m.as_str()).unwrap_or("?"), session.as_deref().unwrap_or("-"));
    let (new_session, resp) = mcp::handle_message(api, &api.mcp_sessions, session.as_deref(), &msg);
    let Some(resp) = resp else {
        // 通知类：202 无 body
        let _ = req.respond(tiny_http::Response::empty(202));
        return;
    };
    let (text, content_type) = mcp::wrap_response(&resp, want_sse);
    let mut response = tiny_http::Response::from_string(text)
        .with_status_code(200)
        .with_header(tiny_http::Header::from_bytes("Content-Type", content_type).unwrap());
    if let Some(sid) = new_session {
        response = response.with_header(tiny_http::Header::from_bytes("Mcp-Session-Id", sid.as_str()).unwrap());
    }
    let _ = req.respond(response);
}

/// AskEvent → SSE 帧 JSON
fn ask_event_json(ev: &ask::AskEvent) -> String {
    let v = match ev {
        ask::AskEvent::Think(t) => serde_json::json!({"type": "think", "text": t}),
        ask::AskEvent::Delta(d) => serde_json::json!({"type": "delta", "text": d}),
        ask::AskEvent::ToolStart { name, args } => serde_json::json!({"type": "tool_start", "name": name, "args": args}),
        ask::AskEvent::ToolCall { name, args, summary } => {
            serde_json::json!({"type": "tool_call", "name": name, "args": args, "summary": summary})
        }
        ask::AskEvent::Sources(hits) => serde_json::json!({"type": "sources", "hits": hits.iter().map(|h| {
            // url 拼章节锚点（#标题 slug），出处链接直达小节
            serde_json::json!({"file": h.file, "heading": h.heading, "text": h.text, "url": h.url_with_anchor(), "score": h.score})
        }).collect::<Vec<_>>()}),
        ask::AskEvent::Done => serde_json::json!({"type": "done"}),
    };
    v.to_string()
}

/// /_api/ask：POST {question, history} → SSE 流式回答
fn route_ask(mut req: tiny_http::Request, api: &Arc<ApiState>) {
    use std::io::Read;
    // 读 POST body
    let mut body = String::new();
    if let Err(e) = req.as_reader().read_to_string(&mut body) {
        respond_json(req, 400, serde_json::json!({"error": format!("读 body 失败: {e}")}).to_string());
        return;
    }
    let payload: serde_json::Value = serde_json::from_str(&body).unwrap_or(serde_json::json!({}));
    let question = payload.get("question").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let history = payload.get("history").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    // 用户当前阅读的文档路径（前端 useData relativePath，与 kb 路径一致；「解释当前文档」的上下文）
    let page = payload.get("page").and_then(|v| v.as_str()).unwrap_or("").to_string();
    // 聊天面板可临时切换模型（不发则全局配置；不落盘，设置页管默认值）
    let model_override = payload.get("model").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if question.is_empty() {
        respond_json(req, 400, r#"{"error":"缺少 question"}"#.into());
        return;
    }
    if api.llm.base_url.is_empty() || api.llm.api_key.is_empty() || (api.llm.model.is_empty() && model_override.is_empty()) {
        respond_json(req, 400, r#"{"error":"LLM 未配置（AI 设置页填 base_url/api_key/model）"}"#.into());
        return;
    }
    // 聊天面板临时模型覆盖全局配置（不落盘）
    let mut llm = api.llm.clone();
    if !model_override.is_empty() {
        llm.model = model_override;
    }
    // 拿走裸 writer 手写 SSE（CGI 式）：tiny_http 的 Response chunked 输出不 flush 会合批
    //（浏览器实测 6s 攒一个包——「卡住感」根因），必须逐帧 write + flush
    let mut w = req.into_writer();
    let api2 = api.clone();
    std::thread::spawn(move || {
        use std::io::Write;
        tracing::info!("ask 开始: {} | page: {} | model: {}", question.chars().take(80).collect::<String>(), page, llm.model);
        if w.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n").is_err() {
            return;
        }
        let mut send = |w: &mut Box<dyn Write + Send>, v: String| -> bool {
            w.write_all(format!("data: {v}\n\n").as_bytes()).is_ok() && w.flush().is_ok()
        };
        if !send(&mut w, r#"{"type":"start"}"#.into()) {
            return;
        }
        let r = {
            let mut wref = &mut w;
            ask::ask(&api2.search, &llm, &question, history, api2.max_rounds, api2.use_rerank, &page, |ev| {
                send(&mut wref, ask_event_json(&ev));
            })
        };
        match r {
            Ok(()) => tracing::info!("ask 完成"),
            Err(e) => {
                tracing::info!("ask 失败: {e}");
                send(&mut w, serde_json::json!({"type": "error", "text": e}).to_string());
            }
        }
        // w 随线程结束 drop → 连接关闭（Connection: close）
    });
}

/// 解析 URL query 参数（?a=b&c=d）
fn query_param(url: &str, key: &str) -> Option<String> {
    let qs = url.split('?').nth(1)?;
    for pair in qs.split('&') {
        let mut it = pair.splitn(2, '=');
        if it.next() == Some(key) {
            return it.next().map(|v| percent_decode(v));
        }
    }
    None
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
        let r = start(root, 18399, false, vec![], None);
        assert!(r.is_err());
        let msg = r.err().unwrap();
        assert!(msg.contains("被占用"), "报错应说明占用：{msg}");
        drop(hold);
    }
}
