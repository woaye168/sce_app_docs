# sce_app_docs · 本地文档站

BGD的SCE项目的本地文档聚合阅读工具：一个 egui 桌面应用，把项目的框架 API 文档（api_generated）与自定义文档源构建成 VitePress 静态站点，内嵌 HTTP 服务托管，浏览器阅读体验。

## 功能特性

- **静态构建 + 内嵌服务**：VitePress build 出静态站点，由应用内嵌 HTTP 服务托管——无外部常驻进程，关应用即停服，无端口残留
- **自动重建**：md 文件保存后自动重新构建（约 7 秒），手动刷新即见新内容；可在设置页关闭
- **零复制聚合**：文档源通过 Windows junction 原地引用，不做任何文件拷贝（项目根 md 除外，见下）
- **多文档源**：框架 API 文档 + `.bgd/docs.json` 配置的任意多个自定义源目录
- **左侧导航**：按目录结构多级递归生成 sidebar；目录含 index/README/AGENTS 时作为目录首页，否则纯分组
- **全文搜索**：VitePress 本地搜索（构建期生成索引），无需外部服务
- **智能首页**：项目根 md 文件（index/README/AGENTS 等）复制到 `_root/` 路由；首页为导航页（项目 md 列表 + 文档源列表 + `.bgd` 结构图）
- **明暗主题**：结构图与文档站整体跟随 VitePress 明暗主题自动切换
- **局域网访问**：可选绑定 `0.0.0.0`，手机/其他设备直接打开
- **域名白名单**：配合 Cloudflare Tunnel 等反向代理，界面配置域名 + 开关
- **配置化**：站点模板（`vitepress_home/site_template.json`）可手改覆盖标题/搜索/链接风格等
- **语义检索**：文档分块 → 本地向量模型（bge-m3，免 API 费用）→ sqlite 向量库增量索引，`/_api/search` 开箱可用
- **AI 问答**：右下角毛玻璃问答面板——多轮 Tool Calling（LLM 自主检索文档、思考链/工具调用可视化、回答附出处链接），明暗主题跟随；LLM 走你自己的中转站（OpenAI 兼容接口）
- **MCP 端点**：`/_mcp`（Streamable HTTP）暴露 search_docs/get_doc/list_sources 三个只读工具，接回 Trae 等 agent 形成「文档 → 反哺开发」闭环
- **安全边界**：检索/问答/MCP 全部只读文档库，不能执行命令、不能写文件、不能读任意路径

## 安装与使用

通过宿主 [bgd_sce_tools](https://github.com/woaye168/bgd_sce_tools) 的「应用市场」安装。宿主启动时传入 `--project-path <项目根>`，点「启动服务」后浏览器打开本地端口即可阅读。

文档源配置（项目 `.bgd/docs.json`）：

```json
{
  "sources": [
    { "name": "requirements", "path": "D:/path/to/doc/requirements" },
    { "name": "research", "path": "D:/path/to/doc/research" }
  ]
}
```

## 技术栈

- **桌面壳**：Rust + eframe/egui，基于 [bgd_appsdk](https://github.com/woaye168/bgd_sce_appsdk) 统一入口（单实例/看守线程/日志/配置全托管）
- **文档渲染**：VitePress 2.x（Vite + Vue 3），构建模式产出静态站点，全局安装于应用旁 `vitepress_home/`
- **内嵌服务**：tiny_http（静态文件托管 + Host 白名单 + 缓存策略），端口随进程生命周期
- **构建隔离**：vitepress build 短命子进程 + Windows Job Object（KILL_ON_JOB_CLOSE），app 异常退出也无孤儿
- **目录链接**：Windows junction（无需管理员特权）
- **向量检索**：fastembed-rs 本地跑 bge-m3 + bge-reranker-v2-m3（user-defined 加载，支持导入预下载模型），sqlite 持久化 + 内存暴力余弦（文档量小，毫秒级）
- **LLM**：reqwest 流式 SSE（OpenAI 兼容 / reasoning_content / tool_calls）

## CLI 子命令（自测/自动化）

```bash
sce_app_docs serve --project-path <项目根> [--port <端口>] [--lan] [--allowed-hosts <域名>]
                   [--llm-base <URL>] [--llm-key <KEY>] [--llm-model <模型>]
```

前台常驻：构建完成后打印 `OK http://localhost:<port>` 并持续服务（md 变更自动重建 + 索引增量），kill/Ctrl+C 即停（端口随进程释放）；失败 `FAILED <原因>`（exit 1）。LLM 参数继承 GUI 配置、命令行可覆盖。供 AI 或脚本端到端验证，无需操作 GUI。

## 测试

```bash
cargo test --lib                                 # 单元测试
cargo test --test lifecycle -- --test-threads=1  # 生命周期 E2E（真实 exe + vitepress build）
```

## 从源码构建

```bash
cargo build --release
```

## 发布

版本号唯一来源是 git tag；打 tag 触发 CI 构建并上传 exe 与 `app-release.json`，宿主应用市场负责分发更新。

```bash
git tag v0.x.0 && git push origin v0.x.0
```
