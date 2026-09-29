//! MCP 端点（Streamable HTTP，完整实现）：JSON-RPC + session 管理 + SSE 流式响应。
//! 工具定义/执行与 /_api/ask **同一份实现**（ask::tools_defs / ask::exec_tool）——改一处两边生效。
//! 暴露工具：search_docs / get_doc / list_sources（只读检索，安全边界见设计 §6.2）。
//!
//! 协议要点（2025-03-26 Streamable HTTP）：
//! - POST /_mcp 携带 JSON-RPC；initialize 响应头带 Mcp-Session-Id，后续请求必须携带
//! - 客户端 Accept 含 text/event-stream 时用 SSE 帧回响应，否则直接 JSON
//! - GET /_mcp（服务端主动推送流）本服务不支持 → 405（协议允许的合规行为）
//! - notifications/*（无 id）→ 202 无 body

use crate::core::ask;
use crate::core::httpd::ApiState;
use crate::core::kb;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 会话管理（TTL 1 小时，惰性 GC）
pub struct SessionMgr {
    sessions: HashMap<String, Instant>,
    ttl: Duration,
    counter: u64,
}

impl SessionMgr {
    pub fn new() -> Self {
        Self { sessions: HashMap::new(), ttl: Duration::from_secs(3600), counter: 0 }
    }
    /// 创建会话，返回 session id
    pub fn create(&mut self) -> String {
        self.gc();
        self.counter += 1;
        let id = format!("bgd{}-{}-{:x}", self.counter, std::process::id(), now_secs());
        self.sessions.insert(id.clone(), Instant::now());
        id
    }
    pub fn validate(&mut self, id: &str) -> bool {
        self.gc();
        self.sessions.contains_key(id)
    }
    fn gc(&mut self) {
        let ttl = self.ttl;
        self.sessions.retain(|_, t| t.elapsed() < ttl);
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn rpc_ok(id: &serde_json::Value, result: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn rpc_err(id: &serde_json::Value, code: i64, message: &str) -> serde_json::Value {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// ask 的 OpenAI tools 格式 → MCP tools/list 格式
fn mcp_tools() -> Vec<serde_json::Value> {
    ask::tools_defs()
        .into_iter()
        .map(|t| {
            let f = &t["function"];
            serde_json::json!({
                "name": f["name"],
                "description": f["description"],
                "inputSchema": f["parameters"],
            })
        })
        .collect()
}

/// 处理一条 JSON-RPC 消息。
/// 返回 (需设置的 session 头, 响应 JSON)；通知类返回 (None, None)。
/// session 校验失败返回错误响应。
pub fn handle_message(
    api: &ApiState,
    sessions: &Mutex<SessionMgr>,
    session_header: Option<&str>,
    msg: &serde_json::Value,
) -> (Option<String>, Option<serde_json::Value>) {
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let id = msg.get("id").cloned().unwrap_or(serde_json::Value::Null);
    let is_notification = msg.get("id").is_none();

    // initialize 免 session；其余必须带有效 session（完整实现：session 管理不降级）
    if method != "initialize" {
        let ok = session_header.map(|h| sessions.lock().map(|mut m| m.validate(h)).unwrap_or(false)).unwrap_or(false);
        if !ok {
            if is_notification {
                return (None, None);
            }
            return (None, Some(rpc_err(&id, -32000, "无效或过期的 Mcp-Session-Id（请先 initialize）")));
        }
    }

    match method {
        "initialize" => {
            let sid = sessions.lock().map(|mut m| m.create()).unwrap_or_default();
            let result = serde_json::json!({
                "protocolVersion": "2025-03-26",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "sce_app_docs", "version": env!("CARGO_PKG_VERSION") }
            });
            (Some(sid), Some(rpc_ok(&id, result)))
        }
        "notifications/initialized" | "notifications/cancelled" => (None, None),
        "ping" => (None, Some(rpc_ok(&id, serde_json::json!({})))),
        "tools/list" => (None, Some(rpc_ok(&id, serde_json::json!({"tools": mcp_tools()})))),
        "tools/call" => {
            let params = msg.get("params").cloned().unwrap_or(serde_json::json!({}));
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params.get("arguments").map(|a| a.to_string()).unwrap_or_else(|| "{}".into());
            let mut sources: Vec<kb::SearchHit> = Vec::new();
            match ask::exec_tool(&api.search, name, &args, api.use_rerank, &mut sources) {
                Ok(text) => (
                    None,
                    Some(rpc_ok(&id, serde_json::json!({
                        "content": [{ "type": "text", "text": text }]
                    }))),
                ),
                Err(e) => (None, Some(rpc_err(&id, -32603, &e))),
            }
        }
        _ => {
            if is_notification {
                (None, None)
            } else {
                (None, Some(rpc_err(&id, -32601, &format!("方法不存在: {method}"))))
            }
        }
    }
}

/// 响应封装：Accept 含 text/event-stream → SSE 帧；否则纯 JSON
pub fn wrap_response(body: &serde_json::Value, sse: bool) -> (String, &'static str) {
    if sse {
        (format!("event: message\ndata: {}\n\n", body), "text/event-stream")
    } else {
        (body.to_string(), "application/json; charset=utf-8")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::indexer::{SearchCtx, SharedIndexStatus};
    use crate::core::kb::{ChunkRecord, Kb};
    use crate::core::indexer;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn setup(tag: &str) -> (ApiState, PathBuf, PathBuf) {
        let home = std::env::temp_dir().join(format!("bgd_mcp_home_{tag}_{}", std::process::id()));
        let site = std::env::temp_dir().join(format!("bgd_mcp_site_{tag}_{}", std::process::id()));
        std::fs::create_dir_all(&site).unwrap();
        let mut kb = Kb::open(&indexer::kb_path(&home, &site)).unwrap();
        kb.upsert_file("x.md", "h", &[ChunkRecord {
            file: "x.md".into(), heading: "H".into(), text: "内容".into(), url: "/x".into(), embedding: vec![1.0],
        }]).unwrap();
        let api = ApiState {
            status: SharedIndexStatus::default(),
            search: Mutex::new(SearchCtx::new(home.clone(), site.clone())),
            use_rerank: false,
            llm: crate::core::llm::LlmConfig { base_url: String::new(), api_key: String::new(), model: String::new() },
            max_rounds: 5,
            mcp_sessions: Mutex::new(SessionMgr::new()),
        };
        (api, home, site)
    }

    #[test]
    fn full_session_flow() {
        let (api, home, site) = setup("flow");
        let sessions = Mutex::new(SessionMgr::new());
        // 1. 无 session 的 tools/list 被拒
        let (_, resp) = handle_message(&api, &sessions, None, &serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}));
        assert!(resp.unwrap().get("error").is_some(), "无 session 应报错");
        // 2. initialize → 发 session
        let (sid, resp) = handle_message(&api, &sessions, None, &serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}));
        let sid = sid.expect("initialize 应发 session");
        let r = resp.unwrap();
        assert_eq!(r["result"]["serverInfo"]["name"], "sce_app_docs");
        // 3. 通知类无响应
        let (_, resp) = handle_message(&api, &sessions, Some(&sid), &serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        assert!(resp.is_none());
        // 4. tools/list 带 session 正常
        let (_, resp) = handle_message(&api, &sessions, Some(&sid), &serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
        let tools = resp.unwrap()["result"]["tools"].as_array().unwrap().clone();
        assert_eq!(tools.len(), 3);
        assert!(tools.iter().any(|t| t["name"] == "search_docs"));
        // 5. tools/call list_sources
        let (_, resp) = handle_message(&api, &sessions, Some(&sid), &serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_sources","arguments":{}}}));
        let text = resp.unwrap()["result"]["content"][0]["text"].as_str().unwrap().to_string();
        assert!(text.contains("x.md"), "{text}");
        // 6. 假 session 被拒
        let (_, resp) = handle_message(&api, &sessions, Some("fake"), &serde_json::json!({"jsonrpc":"2.0","id":4,"method":"ping"}));
        assert!(resp.unwrap().get("error").is_some());
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&site);
    }

    #[test]
    fn sse_wrap_format() {
        let body = serde_json::json!({"jsonrpc":"2.0","id":1,"result":{}});
        let (frame, ct) = wrap_response(&body, true);
        assert_eq!(ct, "text/event-stream");
        assert!(frame.starts_with("event: message\ndata: "));
        assert!(frame.ends_with("\n\n"));
        let (_, ct2) = wrap_response(&body, false);
        assert!(ct2.starts_with("application/json"));
    }
}
