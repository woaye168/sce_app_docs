//! 非 UI 逻辑：配置（config）/ node 探测（node）/ 站点生成（site）/ 构建管线（builder）
//! / 内嵌静态服务（httpd）/ 文档监听（watcher）/ 服务编排（service）
pub mod builder;
pub mod config;
pub mod httpd;
pub mod node;
pub mod service;
pub mod site;
pub mod watcher;
