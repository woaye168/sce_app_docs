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

/// 域名白名单：开启后未列名域名 403，白名单域名/localhost/IP 正常；不开则任意 Host 可访问
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
