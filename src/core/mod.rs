//! 非 UI 逻辑：配置（config）/ node 探测（node）/ 站点生成（site）/ 构建管线（builder）
//! / 内嵌静态服务（httpd）/ 文档监听（watcher）/ 服务编排（service）
//! / 分块（chunk）/ 向量库（kb）/ 模型（embed）/ 索引（indexer）/ LLM（llm）/ 问答（ask）
pub mod builder;
pub mod chunk;
pub mod ask;
pub mod config;
pub mod embed;
pub mod httpd;
pub mod indexer;
pub mod kb;
pub mod llm;
pub mod mcp;
pub mod net;
pub mod node;
pub mod service;
pub mod site;
pub mod site_templates;
pub mod watcher;
