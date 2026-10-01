//! AI 设置页：LLM 配置 + 连通测试 + 轮数/rerank + 模型管理（导入本地/下载链接）+ 索引状态

use crate::core::{config, embed, llm};
use crate::App;

impl App {
    pub(crate) fn ui_ai(&mut self, ui: &mut egui::Ui) {
        ui.heading("AI 设置");
        ui.label("LLM 走你自己的中转站（OpenAI 兼容接口）；embedding/reranker 模型本地运行，不消耗任何 API。");
        ui.add_space(8.0);

        ui.label("LLM 配置：");
        ui.horizontal(|ui| {
            ui.label("base_url：");
            ui.text_edit_singleline(&mut self.global_cfg.llm_base_url);
        });
        ui.horizontal(|ui| {
            ui.label("api_key：");
            ui.add(egui::TextEdit::singleline(&mut self.global_cfg.llm_api_key).password(true));
        });
        ui.horizontal(|ui| {
            ui.label("model：");
            ui.text_edit_singleline(&mut self.global_cfg.llm_model);
            if ui.button("获取可用模型").clicked() {
                let cfg = llm::LlmConfig {
                    base_url: self.global_cfg.llm_base_url.clone(),
                    api_key: self.global_cfg.llm_api_key.clone(),
                    model: String::new(),
                };
                let (tx, rx) = std::sync::mpsc::channel();
                self.ai_models_rx = Some(rx);
                std::thread::spawn(move || {
                    let _ = tx.send(llm::fetch_models(&cfg));
                });
            }
        });
        // 收模型列表结果；有列表时给下拉（选中即写入 model，免手敲）
        if let Some(rx) = &self.ai_models_rx {
            if let Ok(r) = rx.try_recv() {
                match r {
                    Ok(list) => {
                        self.status = format!("拉到 {} 个可用模型", list.len());
                        self.ai_models = list;
                    }
                    Err(e) => self.status = format!("获取模型失败：{e}"),
                }
                self.ai_models_rx = None;
            }
        }
        if !self.ai_models.is_empty() {
            ui.horizontal(|ui| {
                ui.label("下拉选择：");
                let mut cur = self.global_cfg.llm_model.clone();
                egui::ComboBox::from_id_salt("llm_model_pick")
                    .selected_text(if cur.is_empty() { "（选择模型）".to_string() } else { cur.clone() })
                    .show_ui(ui, |ui| {
                        for m in &self.ai_models {
                            ui.selectable_value(&mut cur, m.clone(), m);
                        }
                    });
                if cur != self.global_cfg.llm_model {
                    self.global_cfg.llm_model = cur;
                    config::write_global(&self.global_cfg);
                    self.status = format!("已选模型：{}", self.global_cfg.llm_model);
                }
            });
        }
        ui.horizontal(|ui| {
            ui.label("最大工具调用轮数：");
            ui.add(egui::DragValue::new(&mut self.global_cfg.max_tool_rounds).range(1..=20));
            ui.checkbox(&mut self.global_cfg.rerank_enabled, "检索重排（rerank，建议开）");
        });
        ui.horizontal(|ui| {
            if ui.button("保存并测试连通").clicked() {
                config::write_global(&self.global_cfg);
                let cfg = llm::LlmConfig {
                    base_url: self.global_cfg.llm_base_url.clone(),
                    api_key: self.global_cfg.llm_api_key.clone(),
                    model: self.global_cfg.llm_model.clone(),
                };
                // 后台线程测试，不卡 UI
                let (tx, rx) = std::sync::mpsc::channel();
                self.ai_test_rx = Some(rx);
                std::thread::spawn(move || {
                    let r = llm::chat_stream(&cfg, vec![serde_json::json!({"role":"user","content":"ping，回一个字：pong"})], &[], |_| {});
                    let _ = tx.send(match r {
                        Ok(c) => format!("连通正常：{}", c.text.chars().take(50).collect::<String>()),
                        Err(e) => format!("连通失败：{e}"),
                    });
                });
            }
            // 收测试结果
            if let Some(rx) = &self.ai_test_rx {
                if let Ok(msg) = rx.try_recv() {
                    self.status = msg.clone();
                    self.ai_test_result = msg;
                    self.ai_test_rx = None;
                }
            }
            if !self.ai_test_result.is_empty() {
                ui.label(&self.ai_test_result);
            }
        });

        ui.add_space(12.0);
        ui.separator();
        ui.label("本地模型（向量化 + 重排，首次需导入或下载）：");
        for kind in [embed::ModelKind::Embedding, embed::ModelKind::Reranker] {
            let home = crate::core::site::vitepress_home();
            let ready = embed::model_ready(&home, kind);
            ui.horizontal(|ui| {
                let (icon, text) = if ready { ("✓", "已就绪") } else { ("✗", "未安装") };
                ui.label(format!("{} {}：{}", icon, kind.dir_name(), text));
                if ui.button("导入本地目录…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().pick_folder() {
                        self.status = match embed::import_local(&home, kind, &p) {
                            Ok(d) => format!("已导入到 {}", d.display()),
                            Err(e) => format!("导入失败：{e}"),
                        };
                    }
                }
                if ui.link("下载地址").clicked() {
                    let _ = open::that(kind.download_url());
                }
            });
        }
        ui.label("提示：导入目录可以是仓库根（含 onnx/ 子目录）或文件平铺目录；在线下载可用 hf-mirror 镜像或 clash 代理。");

        ui.add_space(12.0);
        ui.separator();
        ui.label("知识库索引状态：");
        if let Ok(s) = self.server.index_status.read() {
            match s.state.as_str() {
                "indexing" => { ui.label(format!("索引中 {}/{} 文件…", s.done, s.total)); }
                "ready" => { ui.label(format!("就绪：{} 块（最后更新 {}）", s.chunks, s.updated_at)); }
                "failed" => { ui.colored_label(egui::Color32::RED, format!("失败：{}", s.error)); }
                _ => { ui.label("未启动（主页启动服务后自动建立索引）"); }
            }
        }
    }
}
