//! 生命周期端到端测试（黑盒：spawn 真实 exe 的 serve 子命令）
//! 覆盖验收标准：起服务 / HTTP 200 / 改 md 自动重建 / 切项目 / 关 app 即释放端口 /
//! 端口占用明确报错 / 域名白名单。
//!
//! 运行：cargo test --test lifecycle -- --test-threads=1
//! （vitepress build 较重，串行跑避免资源争抢）

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_sce_app_docs")
}

/// 造测试项目：.bgd/doc/api_generated/README.md + 根 index.md（含唯一标记）
fn make_project(tag: &str, marker: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bgd_docs_e2e_{}_{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".bgd/doc/api_generated")).unwrap();
    std::fs::write(
        dir.join(".bgd/doc/api_generated/README.md"),
        format!("# API 索引\n\n{marker}\n"),
    )
    .unwrap();
    std::fs::write(dir.join("index.md"), format!("# 项目首页\n\n{marker}\n")).unwrap();
    dir
}

struct ServeProc {
    child: Child,
}

impl Drop for ServeProc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// spawn serve 并等 stdout 出现 OK / FAILED / TIMEOUT 行（超时 panic）
fn spawn_serve(project: &Path, port: u16, extra: &[&str]) -> (ServeProc, String) {
    let mut cmd = Command::new(exe());
    cmd.args(["serve", "--project-path"])
        .arg(project)
        .args(["--port", &port.to_string()])
        .args(extra)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn().expect("spawn serve 失败");
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if tx.send(line.unwrap_or_default()).is_err() {
                return;
            }
        }
    });
    let (tx2, rx2) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            if tx2.send(line.unwrap_or_default()).is_err() {
                return;
            }
        }
    });
    // 等结果行（stdout OK 或 stderr FAILED/TIMEOUT），最长 180s（首次可能装依赖）
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut all = String::new();
    loop {
        if let Ok(line) = rx.try_recv() {
            all.push_str(&line);
            all.push('\n');
            if line.starts_with("OK ") {
                return (ServeProc { child }, all);
            }
        }
        if let Ok(line) = rx2.try_recv() {
            all.push_str(&line);
            all.push('\n');
            if line.starts_with("FAILED") || line.starts_with("TIMEOUT") {
                return (ServeProc { child }, all);
            }
        }
        assert!(Instant::now() < deadline, "等 serve 结果超时，输出：{all}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// 裸 HTTP/1.1 GET（Connection: close），返回 (状态码, body)
fn http_get(port: u16, path: &str, host: &str) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("连接失败");
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    write!(s, "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").unwrap();
    let mut buf = String::new();
    s.read_to_string(&mut buf).unwrap();
    let status: u16 = buf
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let body = buf.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    (status, body)
}

/// 双栈端口空闲检查
fn port_free(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok() && TcpListener::bind(("::1", port)).is_ok()
}

fn wait_until(mut cond: impl FnMut() -> bool, secs: u64, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    panic!("等待超时：{what}");
}

/// 起服务 → HTTP 200 含标记 → 进程仍在运行（前台常驻）→ kill 后端口释放（关 app 即停）
/// 注：/ 是聚合导航页；项目根 index.md 内容在 /_root/index.html
#[test]
fn serve_start_then_kill_frees_port() {
    let port = 18301u16;
    let project = make_project("start", "MARKER_START_A1");
    let (mut proc, out) = spawn_serve(&project, port, &[]);
    assert!(out.contains("OK "), "应输出 OK：{out}");
    // 前台常驻：OK 后进程不能退出
    assert_eq!(proc.child.try_wait().unwrap(), None, "serve 应前台常驻而非起完即退");
    let (status, _) = http_get(port, "/", "localhost");
    assert_eq!(status, 200, "首页应 200");
    let (status, body) = http_get(port, "/_root/index.html", "localhost");
    assert_eq!(status, 200, "项目首页应 200");
    assert!(body.contains("MARKER_START_A1"), "项目首页应含项目内容");
    proc.child.kill().unwrap();
    proc.child.wait().unwrap();
    wait_until(|| port_free(port), 10, "kill 后端口应释放（双栈）");
    let _ = std::fs::remove_dir_all(&project);
}

/// 改 md → 自动重建 → 刷新可见新内容（无需重启服务）
#[test]
fn md_change_triggers_rebuild() {
    let port = 18302u16;
    let project = make_project("rebuild", "MARKER_RB_V1");
    let (proc, out) = spawn_serve(&project, port, &[]);
    assert!(out.contains("OK "), "{out}");
    let (status, body) = http_get(port, "/_root/index.html", "localhost");
    assert_eq!(status, 200);
    assert!(body.contains("MARKER_RB_V1"));
    // 改文件（追加新标记）
    std::fs::write(
        project.join("index.md"),
        "# 项目首页\n\nMARKER_RB_V1\n\nMARKER_RB_V2\n",
    )
    .unwrap();
    wait_until(
        || http_get(port, "/_root/index.html", "localhost").1.contains("MARKER_RB_V2"),
        90,
        "改 md 后应自动重建并生效",
    );
    drop(proc);
    let _ = std::fs::remove_dir_all(&project);
}

/// 切项目 A→B→A：同端口复用，内容始终对应当前项目
#[test]
fn switch_project_roundtrip() {
    let port = 18303u16;
    let pa = make_project("swa", "MARKER_SW_AAA");
    let pb = make_project("swb", "MARKER_SW_BBB");
    {
        let (_p, out) = spawn_serve(&pa, port, &[]);
        assert!(out.contains("OK "), "A 启动失败：{out}");
        assert!(http_get(port, "/_root/index.html", "localhost").1.contains("MARKER_SW_AAA"));
    } // drop 即 kill
    wait_until(|| port_free(port), 10, "A 停止后端口释放");
    {
        let (_p, out) = spawn_serve(&pb, port, &[]);
        assert!(out.contains("OK "), "B 启动失败：{out}");
        let body = http_get(port, "/_root/index.html", "localhost").1;
        assert!(body.contains("MARKER_SW_BBB"), "应是 B 内容");
        assert!(!body.contains("MARKER_SW_AAA"), "不应残留 A 内容");
    }
    wait_until(|| port_free(port), 10, "B 停止后端口释放");
    {
        let (_p, out) = spawn_serve(&pa, port, &[]);
        assert!(out.contains("OK "), "回切 A 失败：{out}");
        assert!(http_get(port, "/_root/index.html", "localhost").1.contains("MARKER_SW_AAA"));
    }
    let _ = std::fs::remove_dir_all(&pa);
    let _ = std::fs::remove_dir_all(&pb);
}

/// 端口被占 → 明确报错退出（exit 1），不静默漂移
#[test]
fn port_occupied_fails_clearly() {
    let port = 18304u16;
    let _hold_v4 = TcpListener::bind(("127.0.0.1", port)).unwrap();
    let project = make_project("occ", "MARKER_OCC");
    let (mut proc, out) = spawn_serve(&project, port, &[]);
    assert!(
        out.contains("FAILED") || out.contains("TIMEOUT"),
        "端口被占应报 FAILED：{out}"
    );
    assert!(out.contains("占用") || out.contains("in use"), "报错应说明端口占用：{out}");
    wait_until(
        || proc.child.try_wait().unwrap().is_some(),
        10,
        "失败后进程应退出",
    );
    let _ = std::fs::remove_dir_all(&project);
}

/// 把本机预下载的模型硬链到 exe 旁 vitepress_home/models/（同盘零拷贝秒完）
/// 模型不在（CI/他机）返回 false，测试跳过
fn link_models() -> bool {
    let exe = PathBuf::from(exe());
    let home = exe.parent().unwrap().join("vitepress_home");
    let jobs = [
        ("bge-m3", "onnx", vec!["model.onnx", "model.onnx_data", "tokenizer.json", "config.json", "tokenizer_config.json", "special_tokens_map.json", "sentencepiece.bpe.model"]),
        ("bge-reranker-v2-m3", "", vec!["model.onnx", "model.onnx.data", "ort_config.json", "tokenizer.json", "config.json", "tokenizer_config.json", "special_tokens_map.json", "sentencepiece.bpe.model"]),
    ];
    for (name, sub, files) in &jobs {
        let src = PathBuf::from(r"D:\local_models").join(name).join(sub);
        if !src.is_dir() {
            return false;
        }
        let dst = home.join("models").join(name);
        for f in files {
            let (s, d) = (src.join(f), dst.join(f));
            if d.is_file() {
                continue;
            }
            std::fs::create_dir_all(&dst).unwrap();
            std::fs::hard_link(&s, &d).unwrap_or_else(|e| panic!("硬链 {} 失败: {e}", s.display()));
        }
    }
    true
}

/// 等索引就绪（index_status state == ready）
fn wait_index_ready(port: u16, secs: u64) -> String {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        let (code, body) = http_get(port, "/_api/index_status", "localhost");
        if code == 200 && body.contains("\"state\":\"ready\"") {
            return body;
        }
        if body.contains("\"state\":\"failed\"") {
            panic!("索引失败：{body}");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    panic!("索引就绪超时");
}

/// 语义检索全生命周期：起服务→索引就绪→检索命中→改 md→增量更新→新内容可检索
#[test]
fn search_index_lifecycle() {
    if !link_models() {
        eprintln!("本机无预下载模型（D:\\local_models），跳过");
        return;
    }
    let port = 18306u16;
    let project = make_project("search", "MARKER_SEARCH_BASE");
    // 加一篇有独特术语的文档（语义检索验证：查询词不出现原文中）
    std::fs::write(
        project.join(".bgd/doc/api_generated/arcane.md"),
        "# 奥术引擎\n\n紫晶魔导炉是奥术引擎的核心部件，负责能量转换与法力稳压。\n",
    )
    .unwrap();
    let (proc, out) = spawn_serve(&project, port, &[]);
    assert!(out.contains("OK "), "{out}");
    let status = wait_index_ready(port, 120);
    assert!(status.contains("\"chunks\":"), "{status}");
    // 语义检索：查「魔法炉」（原文无此词，纯语义命中）
    let (code, body) = http_get(port, "/_api/search?q=%E9%AD%94%E6%B3%95%E7%82%89&k=3", "localhost");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains("紫晶魔导炉"), "语义检索应命中：{body}");
    assert!(body.contains("/api/arcane"), "应带出处 URL：{body}");
    // 改文档 → 重建 + 索引增量 → 新内容可检索
    std::fs::write(
        project.join(".bgd/doc/api_generated/arcane.md"),
        "# 奥术引擎\n\n紫晶魔导炉是奥术引擎的核心部件。\n\n新增：霜寒编织术用于低温环境稳态维持。\n",
    )
    .unwrap();
    wait_index_ready(port, 120);
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut found = false;
    while Instant::now() < deadline {
        let (_, body) = http_get(port, "/_api/search?q=%E9%9C%9C%E5%AF%92&k=3", "localhost");
        if body.contains("霜寒编织术") {
            found = true;
            break;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    assert!(found, "改 md 后新内容应可检索");
    drop(proc);
    let _ = std::fs::remove_dir_all(&project);
}

/// 剥 chunked 编码（每块 = 十六进制长度行 + 数据 + CRLF，0 长度结束）
fn dechunk(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let Some(eol) = raw[i..].find("\r\n").map(|p| p + i) else { break };
        let size = usize::from_str_radix(raw[i..eol].trim(), 16).unwrap_or(0);
        if size == 0 {
            break;
        }
        let start = eol + 2;
        if start + size > bytes.len() {
            break;
        }
        out.push_str(&raw[start..start + size]);
        i = start + size + 2;
    }
    out
}

/// 裸 HTTP/1.1 POST（JSON body，Connection: close），返回 (状态码, 响应头, body)
fn http_post_full(port: u16, path: &str, body: &str, extra_headers: &[(&str, &str)]) -> (u16, String, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("连接失败");
    s.set_read_timeout(Some(Duration::from_secs(300))).unwrap();
    let extra: String = extra_headers.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).unwrap();
    let text = String::from_utf8_lossy(&buf).to_string();
    let status: u16 = text.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    let headers = text.split("\r\n\r\n").next().unwrap_or("").to_string();
    let raw = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    let body = if headers.to_lowercase().contains("transfer-encoding: chunked") {
        dechunk(&raw)
    } else {
        raw
    };
    (status, headers, body)
}

fn http_post(port: u16, path: &str, body: &str) -> (u16, String) {
    let (s, _, b) = http_post_full(port, path, body, &[]);
    (s, b)
}

/// SSE 流式保真（端到端）：假 LLM 每 150ms 一帧，断言 /_api/ask 客户端逐帧到达
/// ——「发送中…然后一次性全出」Bug（tiny_http chunked 输出不 flush 合批）的防线
#[test]
fn ask_sse_streams_incrementally() {
    // 假 LLM：裸 TcpStream 手写 chunked + flush，150ms 一帧
    let llm_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let llm_port = llm_listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        use std::io::Write as _;
        while let Ok((mut s, _)) = llm_listener.accept() {
            std::thread::spawn(move || {
                let mut buf = vec![0u8; 65536];
                let _ = s.read(&mut buf);
                s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").unwrap();
                for i in 0..5 {
                    let frame = format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"字{i}\"}}}}]}}\n\n");
                    s.write_all(format!("{:x}\r\n{frame}\r\n", frame.len()).as_bytes()).unwrap();
                    s.flush().unwrap();
                    std::thread::sleep(Duration::from_millis(150));
                }
                let tail = "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
                s.write_all(format!("{:x}\r\n{tail}\r\n0\r\n\r\n", tail.len()).as_bytes()).unwrap();
                s.flush().unwrap();
            });
        }
    });
    let port = 18310u16;
    let project = make_project("sse", "MARKER_SSE");
    let llm_base = format!("http://127.0.0.1:{llm_port}");
    let (_proc, out) = spawn_serve(&project, port, &["--llm-base", &llm_base, "--llm-key", "t", "--llm-model", "t"]);
    assert!(out.contains("OK "), "{out}");
    // 客户端：裸 socket 短超时读，统计每次读到的到达时间
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_millis(600))).unwrap();
    let body = r#"{"question":"hi"}"#;
    let req = format!("POST /_api/ask HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    s.write_all(req.as_bytes()).unwrap();
    let t0 = Instant::now();
    let mut arrivals: Vec<(u128, usize)> = Vec::new(); // (到达ms, 字节数)
    let mut buf = [0u8; 4096];
    loop {
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => arrivals.push((t0.elapsed().as_millis(), n)),
            Err(_) => break, // 超时也算一次观测机会
        }
        if t0.elapsed().as_millis() > 20000 { break; }
    }
    // start 帧应立即到达（<1s），且后续帧分批到达（至少 3 次间隔 >100ms 的读）
    assert!(!arrivals.is_empty(), "无任何数据");
    assert!(arrivals[0].0 < 1000, "首帧应 <1s（start 帧）：{arrivals:?}");
    let spread = arrivals.windows(2).filter(|w| w[1].0 - w[0].0 > 100).count();
    assert!(spread >= 3, "帧应分批到达（合批则间隔≈0）：{arrivals:?}");
    let _ = std::fs::remove_dir_all(&project);
}

/// mermaid 代码块经构建后应转成 Mermaid 组件标记（图表渲染在浏览器运行时做，SSG 产物含 graph 数据）
#[test]
fn mermaid_block_in_built_site() {
    let port = 18322u16;
    let project = make_project("mmd", "MARKER_MMD");
    std::fs::write(
        project.join(".bgd/doc/api_generated/diagram.md"),
        "# 架构图\n\n```mermaid\nflowchart TD\n  Alpha --> Beta\n```\n",
    )
    .unwrap();
    let (_proc, out) = spawn_serve(&project, port, &[]);
    assert!(out.contains("OK "), "{out}");
    let (code, body) = http_get(port, "/api/diagram.html", "localhost");
    assert_eq!(code, 200, "{body}");
    // 插件生效 = fence 被替换成 MermaidViewer 组件（SSG 预渲染为 mermaid-block，graph 数据在 JS payload）；
    // 未生效则 Shiki 按代码块渲染出 language-mermaid
    assert!(body.contains("mermaid-block"), "构建产物应含 MermaidViewer 组件：{}", &body[..body.len().min(500)]);
    assert!(!body.contains("language-mermaid"), "fence 不应还是代码块（插件未生效）");
    let _ = std::fs::remove_dir_all(&project);
}

#[test]
fn mcp_streamable_http_flow() {
    let port = 18309u16;
    let project = make_project("mcp", "MARKER_MCP");
    let (_proc, out) = spawn_serve(&project, port, &[]);
    assert!(out.contains("OK "), "{out}");

    // 1. initialize（SSE Accept 变体）→ 响应头带 session
    let (code, headers, body) = http_post_full(
        port, "/_mcp",
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#,
        &[("Accept", "application/json, text/event-stream")],
    );
    assert_eq!(code, 200, "{body}");
    let sid = headers.lines().find_map(|l| l.strip_prefix("Mcp-Session-Id: ")).unwrap_or("").trim().to_string();
    assert!(!sid.is_empty(), "应发 session：{headers}");
    assert!(body.contains("event: message"), "Accept SSE 应回 SSE 帧：{body}");
    assert!(body.contains("serverInfo"), "{body}");

    // 2. initialized 通知 → 202 无 body
    let (code, _, body) = http_post_full(port, "/_mcp", r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#, &[("Mcp-Session-Id", &sid)]);
    assert_eq!(code, 202, "{body}");
    assert!(body.is_empty(), "{body}");

    // 3. tools/list（纯 JSON 变体）
    let (code, _, body) = http_post_full(port, "/_mcp", r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#, &[("Mcp-Session-Id", &sid)]);
    assert_eq!(code, 200, "{body}");
    assert!(body.contains("search_docs") && body.contains("get_doc") && body.contains("list_sources"), "{body}");

    // 4. tools/call list_sources → 命中文档
    //（索引同步已改为不阻塞检索的后台增量，需轮询等就绪；超时按失败处理）
    let mut body = String::new();
    for _ in 0..120 {
        let (code, _, b) = http_post_full(port, "/_mcp", r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_sources","arguments":{}}}"#, &[("Mcp-Session-Id", &sid)]);
        assert_eq!(code, 200, "{b}");
        body = b;
        if body.contains("index.md") { break; }
        std::thread::sleep(Duration::from_millis(500));
    }
    assert!(body.contains("index.md"), "应列出索引文件：{body}");

    // 5. 无/假 session 被拒
    let (_, _, body) = http_post_full(port, "/_mcp", r#"{"jsonrpc":"2.0","id":4,"method":"tools/list"}"#, &[]);
    assert!(body.contains("Mcp-Session-Id"), "{body}");

    // 6. GET → 405（协议合规：不支持服务端推送流）
    let (code, _) = http_get(port, "/_mcp", "localhost");
    assert_eq!(code, 405);

    let _ = std::fs::remove_dir_all(&project);
}

#[test]
fn ask_requires_llm_config() {
    let port = 18307u16;
    let project = make_project("ask400", "MARKER_ASK400");
    let (_proc, out) = spawn_serve(&project, port, &[]);
    assert!(out.contains("OK "), "{out}");
    let (code, body) = http_post(port, "/_api/ask", r#"{"question":"你好"}"#);
    assert_eq!(code, 400, "{body}");
    assert!(body.contains("LLM 未配置"), "{body}");
    let _ = std::fs::remove_dir_all(&project);
}

/// 真实 LLM 问答全链路（环境变量门控：BGD_TEST_LLM_BASE/KEY/MODEL 齐才跑，CI 跳过）
/// 验证：SSE 流式 + done 帧 + 回答内容非空
#[test]
fn ask_live_llm_streaming() {
    let (Ok(base), Ok(key), Ok(model)) = (
        std::env::var("BGD_TEST_LLM_BASE"),
        std::env::var("BGD_TEST_LLM_KEY"),
        std::env::var("BGD_TEST_LLM_MODEL"),
    ) else {
        eprintln!("未设 BGD_TEST_LLM_*，跳过真实 LLM 测试");
        return;
    };
    if !link_models() {
        eprintln!("本机无预下载模型，跳过");
        return;
    }
    let port = 18308u16;
    let project = make_project("asklive", "MARKER_ASKLIVE");
    std::fs::write(
        project.join(".bgd/doc/api_generated/arcane.md"),
        "# 奥术引擎\n\n紫晶魔导炉是奥术引擎的核心部件，负责能量转换与法力稳压。额定功率 5000 法瓦。\n",
    )
    .unwrap();
    let (_proc, out) = spawn_serve(&project, port, &[
        "--llm-base", &base, "--llm-key", &key, "--llm-model", &model,
    ]);
    assert!(out.contains("OK "), "{out}");
    wait_index_ready(port, 120);
    let (code, body) = http_post(port, "/_api/ask", r#"{"question":"紫晶魔导炉的额定功率是多少？"}"#);
    assert_eq!(code, 200, "{body}");
    assert!(body.contains("\"type\":\"done\""), "应有 done 帧：{body}");
    assert!(body.contains("5000"), "回答应含检索到的功率数值：{body}");
    let _ = std::fs::remove_dir_all(&project);
}

#[test]
fn host_whitelist() {
    let port = 18305u16;
    let project = make_project("wl", "MARKER_WL");
    // 开启白名单
    let (proc1, out) = spawn_serve(&project, port, &["--allowed-hosts", "docs.example.com"]);
    assert!(out.contains("OK "), "{out}");
    assert_eq!(http_get(port, "/", "localhost").0, 200, "localhost 应放行");
    assert_eq!(http_get(port, "/", "127.0.0.1").0, 200, "IP 应放行");
    assert_eq!(http_get(port, "/", "docs.example.com").0, 200, "白名单域名应放行");
    assert_eq!(http_get(port, "/", "evil.com").0, 403, "未列名域名应 403");
    drop(proc1);
    wait_until(|| port_free(port), 10, "停止后端口释放");
    // 不开白名单：任意 Host 可访问
    let (proc2, out) = spawn_serve(&project, port, &[]);
    assert!(out.contains("OK "), "{out}");
    assert_eq!(http_get(port, "/", "anything.example.org").0, 200, "不开白名单应任意 Host 放行");
    drop(proc2);
    let _ = std::fs::remove_dir_all(&project);
}
