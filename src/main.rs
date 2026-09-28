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
