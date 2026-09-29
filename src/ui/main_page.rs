//! 主页标签页：文档服务启停 + 状态 + 打开浏览器
//! 静态构建模式：启动 = 构建 vitepress → 内嵌服务上线；md 变更自动重建。

use crate::core::{config, service};
use crate::App;

impl App {
    pub(crate) fn ui_main(&mut self, ui: &mut egui::Ui) {
        // 轮询构建链结果 + 监听事件（debounce 重建）
        service::tick(&mut self.server, self.global_cfg.auto_rebuild);

        ui.heading("本地文档站");
        ui.label("项目文档聚合：构建为静态站点，内嵌服务托管，浏览器阅读。");
        ui.add_space(12.0);

        // 无项目时不可用
        let Some(project) = self.project_root.clone() else {
            ui.label("未选择项目（顶部项目栏选择）");
            return;
        };

        let phase = self.server.phase.clone();
        let busy = matches!(phase, service::Phase::InstallingDeps | service::Phase::Building);
        let serving = phase == service::Phase::Serving;

        ui.horizontal(|ui| {
            if serving {
                if ui.button("停止服务").clicked() {
                    service::stop(&mut self.server);
                }
                if ui.button("打开浏览器").clicked() {
                    let _ = open::that(format!("http://localhost:{}", self.server.port));
                }
            } else {
                let btn = ui.add_enabled(!busy, egui::Button::new("启动服务"));
                if btn.clicked() {
                    let g = config::read_global();
                    let p = config::read_project(&project);
                    service::start(&project, &g, &p, &mut self.server);
                }
            }
        });

        ui.add_space(8.0);
        match phase {
            service::Phase::Serving => {
                ui.label(format!("运行中：http://localhost:{}", self.server.port));
                // 索引状态条（§6.1：状态可见、不阻塞）
                if let Ok(s) = self.server.index_status.read() {
                    match s.state.as_str() {
                        "indexing" => {
                            ui.label(format!("知识库索引中 {}/{} 文件…", s.done, s.total));
                        }
                        "ready" => {
                            ui.label(format!("知识库就绪：{} 块（{}）", s.chunks, s.updated_at));
                        }
                        "failed" => {
                            ui.colored_label(egui::Color32::YELLOW, format!("知识库索引失败：{}（不影响文档站）", s.error));
                        }
                        _ => {}
                    }
                }
                if self.server.rebuilding {
                    ui.label("文档已变更，正在后台重建…");
                } else if self.global_cfg.auto_rebuild {
                    ui.label("自动重建已开启（md 保存后约 7 秒生效，手动刷新查看）");
                }
                if !self.server.last_error.is_empty() {
                    ui.colored_label(egui::Color32::YELLOW, format!("提示：{}", self.server.last_error));
                }
            }
            service::Phase::InstallingDeps => {
                ui.label("正在安装依赖（npm install，首次需几分钟）…");
            }
            service::Phase::Building => {
                ui.label("正在构建文档（vitepress build，约几秒）…");
            }
            service::Phase::Failed => {
                ui.colored_label(egui::Color32::RED, format!("启动失败：{}", self.server.last_error));
            }
            _ => {
                ui.label("未运行");
            }
        }
    }
}
