//! OpenAI 兼容 LLM 客户端（用户中转站）：POST /chat/completions 流式 SSE。
//! 支持 reasoning_content（思考链）/ content（正文）/ tool_calls（工具调用增量）三路解析。
//! SSE 解析器是纯函数（SseParser），网络层薄封装——解析逻辑单测全覆盖，不靠真 LLM。

/// LLM 配置（AI 设置页）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LlmConfig {
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub model: String,
}

/// 工具调用（聚合增量后）
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args: String,
}

/// 流式事件
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    /// 思考链增量
    Think(String),
    /// 正文增量
    Delta(String),
}

/// 一轮完成的聚合结果
#[derive(Debug, Default)]
pub struct Completion {
    pub text: String,
    pub think: String,
    pub tool_calls: Vec<ToolCall>,
    /// finish_reason（stop / tool_calls / ...）
    pub finish: String,
}

/// SSE 解析器（无状态网络无关）：喂字节流，吐事件 + 聚合结果
#[derive(Default)]
pub struct SseParser {
    buf: String,
    pub completion: Completion,
    /// tool_calls 增量聚合（index → ToolCall）
    tc_map: std::collections::BTreeMap<usize, ToolCall>,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂一段字节，返回本段产出的事件
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<StreamEvent> {
        self.buf.push_str(&String::from_utf8_lossy(bytes));
        let mut events = Vec::new();
        // SSE 以 \n 分行；最后不完整行留 buffer
        while let Some(pos) = self.buf.find('\n') {
            let line = self.buf[..pos].trim_end_matches('\r').to_string();
            self.buf = self.buf[pos + 1..].to_string();
            if let Some(ev) = self.handle_line(&line) {
                events.push(ev);
            }
        }
        events
    }

    fn handle_line(&mut self, line: &str) -> Option<StreamEvent> {
        // 只处理 data: 行；event:/空行/[DONE] 跳过（finish_reason 在 data 里）
        let data = line.strip_prefix("data:")?.trim();
        if data.is_empty() || data == "[DONE]" {
            return None;
        }
        let v: serde_json::Value = serde_json::from_str(data).ok()?;
        let choice = v.get("choices")?.get(0)?;
        if let Some(fr) = choice.get("finish_reason").and_then(|f| f.as_str()) {
            self.completion.finish = fr.to_string();
        }
        let delta = choice.get("delta")?;
        let mut ev = None;
        if let Some(t) = delta.get("reasoning_content").and_then(|c| c.as_str()) {
            self.completion.think.push_str(t);
            ev = Some(StreamEvent::Think(t.to_string()));
        }
        if let Some(c) = delta.get("content").and_then(|c| c.as_str()) {
            self.completion.text.push_str(c);
            ev = Some(StreamEvent::Delta(c.to_string()));
        }
        // tool_calls 增量按 index 聚合
        if let Some(tcs) = delta.get("tool_calls").and_then(|t| t.as_array()) {
            for tc in tcs {
                let idx = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                let entry = self.tc_map.entry(idx).or_insert_with(|| ToolCall { id: String::new(), name: String::new(), args: String::new() });
                if let Some(id) = tc.get("id").and_then(|s| s.as_str()) {
                    entry.id = id.to_string();
                }
                if let Some(f) = tc.get("function") {
                    if let Some(n) = f.get("name").and_then(|s| s.as_str()) {
                        entry.name.push_str(n);
                    }
                    if let Some(a) = f.get("arguments").and_then(|s| s.as_str()) {
                        entry.args.push_str(a);
                    }
                }
            }
        }
        ev
    }

    /// 收尾：聚合 tool_calls 进 completion
    pub fn finish(mut self) -> Completion {
        self.completion.tool_calls = self.tc_map.into_values().collect();
        self.completion
    }
}

/// 发起一轮流式对话（blocking 外壳，内部 current_thread tokio 跑 reqwest async 流）。
/// **不能用 reqwest blocking 读 SSE**——blocking Read 会攒到流结束才返回（实测合批 bug），
/// 必须 async bytes_stream 逐块推送。on_event 实时回调思考/正文增量。
/// messages/tools 用 serde_json::Value 构建（OpenAI 兼容格式）。
pub fn chat_stream(
    cfg: &LlmConfig,
    messages: Vec<serde_json::Value>,
    tools: &[serde_json::Value],
    mut on_event: impl FnMut(StreamEvent),
) -> Result<Completion, String> {
    if cfg.base_url.is_empty() || cfg.api_key.is_empty() || cfg.model.is_empty() {
        return Err("LLM 未配置（AI 设置页填 base_url/api_key/model）".into());
    }
    let url = format!("{}/chat/completions", cfg.base_url.trim_end_matches('/'));
    let mut body = serde_json::json!({
        "model": cfg.model,
        "messages": messages,
        "stream": true,
    });
    if !tools.is_empty() {
        body["tools"] = serde_json::Value::Array(tools.to_vec());
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client
            .post(&url)
            .bearer_auth(&cfg.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("LLM 请求失败: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("LLM 返回 {status}: {}", text.chars().take(300).collect::<String>()));
        }
        let mut parser = SseParser::new();
        let mut stream = resp.bytes_stream();
        use futures_util::StreamExt;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| format!("读取流失败: {e}"))?;
            for ev in parser.feed(&chunk) {
                on_event(ev);
            }
        }
        Ok(parser.finish())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_content_and_reasoning() {
        let mut p = SseParser::new();
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"让我\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"想想\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"答案\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"在这\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let events = p.feed(sse.as_bytes());
        assert!(events.contains(&StreamEvent::Think("让我".into())));
        assert!(events.contains(&StreamEvent::Delta("答案".into())));
        let c = p.finish();
        assert_eq!(c.think, "让我想想");
        assert_eq!(c.text, "答案在这");
        assert_eq!(c.finish, "stop");
        assert!(c.tool_calls.is_empty());
    }

    #[test]
    fn aggregates_tool_calls_by_index() {
        let mut p = SseParser::new();
        // 模拟 OpenAI 增量 tool_calls：id/name 只在首片，args 逐片拼接
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"search_docs\",\"arguments\":\"\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"que\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"ry\\\":\\\"sql\\\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        p.feed(sse.as_bytes());
        let c = p.finish();
        assert_eq!(c.finish, "tool_calls");
        assert_eq!(c.tool_calls.len(), 1);
        assert_eq!(c.tool_calls[0].id, "call_1");
        assert_eq!(c.tool_calls[0].name, "search_docs");
        assert_eq!(c.tool_calls[0].args, "{\"query\":\"sql\"}");
    }

    #[test]
    fn split_chunks_reassemble() {
        // 字节流在 JSON 中间断开也能正确重组
        let mut p = SseParser::new();
        let full = "data: {\"choices\":[{\"delta\":{\"content\":\"你好\"}}]}\n\n";
        let bytes = full.as_bytes();
        let mid = 17;
        let e1 = p.feed(&bytes[..mid]);
        assert!(e1.is_empty(), "不完整行不产事件");
        let e2 = p.feed(&bytes[mid..]);
        assert_eq!(e2, vec![StreamEvent::Delta("你好".into())]);
    }

    #[test]
    fn unconfigured_errors() {
        let cfg = LlmConfig { base_url: String::new(), api_key: String::new(), model: String::new() };
        let r = chat_stream(&cfg, vec![], &[], |_| {});
        assert!(r.unwrap_err().contains("未配置"));
    }
}
