//! 本地文档站（sce_app_docs）：基于 bgd_appsdk 的标准应用骨架
//!
//! 本文件为入口聚合：应用状态 + ShellApp 壳实现（ui_tab 只做分发）；
//! 标签页 UI 分散在 src/ui/ 各页面文件（impl App）。
//!
//! 公共逻辑（CLI 分发 --quit/notify、单实例、看守线程、--background、项目解析、窗口壳）
//! 由 bgd_appsdk::app::run 全托管——业务只需实现 ShellApp（标签页渲染）。

// Windows 下不弹出黑色控制台窗口
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod core;
mod ui;

use std::path::PathBuf;

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const APP_NAME: &str = "本地文档站";

fn main() -> eframe::Result<()> {
    // CLI 子命令（自测/自动化用；命中即控制台模式执行，不进 GUI）
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 2 && args[1] == "serve" {
        run_cli_serve(&args);
        std::process::exit(0);
    }
    bgd_appsdk::app::run(
        bgd_appsdk::app::AppOptions {
            app_name: APP_NAME,
            inner_size: [860.0, 640.0],
            min_size: [720.0, 520.0],
            // 单实例/信号前缀一律由 appsdk 按 exe 名推导，禁止硬编码
            si_prefix: None,
            is_valid_project: Some(|p| p.join(".bgd").is_dir()),
            app: App::default(),
        },
        APP_VERSION,
    )
}

/// 应用状态（壳只负责框架，业务状态都放这里）
struct App {
    /// 当前项目根（on_project_changed 回调维护）
    project_root: Option<PathBuf>,
    /// 状态栏文本
    status: String,
    /// dev server 状态（主页启停）
    server: crate::core::server::ServerState,
    /// 全局配置（进入设置页时读盘刷新）
    global_cfg: crate::core::config::GlobalConfig,
    /// 项目配置（随 on_project_changed 重读）
    project_cfg: crate::core::config::ProjectConfig,
    /// 设置页编辑态（文本框绑定；保存才落盘）
    node_path_edit: String,
    port_edit: String,
}

impl Default for App {
    fn default() -> Self {
        let global_cfg = crate::core::config::read_global();
        Self {
            project_root: None,
            status: String::new(),
            server: crate::core::server::ServerState::default(),
            node_path_edit: global_cfg.node_path.clone(),
            port_edit: if global_cfg.port == 0 { String::new() } else { global_cfg.port.to_string() },
            global_cfg,
            project_cfg: crate::core::config::ProjectConfig::default(),
        }
    }
}

impl App {
    /// 项目切换时重读项目配置（并刷状态栏）
    fn reload_project_cfg(&mut self) {
        self.project_cfg = self
            .project_root
            .as_deref()
            .map(crate::core::config::read_project)
            .unwrap_or_default();
    }
}

const TABS: &[bgd_appsdk::ui::ShellTab] = &[
    bgd_appsdk::ui::ShellTab { id: "main", label: "主页" },
    bgd_appsdk::ui::ShellTab { id: "global", label: "全局设置" },
    bgd_appsdk::ui::ShellTab { id: "project", label: "项目设置" },
];

/// CLI serve 子命令：sce_app_docs serve --project-path <项目根> [--port <端口>] [--timeout <秒>] [--lan]
/// 同步阻塞：启动 dev server → 轮询端口直到就绪/超时 → 打印结果并 kill 退出。
/// 专供自测/自动化（AI 无需 GUI 即可端到端验证服务可用）。--lan 绑 0.0.0.0（局域网可访问）。
fn run_cli_serve(args: &[String]) {
    use crate::core::{config, server};
    let get_arg = |flag: &str| -> Option<String> {
        args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned()
    };
    let Some(project) = get_arg("--project-path").map(PathBuf::from) else {
        eprintln!("缺少 --project-path <项目根>");
        std::process::exit(2);
    };
    let timeout_secs: u64 = get_arg("--timeout").and_then(|s| s.parse().ok()).unwrap_or(30);
    let mut st = server::ServerState::default();
    let mut g = config::read_global();
    if let Some(p) = get_arg("--port").and_then(|s| s.parse().ok()) {
        g.port = p;
    }
    if args.iter().any(|a| a == "--lan") {
        g.lan_access = true;
    }
    if let Some(h) = get_arg("--allowed-hosts") {
        g.allowed_hosts = h;
    }
    let p = config::read_project(&project);
    server::start(&project, &g, &p, &mut st);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    // 等待 Running（期间可能短暂 Failed——vitepress 热重载 restart 瞬间端口断连，boot_tick 会误判；
    // 真 Failed 是进程没起来/报错，日志 tail 有错误内容）
    loop {
        server::boot_tick(&mut st);
        match st.phase {
            server::StartPhase::Running => {
                println!("OK http://localhost:{}", st.port);
                server::stop(&mut st);
                return;
            }
            server::StartPhase::Failed => {
                // 区分真假 Failed：日志含 error/EADDRINUSE/address 才是启动失败，
                // 「restarting server」是热重载中间态，继续等
                let err = st.last_error.to_lowercase();
                if err.contains("error") || err.contains("eaddrinuse") || err.contains("address already") {
                    eprintln!("FAILED {}", st.last_error);
                    server::stop(&mut st);
                    std::process::exit(1);
                }
                // 热重载中间态：重置继续等
                st.phase = server::StartPhase::Starting;
                st.last_error.clear();
            }
            _ if std::time::Instant::now() > deadline => {
                eprintln!("TIMEOUT 等待服务就绪超时（{}s）", timeout_secs);
                server::stop(&mut st);
                std::process::exit(1);
            }
            _ => std::thread::sleep(std::time::Duration::from_millis(200)),
        }
    }
}

impl bgd_appsdk::ui::ShellApp for App {
    fn app_title(&self) -> &'static str {
        APP_NAME
    }

    fn tabs(&self) -> &[bgd_appsdk::ui::ShellTab] {
        TABS
    }

    fn ui_tab(&mut self, ui: &mut egui::Ui, tab: &str) {
        match tab {
            "main" => self.ui_main(ui),
            "global" => self.ui_global(ui),
            "project" => self.ui_project(ui),
            _ => {}
        }
    }

    fn on_project_changed(&mut self, project: Option<&std::path::Path>) {
        // 切项目即停旧服务（旧 dev server 服务的是旧项目站点，留着会误导）
        crate::core::server::stop(&mut self.server);
        self.project_root = project.map(|p| p.to_path_buf());
        self.reload_project_cfg();
        if let Some(p) = project {
            self.status = format!("当前项目: {}", p.display());
        }
    }

    fn status_text(&self) -> String {
        if self.server.port > 0 {
            format!("运行中 http://localhost:{}", self.server.port)
        } else {
            self.status.clone()
        }
    }
}
