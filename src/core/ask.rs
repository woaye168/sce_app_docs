//! 多轮 Tool Calling 问答管线（agentic RAG）：LLM 当司机，自主检索/深读，够了才回答。
//! 工具定义与执行和 MCP 端点**同一份实现**（exec_tool / tools_defs 两边共用）。
//! 纯管线逻辑，事件经回调吐出——HTTP SSE 层（httpd）只是把它转写成 SSE 帧。

use crate::core::indexer::SearchCtx;
use crate::core::{indexer, kb};
use crate::core::llm::{self, LlmConfig, StreamEvent};
use std::sync::Mutex;

/// 问答事件（前端 SSE / 调试共用）
#[derive(Debug, Clone)]
pub enum AskEvent {
    /// 思考链增量
    Think(String),
    /// 正文增量
    Delta(String),
    /// 工具调用开始（name/args；前端立即展示「正在查…」，Codex 式过程可见）
    ToolStart { name: String, args: String },
    /// 工具调用（name/args + 结果摘要，前端展示「正在查…」）
    ToolCall { name: String, args: String, summary: String },
    /// 出处（全部检索命中去重后）
    Sources(Vec<kb::SearchHit>),
    /// 完成
    Done,
}

/// 工具定义（OpenAI tools 格式；MCP tools/list 由它转换）
pub fn tools_defs() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "search_docs",
                "description": "语义检索项目文档库（API 文档/设计文档/研究笔记），返回相关文本块及出处",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "自然语言查询" },
                        "top_k": { "type": "number", "description": "返回条数（默认 5，1-10）" }
                    },
                    "required": ["query"]
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "get_doc",
                "description": "按文件路径取文档全文（search_docs 命中后深读用）",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "文档路径（search_docs 结果里的 file 字段）" }
                    },
                    "required": ["path"]
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "list_sources",
                "description": "列出文档库中所有已索引的文件",
                "parameters": { "type": "object", "properties": {} }
            }
        }),
    ]
}

/// 执行工具（只读文档库；无 shell/无写文件/无任意路径——安全边界见设计 §6.2）
/// ctx 是共享锁：**只在工具执行瞬间持有**（LLM 网络等待期间不持锁，索引同步/其他检索不被卡）
/// 返回给 LLM 的 JSON 字符串；命中块同时累积到 sources（出处链接用）
pub fn exec_tool(
    ctx: &Mutex<SearchCtx>,
    name: &str,
    args: &str,
    use_rerank: bool,
    sources: &mut Vec<kb::SearchHit>,
) -> Result<String, String> {
    let args_v: serde_json::Value = serde_json::from_str(args).unwrap_or(serde_json::json!({}));
    match name {
        "search_docs" => {
            let q = args_v.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let k = args_v.get("top_k").and_then(|v| v.as_u64()).unwrap_or(5).clamp(1, 10) as usize;
            let mut guard = ctx.lock().map_err(|_| "检索上下文锁失败".to_string())?;
            let hits = indexer::search_with_kb(&mut guard, q, k, use_rerank)?;
            drop(guard);
            sources.extend(hits.iter().cloned());
            // 去重（同文件同标题只留一个）
            let mut seen = std::collections::HashSet::new();
            sources.retain(|h| seen.insert((h.file.clone(), h.heading.clone())));
            // url 拼章节锚点（MCP 调用方/人可直接跳转小节）
            let hits_json: Vec<serde_json::Value> = hits.iter().map(|h| {
                serde_json::json!({"file": h.file, "heading": h.heading, "text": h.text, "url": h.url_with_anchor(), "score": h.score})
            }).collect();
            serde_json::to_string(&hits_json).map_err(|e| e.to_string())
        }
        "get_doc" => {
            let path = args_v.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let guard = ctx.lock().map_err(|_| "检索上下文锁失败".to_string())?;
            match indexer::get_doc(&guard.home, &guard.site, path) {
                Some(text) => Ok(serde_json::json!({"file": path, "text": text}).to_string()),
                None => Ok(serde_json::json!({"error": "文档未索引", "file": path}).to_string()),
            }
        }
        "list_sources" => {
            let guard = ctx.lock().map_err(|_| "检索上下文锁失败".to_string())?;
            let files = indexer::list_indexed(&guard.home, &guard.site);
            Ok(serde_json::json!({"files": files, "count": files.len()}).to_string())
        }
        _ => Ok(serde_json::json!({"error": format!("未知工具 {name}")}).to_string()),
    }
}

const SYSTEM_PROMPT: &str = "你是项目文档助手。回答用户关于本项目的问题。\n\
规则：\n\
1. 优先用 search_docs 检索文档库，不要凭印象编造；必要时用 get_doc 深读全文。\n\
2. 检索结果不足可以换关键词再查（允许调用多轮工具）。\n\
3. 回答用中文，基于检索到的内容。**不要在回答末尾列参考文档——参考由系统自动附带，你列了就是重复**。\n\
4. 文档库里没有的内容，明确说「文档库未覆盖」。\n\
输出格式（前端会实时渲染，务必遵守）：\n\
- 需要画图（流程/架构/时序/关系说明）时，一律用 ```mermaid 代码块输出 mermaid 源码，不要画 ASCII 图；\n\
  mermaid 源码里禁止写硬编码颜色（fill/stroke 色值），要强调就交给主题配色，否则暗色主题下会看不清；\n\
  节点文字含括号/特殊字符时必须用引号包住（如 A[\"co.promise() 创建\"]），否则解析报错；\n\
  连线带文字一律用实线管道语法 A -->|文字| B（最稳）；虚线箭头带文字写 A -. 文字 .-> B，\n\
  禁止把文字塞在箭头中间（如 <-.- \"文字\" .-> 是非法语法，会 Parse error）。\n\
- 给出代码时，一律用带语言标记的代码块（如 ```lua、```rust、```sql），不要用行内代码写多行代码。";

/// 组装 system prompt：page 非空时追加「当前文档」上下文（前端传用户正在阅读的页面相对路径，
/// 与 kb 文件路径一致，LLM 可用 get_doc 直接读全文）
fn build_system_prompt(page: &str) -> String {
    if page.is_empty() {
        return SYSTEM_PROMPT.to_string();
    }
    format!("{SYSTEM_PROMPT}\n5. 用户当前正在阅读文档：{page}。用户说「当前文档/这篇文档/本文」时指这篇，优先用 get_doc 读取其全文。")
}

/// 多轮问答主循环。ctx 为共享检索上下文（锁只在工具执行瞬间持有）。
/// history 为 OpenAI 消息格式的会话历史（user/assistant 交替）。
/// max_rounds = 最大工具调用轮数（界面可配）。page = 用户当前阅读的文档路径（可为空）。
pub fn ask(
    ctx: &Mutex<SearchCtx>,
    cfg: &LlmConfig,
    question: &str,
    history: Vec<serde_json::Value>,
    max_rounds: usize,
    use_rerank: bool,
    page: &str,
    mut on_event: impl FnMut(AskEvent),
) -> Result<(), String> {
    let mut messages = vec![serde_json::json!({"role": "system", "content": build_system_prompt(page)})];
    messages.extend(history);
    messages.push(serde_json::json!({"role": "user", "content": question}));

    let tools = tools_defs();
    let mut sources: Vec<kb::SearchHit> = Vec::new();

    for round in 0..=max_rounds {
        let completion = llm::chat_stream(cfg, messages.clone(), &tools, |ev| {
            match ev {
                StreamEvent::Think(t) => on_event(AskEvent::Think(t)),
                StreamEvent::Delta(d) => on_event(AskEvent::Delta(d)),
            }
        })?;

        if completion.finish == "tool_calls" && !completion.tool_calls.is_empty() && round < max_rounds {
            //  assistant 消息（带 tool_calls）+ 每个工具结果
            let tc_json: Vec<serde_json::Value> = completion.tool_calls.iter().map(|tc| {
                serde_json::json!({
                    "id": tc.id,
                    "type": "function",
                    "function": { "name": tc.name, "arguments": tc.args }
                })
            }).collect();
            messages.push(serde_json::json!({
                "role": "assistant",
                "content": completion.text,
                "tool_calls": tc_json
            }));
            for tc in &completion.tool_calls {
                on_event(AskEvent::ToolStart { name: tc.name.clone(), args: tc.args.clone() });
                let result = exec_tool(ctx, &tc.name, &tc.args, use_rerank, &mut sources)?;
                let summary = summarize_tool_result(&tc.name, &result);
                on_event(AskEvent::ToolCall { name: tc.name.clone(), args: tc.args.clone(), summary });
                messages.push(serde_json::json!({
                    "role": "tool",
                    "tool_call_id": tc.id,
                    "content": result
                }));
            }
            continue;
        }
        // 最终回答完成
        if !sources.is_empty() {
            on_event(AskEvent::Sources(sources));
        }
        on_event(AskEvent::Done);
        return Ok(());
    }
    Err(format!("超过最大工具调用轮数（{max_rounds}）"))
}

/// 工具结果摘要（前端展示用，截断防刷屏）
fn summarize_tool_result(name: &str, result: &str) -> String {
    let n: usize = result.chars().count();
    let preview: String = result.chars().take(80).collect();
    format!("{name} 返回 {n} 字符：{preview}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::kb::{ChunkRecord, Kb};

    fn setup_kb(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let home = std::env::temp_dir().join(format!("bgd_ask_home_{tag}_{}", std::process::id()));
        let site = std::env::temp_dir().join(format!("bgd_ask_site_{tag}_{}", std::process::id()));
        std::fs::create_dir_all(&site).unwrap();
        let mut kb = Kb::open(&indexer::kb_path(&home, &site)).unwrap();
        kb.upsert_file("a/b.md", "h1", &[
            ChunkRecord { file: "a/b.md".into(), heading: "接口".into(), text: "查询接口文档内容".into(), url: "/a/b".into(), embedding: vec![1.0] },
        ]).unwrap();
        (home, site)
    }

    /// system prompt 必须引导输出格式：画图用 mermaid（前端会渲染）、代码用带语言的 fence（前端高亮）、
    /// 正文不列参考（参考由系统附带）、mermaid 禁硬编码颜色（主题自适应）
    #[test]
    fn system_prompt_guides_output_format() {
        assert!(SYSTEM_PROMPT.contains("mermaid"), "应引导 mermaid 画图：{SYSTEM_PROMPT}");
        assert!(SYSTEM_PROMPT.contains("```"), "应引导代码 fence：{SYSTEM_PROMPT}");
        assert!(SYSTEM_PROMPT.contains("参考由系统"), "应禁止正文列参考：{SYSTEM_PROMPT}");
        assert!(SYSTEM_PROMPT.contains("硬编码颜色"), "应禁止 mermaid 硬编码颜色：{SYSTEM_PROMPT}");
        assert!(SYSTEM_PROMPT.contains("-->|"), "应给连线标签的安全写法示例：{SYSTEM_PROMPT}");
    }

    /// 当前文档上下文：传了页面路径 → system prompt 带上并点名「当前文档」；空 → 原 prompt
    #[test]
    fn system_prompt_includes_current_page() {
        let p = build_system_prompt("research/messege/持久化方案认知对齐.md");
        assert!(p.contains("research/messege/持久化方案认知对齐.md"), "应带页面路径：{p}");
        assert!(p.contains("当前文档"), "应点名「当前文档」指代：{p}");
        assert_eq!(build_system_prompt(""), SYSTEM_PROMPT, "无上下文应原样");
    }

    #[test]
    fn exec_get_doc_and_list_sources() {
        let (home, site) = setup_kb("tools");
        let ctx = Mutex::new(SearchCtx::new(home.clone(), site.clone()));
        let mut sources = Vec::new();
        // get_doc 命中
        let r = exec_tool(&ctx, "get_doc", r#"{"path":"a/b.md"}"#, false, &mut sources).unwrap();
        assert!(r.contains("查询接口文档内容"));
        // get_doc 未索引路径（穿越尝试也只能落在索引内）
        let r = exec_tool(&ctx, "get_doc", r#"{"path":"../../etc/passwd"}"#, false, &mut sources).unwrap();
        assert!(r.contains("文档未索引"));
        // list_sources
        let r = exec_tool(&ctx, "list_sources", "{}", false, &mut sources).unwrap();
        assert!(r.contains("a/b.md"));
        // 未知工具不炸
        let r = exec_tool(&ctx, "delete_everything", "{}", false, &mut sources).unwrap();
        assert!(r.contains("未知工具"));
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&site);
    }

    /// 流式保真测试：假 LLM 每 150ms 吐一帧，断言客户端也是逐帧收到（防缓冲合批回归）
    /// ——这是「问答像卡住、最后一次性出」Bug 的防线：SSE 链路任何一环缓冲都会暴露
    #[test]
    fn ask_streams_frames_incrementally() {
        // 假 LLM：裸 TcpStream 手写 chunked + 显式 flush，每 150ms 吐一帧
        //（不用 tiny_http 起服务端——它的 chunked 写出不 flush 会合批，正是要隔离的变量）
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            let Ok((mut s, _)) = listener.accept() else { return };
            let mut req_buf = vec![0u8; 65536];
            let _ = s.read(&mut req_buf); // 吃掉请求（假服务不解析）
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").unwrap();
            let mut send_chunk = |s: &mut std::net::TcpStream, data: &str| {
                let head = format!("{:x}\r\n", data.len());
                s.write_all(head.as_bytes()).unwrap();
                s.write_all(data.as_bytes()).unwrap();
                s.write_all(b"\r\n").unwrap();
                s.flush().unwrap();
            };
            for i in 0..5 {
                send_chunk(&mut s, &format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"字{i}\"}}}}]}}\n\n"));
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
            send_chunk(&mut s, "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n");
            s.write_all(b"0\r\n\r\n").unwrap();
            s.flush().unwrap();
        });
        let cfg = LlmConfig { base_url: format!("http://127.0.0.1:{port}"), api_key: "t".into(), model: "t".into() };
        let t0 = std::time::Instant::now();
        let mut arrivals: Vec<u128> = Vec::new();
        let (home, site) = setup_kb("stream");
        let ctx = Mutex::new(SearchCtx::new(home.clone(), site.clone()));
        ask(&ctx, &cfg, "hi", vec![], 5, false, "", |ev| {
            if matches!(ev, AskEvent::Delta(_)) {
                arrivals.push(t0.elapsed().as_millis());
            }
        }).unwrap();
        assert_eq!(arrivals.len(), 5, "应收 5 帧：{arrivals:?}");
        // 逐帧到达：第 2 帧起相邻间隔应 >= 100ms（假 LLM 150ms 一帧；合批则间隔 ≈ 0）
        for w in arrivals.windows(2) {
            assert!(w[1] - w[0] >= 100, "帧被合批了！到达时间：{arrivals:?}");
        }
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&site);
    }

    /// 假 LLM 服务：第一轮返回 tool_call，第二轮流式返回答案——打全多轮循环
    #[test]
    fn ask_multi_round_with_fake_llm() {
        // 假 LLM：tiny_http 起一个，按请求次数返回脚本化 SSE
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tiny_http::Server::from_listener(listener, None).unwrap();
        let round1 = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"get_doc\",\"arguments\":\"{\\\"path\\\":\\\"a/b.md\\\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        ).to_string();
        let round2 = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"已拿到文档\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"查询接口\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"在 a/b.md\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        ).to_string();
        let handle = std::thread::spawn(move || {
            let mut count = 0;
            while let Ok(Some(req)) = server.recv_timeout(std::time::Duration::from_secs(30)) {
                count += 1;
                let body = if count == 1 { &round1 } else { &round2 };
                let _ = req.respond(tiny_http::Response::from_string(body.clone()));
                if count >= 2 {
                    return count;
                }
            }
            count
        });
        let (home, site) = setup_kb("loop");
        let ctx = Mutex::new(SearchCtx::new(home.clone(), site.clone()));
        let cfg = LlmConfig {
            base_url: format!("http://127.0.0.1:{port}"),
            api_key: "test".into(),
            model: "test".into(),
        };
        let mut events: Vec<String> = Vec::new();
        ask(&ctx, &cfg, "查询接口在哪", vec![], 5, false, "a/b.md", |ev| {
            events.push(format!("{ev:?}"));
        }).unwrap();
        let rounds = handle.join().unwrap();
        assert_eq!(rounds, 2, "应两轮：工具调用→回答");
        assert!(events.iter().any(|e| e.contains("ToolStart")), "应有工具开始事件（前端过程可见）");
        assert!(events.iter().any(|e| e.contains("ToolCall")), "应有工具调用事件");
        assert!(events.iter().any(|e| e.contains("Think")), "应有思考链");
        assert!(events.iter().any(|e| e.contains("Delta")), "应有正文流");
        assert!(events.iter().any(|e| e.contains("Done")), "应完成");
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&site);
    }
}
