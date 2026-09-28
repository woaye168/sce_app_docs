//! 设置标签页分两级：全局（ui_global：node 路径/端口/全局文档源）
//! 与项目（ui_project：本项目追加源/禁用全局源）。保存才落盘。

use crate::core::config::{self, DocSource};
use crate::App;

impl App {
    /// 全局设置页
    pub(crate) fn ui_global(&mut self, ui: &mut egui::Ui) {
        ui.heading("全局设置");
        ui.label("所有项目共享；node 路径留空时启动自动扫描本机并回填。");
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            ui.label("node.exe 路径：");
            ui.text_edit_singleline(&mut self.node_path_edit);
            if ui.button("自动扫描").clicked() {
                if let Some(p) = crate::core::node::auto_detect() {
                    self.node_path_edit = p.display().to_string().replace('\\', "/");
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label(format!("端口（留空={}）：", crate::core::server::DEFAULT_PORT));
            ui.text_edit_singleline(&mut self.port_edit);
        });

        ui.add_space(8.0);
        ui.label("全局文档源（所有项目共享；名称唯一，相对路径按项目根解析）：");
        ui_source_list(ui, &mut self.global_cfg.sources);

        ui.add_space(8.0);
        if ui.button("保存").clicked() {
            self.global_cfg.node_path = self.node_path_edit.trim().to_string();
            self.global_cfg.port = self.port_edit.trim().parse().unwrap_or(0);
            config::write_global(&self.global_cfg);
            self.status = "全局配置已保存".into();
        }
    }

    /// 项目设置页
    pub(crate) fn ui_project(&mut self, ui: &mut egui::Ui) {
        ui.heading("项目设置");
        let Some(project) = self.project_root.clone() else {
            ui.label("未选择项目（顶部项目栏选择）");
            return;
        };
        ui.label(format!("当前项目：{}", project.display()));
        ui.label("本项目追加的文档源；与全局源合并（同名覆盖）。");
        ui.add_space(8.0);

        ui.checkbox(
            &mut self.project_cfg.disable_global_sources,
            "禁用全局文档源（本项目只用自己追加的）",
        );
        ui.label("项目文档源：");
        ui_source_list(ui, &mut self.project_cfg.sources);

        ui.add_space(8.0);
        if ui.button("保存").clicked() {
            match config::write_project(&project, &self.project_cfg) {
                Ok(()) => self.status = "项目配置已保存".into(),
                Err(e) => self.status = format!("保存失败: {e}"),
            }
        }
    }
}

/// 文档源列表编辑器（加/删行；name + path 两列 + 选择器）
fn ui_source_list(ui: &mut egui::Ui, sources: &mut Vec<DocSource>) {
    let mut del = None;
    for (i, s) in sources.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.label("名称");
            ui.text_edit_singleline(&mut s.name);
            ui.label("路径");
            ui.text_edit_singleline(&mut s.path);
            if ui.button("选择…").clicked() {
                if let Some(p) = rfd::FileDialog::new().pick_folder() {
                    s.path = p.display().to_string().replace('\\', "/");
                    // 名称留空时自动取路径尾名
                    if s.name.trim().is_empty() {
                        s.name = p
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                    }
                }
            }
            if ui.button("删除").clicked() {
                del = Some(i);
            }
        });
    }
    if let Some(i) = del {
        sources.remove(i);
    }
    if ui.button("+ 添加文档源").clicked() {
        sources.push(DocSource { name: String::new(), path: String::new() });
    }
}
