//! MCP 设置页：端点地址 / 工具清单 / 接入指引（Trae 等外部 agent 接回项目知识库）

use crate::core::service::Phase;
use crate::App;

impl App {
    pub(crate) fn ui_mcp(&mut self, ui: &mut egui::Ui) {
        ui.heading("MCP");
        ui.label("把项目文档知识库暴露为 MCP 端点，外部 agent（如 Trae）可直接检索——形成「开发产出文档 → AI 反哺开发」的闭环。");
        ui.add_space(8.0);

        ui.label("端点（Streamable HTTP）：");
        match self.server.phase {
            Phase::Serving | Phase::Building => {
                let port = self.server.port;
                ui.horizontal(|ui| {
                    ui.label("本机：");
                    ui.monospace(format!("http://localhost:{port}/_mcp"));
                    if ui.button("复制").clicked() {
                        ui.ctx().copy_text(format!("http://localhost:{port}/_mcp"));
                    }
                });
                if self.global_cfg.lan_access {
                    ui.horizontal(|ui| {
                        ui.label("局域网：");
                        ui.monospace(format!("http://{}:{port}/_mcp", lan_ip()));
                    });
                }
                if self.global_cfg.allowed_hosts_enabled && !self.global_cfg.allowed_hosts.is_empty() {
                    ui.horizontal(|ui| {
                        ui.label("公网：");
                        ui.monospace(format!("https://{}:{port}/_mcp", self.global_cfg.allowed_hosts.split(',').next().unwrap_or("")));
                    });
                }
            }
            _ => {
                ui.label("（主页启动服务后可用）");
            }
        }

        ui.add_space(8.0);
        ui.separator();
        ui.label("暴露工具（只读检索，不能操作电脑/写文件）：");
        ui.monospace("search_docs   语义检索项目文档库（query, top_k）");
        ui.monospace("get_doc       按文件路径取文档全文（path）");
        ui.monospace("list_sources  列出文档库所有已索引文件");

        ui.add_space(8.0);
        ui.separator();
        ui.label("接入 Trae：设置 → MCP → 添加 → 类型选 streamableHTTP → 填上面的本机端点地址。配置示例：");
        ui.monospace("{\n  \"mcpServers\": {\n    \"project-docs\": {\n      \"type\": \"streamableHttp\",\n      \"url\": \"http://localhost:18753/_mcp\"\n    }\n  }\n}");
        ui.label("注意：知识库按项目隔离——切换项目后 agent 查到的就是新项目的文档。");
    }
}

/// 局域网 IPv4（展示用；取第一个非回环地址）
fn lan_ip() -> String {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("8.8.8.8:80")?;
            s.local_addr()
        })
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| "局域网IP".into())
}
