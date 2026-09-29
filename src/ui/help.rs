//! 帮助页：使用说明（配置中转站 / 模型导入 / 问答 / CLI）

use crate::App;

impl App {
    pub(crate) fn ui_help(&mut self, ui: &mut egui::Ui) {
        ui.heading("帮助");
        ui.add_space(8.0);
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.label("【文档服务】主页选项目 → 启动服务 → 浏览器打开。改 md 自动重建（约 7 秒），手动刷新查看。");
            ui.add_space(4.0);
            ui.label("【AI 问答】AI 设置页填 LLM 中转站（base_url / api_key / model）→ 保存并测试连通 → 文档站右下角出现问答按钮。");
            ui.label("问答基于本地向量知识库检索文档，回答附出处链接；embedding/reranker 模型本地运行，不消耗 API 费用。");
            ui.add_space(4.0);
            ui.label("【本地模型】AI 设置页「导入本地目录」装入预下载模型（可用 Motrix 等下载器从下载地址提前下好），或在线下载。");
            ui.add_space(4.0);
            ui.label("【局域网/公网】全局设置开「局域网访问」手机可看；配合 Cloudflare Tunnel 时开「允许域名」填域名。");
            ui.add_space(4.0);
            ui.label("【CLI 自测】sce_app_docs serve --project-path <项目根> [--port N] [--lan] [--allowed-hosts 域名]");
            ui.label("            [--llm-base URL] [--llm-key KEY] [--llm-model 模型]  （前台常驻，kill 即停）");
        });
    }
}
