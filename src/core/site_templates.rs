//! 站点前端模板入口集：frontend/ 下的真实前端文件经 include_str! 编译期嵌入（exe 自包含分发），
//! site.rs 构建站点时写入主题目录。
//!
//! 约定：前端源码一律放 frontend/ 真实文件（IDE 高亮/lint/格式化/node 可测），
//! 禁止在 rs 里写大段前端代码字符串。config.mjs 因含运行时插值（title/sidebar），
//! 模板骨架留在 site.rs 的 format! 里（属生成逻辑，非前端源码）。

/// AI 问答组件 SFC（毛玻璃问答面板：SSE 流式 + 单气泡增量渲染 + 锚点导航 + 窗口三档）
pub(crate) const AI_CHAT_VUE: &str = include_str!("../../frontend/AiChat.vue");

/// VitePress 主题入口（默认主题 + AiChat 槽位 + mermaid viewer 注册）
pub(crate) const THEME_INDEX_JS: &str = include_str!("../../frontend/theme_index.js");

/// 结构图样式（目录树配色跟随明暗主题）
pub(crate) const CUSTOM_CSS: &str = include_str!("../../frontend/custom.css");

/// 流式 markdown 分段扫描器（纯函数，node --test test/stream_md.test.mjs 直测）
pub(crate) const STREAM_MD_MJS: &str = include_str!("../../frontend/stream_md.mjs");
