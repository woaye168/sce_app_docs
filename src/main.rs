//! 本地文档站（sce_app_docs）：基于 bgd_appsdk 的标准应用骨架
//!
//! 本文件为入口聚合：应用状态 + ShellApp 壳实现（ui_tab 只做分发）；
//! 标签页 UI 分散在 src/ui/ 各页面文件（impl App）。
//!
//! 公共逻辑（CLI 分发 --quit/notify、单实例、看守线程、--background、项目解析、窗口壳）
//! 由 bgd_appsdk::app::run 全托管——业务只需实现 ShellApp（标签页渲染）。
//!
//! 架构（v0.2 重构）：静态构建 + 内嵌 HTTP 服务。端口由 exe 自己持有，
//! vitepress 只在构建时短命 spawn（Job Object 兜底）——无孤儿进程、无端口漂移。

// Windows 下不弹出黑色控制台窗口
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod ui;

use sce_app_docs::core;
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
    /// 文档服务状态（主页启停）
    server: core::service::ServiceState,
    /// 全局配置（进入设置页时读盘刷新）
    global_cfg: core::config::GlobalConfig,
    /// 项目配置（随 on_project_changed 重读）
    project_cfg: core::config::ProjectConfig,
    /// 设置页编辑态（文本框绑定；保存才落盘）
    node_path_edit: String,
    port_edit: String,
}

impl Default for App {
    fn default() -> Self {
        let global_cfg = core::config::read_global();
        Self {
            project_root: None,
            status: String::new(),
            server: core::service::ServiceState::default(),
            node_path_edit: global_cfg.node_path.clone(),
            port_edit: if global_cfg.port == 0 { String::new() } else { global_cfg.port.to_string() },
            global_cfg,
            project_cfg: core::config::ProjectConfig::default(),
        }
    }
}

impl App {
    /// 项目切换时重读项目配置（并刷状态栏）
    fn reload_project_cfg(&mut self) {
        self.project_cfg = self
            .project_root
            .as_deref()
            .map(core::config::read_project)
            .unwrap_or_default();
    }
}

const TABS: &[bgd_appsdk::ui::ShellTab] = &[
    bgd_appsdk::ui::ShellTab { id: "main", label: "主页" },
    bgd_appsdk::ui::ShellTab { id: "global", label: "全局设置" },
    bgd_appsdk::ui::ShellTab { id: "project", label: "项目设置" },
];

/// CLI serve 子命令：sce_app_docs serve --project-path <项目根> [--port <端口>] [--lan] [--allowed-hosts <域名>]
/// 前台常驻：构建完成开始服务后打印 OK 行，之后持续运行（md 变更自动重建），
/// kill/Ctrl+C 即停（端口随进程释放）。失败打印 FAILED 退出 1。
/// 专供自测/自动化：这就是真实代码路径，测它 = 测最终产物。
fn run_cli_serve(args: &[String]) {
    use core::{config, service};
    let get_arg = |flag: &str| -> Option<String> {
        args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned()
    };
    let Some(project) = get_arg("--project-path").map(PathBuf::from) else {
        eprintln!("缺少 --project-path <项目根>");
        std::process::exit(2);
    };
    let mut g = config::read_global();
    if let Some(p) = get_arg("--port").and_then(|s| s.parse().ok()) {
        g.port = p;
    }
    // CLI 确定性：lan/白名单只看命令行参数，不继承 GUI 持久化配置（防止脏配置污染自测）
    g.lan_access = args.iter().any(|a| a == "--lan");
    if let Some(h) = get_arg("--allowed-hosts") {
        g.allowed_hosts = h;
        g.allowed_hosts_enabled = true;
    } else {
        g.allowed_hosts_enabled = false;
    }
    g.auto_rebuild = true; // CLI 场景始终自动重建（自测/开发用途）
    let p = config::read_project(&project);
    let mut st = service::ServiceState::default();
    service::start(&project, &g, &p, &mut st);
    let mut announced = false;
    loop {
        service::tick(&mut st, true);
        match st.phase {
            service::Phase::Serving => {
                if !announced {
                    println!("OK http://localhost:{}", st.port);
                    announced = true;
                }
            }
            service::Phase::Failed => {
                eprintln!("FAILED {}", st.last_error);
                std::process::exit(1);
            }
            _ => {}
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
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
        // 切项目即停旧服务（旧站点服务的是旧项目内容，留着会误导）
        core::service::stop(&mut self.server);
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
