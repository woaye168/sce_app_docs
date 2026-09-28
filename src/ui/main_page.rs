//! 主页标签页：dev server 启停 + 状态 + 打开浏览器

use crate::core::{config, server};
use crate::App;

impl App {
    pub(crate) fn ui_main(&mut self, ui: &mut egui::Ui) {
        ui.heading("文档站");
        ui.label("VuePress 本地文档服务：聚合框架 API 文档 + 项目文档，浏览器阅读。");
        ui.add_space(12.0);

        // 无项目时不可用
        let Some(project) = self.project_root.clone() else {
            ui.label("未选择项目（顶部项目栏选择）");
            return;
        };

        let running = server::is_running(&mut self.server);
        ui.horizontal(|ui| {
            if running {
                if ui.button("停止服务").clicked() {
                    server::stop(&mut self.server);
                }
                if ui.button("打开浏览器").clicked() {
                    let _ = open::that(format!("http://localhost:{}", self.server.port));
                }
            } else if ui.button("启动服务").clicked() {
                let g = config::read_global();
                let p = config::read_project(&project);
                server::start(&project, &g, &p, &mut self.server);
            }
        });

        ui.add_space(8.0);
        if running {
            ui.label(format!("运行中：http://localhost:{}", self.server.port));
            ui.label(format!("站点目录：{}", server::site_dir(&project).display()));
        } else if !self.server.last_error.is_empty() {
            ui.colored_label(egui::Color32::RED, format!("启动失败：{}", self.server.last_error));
        } else {
            ui.label("未运行");
        }
    }
}
