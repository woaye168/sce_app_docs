# AGENTS.md — sce_app_docs

> 本文件为 AI 助手提供项目上下文。修改代码前请先阅读。

## 项目定位

本地文档站（sce_app_docs）：独立的 egui 桌面应用。通过宿主 [bgd_sce_tools](https://github.com/woaye168/bgd_sce_tools) 的「应用市场」安装分发，宿主启动时传 `--project-path <项目根>`。应用单实例；`--background` 静默驻留；`--quit` 优雅退出；窗口 X = 正常退出。

## 技术栈与规范

- Rust 2021；eframe/egui 0.29；CLI 由 bgd_appsdk 统一入口托管（`--project-path` / `--background` / `--quit` / `notify`），本仓库不引入 clap
- **bgd_appsdk**（crates.io 公开包 `bgd_appsdk = "0.2"`，仓库 [bgd_sce_appsdk](https://github.com/woaye168/bgd_sce_appsdk)）：单实例/看守线程/日志/应用配置/**通用窗口壳 AppShell** 等公共基建，禁止在本仓库重复实现（UI 经 `ShellApp` trait 注册标签页即可）
- **模块拆分**：单文件接近 500 行必须按职责拆分

## 目录结构

```
src/lib.rs             # 库入口（暴露 core 供集成测试与 bin 复用）
src/main.rs            # 入口（CLI serve 分发 → bgd_appsdk::app::run）+ 应用状态 + ShellApp 壳实现
src/core/
  config.rs            # 双配置（全局 exe 旁 config.json + 项目 .bgd/docs.json）
  node.rs              # node.exe 探测
  site.rs              # 站点生成：junction 聚合源 + index.md 导航页 + config.mjs + 主题（含 AiChat.vue）
  builder.rs           # vitepress build 短命 spawn（Job Object KILL_ON_JOB_CLOSE 兜底防孤儿）
  httpd.rs             # 内嵌 HTTP 服务（tiny_http）：静态 + /_api/* + /_mcp 路由
  watcher.rs           # notify 监听 md 变更（真实路径，不经 junction）
  service.rs           # 服务编排：状态机 + debounce 重建 + dist 原子翻转 + 索引同步调度
  chunk.rs             # md 分块（标题链 + 段落边界 + 重叠）
  kb.rs                # sqlite 向量库（按文件 hash 增量 + 内存暴力余弦）
  embed.rs             # 本地模型管理（fastembed user-defined 加载 / 导入本地目录 / 下载链接）
  indexer.rs           # 索引管线（增量同步 + 检索 + rerank + 进度状态）
  llm.rs               # OpenAI 兼容 LLM 客户端（SSE 流式 / reasoning_content / tool_calls 聚合）
  ask.rs               # 多轮 Tool Calling 问答管线（工具定义/执行与 MCP 同一份实现）
  mcp.rs               # MCP Streamable HTTP（JSON-RPC + session + SSE）
src/ui/                # main_page.rs / settings.rs / ai.rs / mcp.rs / help.rs（impl App 分散定义）
test/lifecycle.rs      # 生命周期 E2E 测试（黑盒 spawn 真实 exe serve；见「测试」节）
app.json               # 应用市场静态元数据（不含版本；CI 合成 app-release.json）
.github/workflows/release.yml  # tag 触发构建发布
doc/research/          # 设计文档（架构重构方案等）
```

## 架构要点（v0.2 重构，2026-09-29）

**静态构建 + 内嵌服务**：vitepress 只在「构建」时短命 spawn（起→跑→自己退出，Job Object 双保险）；HTTP 服务由 exe 内嵌（tiny_http），端口随进程生命周期——无孤儿进程、无句柄丢失、无端口漂移。md 变更经 watcher debounce 1s 后重建（dist-a/dist-b 轮换 + 原子翻转，重建期间旧内容不掉线），手动刷新可见。

**禁止**回到「外部长寿 dev server 进程」架构（病史见 doc/research/2026-09-29-架构重构方案）。

## 架构要点（v0.3 知识库，2026-09-29）

**语义检索 + AI 问答 + MCP**：md → 分块（chunk）→ 本地 embedding（fastembed + bge-m3，模型导入/在线下载）→ sqlite 向量库按文件 hash 增量（kb/indexer）→ `/_api/search`；问答走多轮 Tool Calling（ask.rs，LLM 自主检索/深读，轮数界面可配 1-20），SSE 流式到毛玻璃问答组件；MCP 端点 `/_mcp`（Streamable HTTP）与问答共用同一份工具实现（ask::tools_defs/exec_tool）。

- **路由避让**：文档源占用 `/api/`——本应用 API 一律 `/_api/`，MCP 用 `/_mcp`，禁止新增裸 `/api/` 路由。
- **安全边界（硬约束）**：检索/问答/MCP 三个工具（search_docs/get_doc/list_sources）全部只读文档库；禁止加任何执行 shell/写文件/任意路径读取的工具。get_doc 只能读已索引文件（天然防路径穿越）。
- **模型本地化**：embedding/reranker 本地跑（fastembed user-defined 加载 vitepress_home/models/），不依赖外部 embedding API；LLM 才走用户中转站。
- **日志**：tracing → `<项目>/.bgd/log/docs-<日期>.log`（Windows GUI 子系统 println 被吞，排查必看此文件）。
- **设计全文**：doc/research/2026-09-29-rag-ai-mcp设计.md（含实施对账）。

**Windows 端口陷阱**：tiny_http::Server::http 的 bind 带 SO_REUSEADDR 语义能被挤占同端口——必须 std TcpListener 先 bind（默认独占）再 from_listener；lan 模式绑 0.0.0.0 前要先探测 127.0.0.1（wildcard 与具体地址 bind 可共存，具体地址抢流量）。

**CLI 确定性**：serve 的 lan/白名单只看命令行参数，不继承 GUI 持久化配置（防脏配置污染自测）。

## 架构要点（v0.3.1 mermaid，2026-10-01）

**mermaid 图表**：mermaid 代码块 → 图表，插件用自有 fork [woaye168/vitepress-plugin-mermaid-viewer](https://github.com/woaye168/vitepress-plugin-mermaid-viewer)（git 依赖锁 commit，builder.rs `DEPS_PACKAGE_JSON`；fork 改动：vp2 peer 适配 + 提交 dist 免构建；懒加载上游自带）。**选 viewer 版的原因**：点击图表开全屏 viewer（滚轮缩放/拖拽平移/下载 PNG/SVG/复制源码），大图可读性刚需；且它不要 withMermaid 包裹、三件套自注册（config 里 mermaidMarkdown+mermaidPlugin、主题 enhanceMermaid+client.css），无 vp 版本敏感 hack。暗色由组件自适配；问答气泡里的 mermaid 块在回答完成时渲染（AiChat.vue `renderMermaidIn`，inline svg 不走 viewer）。

**「当前文档」上下文**：AiChat 发问时带 `page`（useData page relativePath，与 kb 文件路径一致）→ ask.rs `build_system_prompt` 追加规则点名「当前文档」指代，LLM 直接 get_doc 读全文。**坑：useData 解构出的是 ref，script 里必须 `.value`**（漏写静默 undefined → 空 page，不报错）。

**输出格式与体验（v0.3.1 追加）**：system prompt 管输出形态——画图一律 mermaid（禁 ASCII/禁硬编码色）、代码带语言 fence、**正文不列参考**（参考由系统 Sources 帧附带）。气泡：mermaid 块 done 后 createApp 手动挂载 MermaidViewer（v-html 不编译组件；全屏交互与文档页一致）、highlight.js lib/common 懒加载高亮（token 色自绘明暗双色）、出处显示短名+末节（全路径悬停）、暗色阴影加深（.18 黑影在深底上变灰晕）。**mermaid 语义强调色**：文档写 `fill:mmdaccent,stroke:mmdaccentline` 占位符，fork render 按主题替换真实色（`var()`/引号被 mermaid parser 拒绝——实测；占位符必须裸字母 token）。**模型切换**：`/_api/models` 代理中转站 /models；设置页「获取可用模型」下拉存默认；聊天面板自制液态玻璃下拉（原生 select 弹层是 OS 控件不可 CSS、暗色极丑）临时切换（发问带 model 参数覆盖、localStorage 记忆、不落盘；**localStorage 必须 SSR 守卫**）。**git 依赖版本戳**：node_modules/.mermaid_viewer_sha，sha 变化强制重装（npm 对同 url git 依赖不主动更新）。

**出处锚点**：检索命中的 url 拼 `#标题锚点`（kb.rs `heading_anchor`，**严格对齐 @mdit-vue/shared slugify**：NFKD→特殊字符→`-`→折叠→去首尾→数字开头加`_`→小写，`·`保留——和 GitHub slugger 不同，实测比对过）；AiChat 出处点击自接管（goSource）：站点 cleanUrls=false 路由是 .html 风格，clean 路径不匹配会整页刷新丢 SPA 上下文导致锚点滚动失败。

**对话面板交互（v0.3.1 追加）**：流式渲染=单气泡增量提交——一条消息一个 `.ai-body` 气泡，内部按已闭合 fence 切 md/code/mermaid 块（`frontend/stream_md.mjs` 纯函数 `scanStream`，真实前端文件 `include_str!` 嵌入、node --test 可测；code/mermaid 块带稳定 key cc0/mm1 保组件身份，fence 闭合即高亮/渲染不被后续 delta 冲掉）。**禁止把块渲染成独立气泡卡片**（机制泄漏成视觉，被用户打回）。**坑：局部组件写运行时 template 字符串在 runtime-only vue 下静默渲染为空**（CodeSeg 曾因此整段代码消失）——必须用 render 函数。侧边用户消息锚点导航（左缘竖点 rail，PC hover 展开、移动端点展开/点外或点锚点关闭——**弹层必须是 rail 旁的 flex 兄弟节点**（in-flow），absolute top:0 会让弹层与垂直居中的 rail 错位，hover 路径一离开就 mouseleave 关弹层）；窗口三档尺寸（''/tall/full + 移动端媒体查询占满屏，goSource 自动退出 full）；head 注入 viewport `user-scalable=no` 防手机端页面被捏合拖大（mermaid viewer 是自身 transform 手势缩放，不受影响）。**大坑：手写 `-webkit-backdrop-filter` 会被 lightningcss 去重吞掉无前缀 `backdrop-filter`**——Chrome 142+ 已不支持 -webkit 别名，产物里只剩 webkit 前缀 = 毛玻璃从不生效（「穿透看不清」根治其实是透明度）。**只写无前缀属性**，交给构建处理。暗色下代码块底色要用白 7% 而不是黑 6%（深底上再压黑等于没底色）。**内容卡片一律复用 vp-doc**：气泡容器挂 `vp-doc` class + `vpFence` 产出 VP 代码块包裹结构（div.language-xxx + lang 角标），代码/表格/引用/列表全走 VP 主题样式，禁止自绘内容卡片；vp-doc 的 pre 横向 padding 依赖 shiki .line span，非 shiki 来源要自补横向 padding。mermaid 连线标签一律 `A -->|文字| B`（`<-.- "x" .->` 非法，LLM 高频踩）。

## 使用方约定（改代码前必读）

- 应用只需实现 `ShellApp` 并调 `bgd_appsdk::app::run`——公共逻辑（CLI 分发、单实例、看守线程、项目解析、窗口壳）全托管，禁止自己再写一套。
- 新增标签页 = `src/ui/` 加页面文件（`impl App` 定义 `ui_xxx`）+ `ui/mod.rs` 加 mod 声明 + main.rs 的 `TABS` / `ui_tab` 分发各加一行。
- 宿主协议：`--background` 静默驻留、`--quit` 优雅退出、`notify key=value` 解耦通知（切项目时宿主会发 `notify project_path=<路径>`，壳自动刷新并回调 `on_project_changed`）。
- **命名契约**：宿主按 `<id>.exe` 落盘，单实例/信号前缀一律由 appsdk 按 exe 名推导（`app::default_si_prefix`），应用方禁止硬编码（`AppOptions.si_prefix` 保持 `None`）。
- **关键结论**：egui 窗口隐藏时事件循环休眠，任何信号处理不能放 UI update，也不能依赖 ViewportCommand——这类需求一律提到 bgd_appsdk 看守线程里实现。

## 构建与发布

```bash
cargo build --release
git tag v0.x.0 && git push origin v0.x.0   # CI 注入版本号 → 构建 → 上传 exe + app-release.json
```

- 版本号唯一来源是 git tag（Cargo.toml 固定 `0.0.0-dev`，CI 构建时注入）。
- **本应用无自我更新**：版本更新统一由宿主 bgd_sce_tools 应用市场负责（registry 在 bgd_sce_appsdk，元数据来自本仓库 CI 合成的 app-release.json）。

## CLI 子命令（自测/自动化）

```bash
sce_app_docs serve --project-path <项目根> [--port <端口>] [--lan] [--allowed-hosts <域名>]
                   [--llm-base <URL>] [--llm-key <KEY>] [--llm-model <模型>]
```

前台常驻：构建完成后打印 `OK http://localhost:<port>` 并持续服务（md 变更自动重建 + 索引增量），kill/Ctrl+C 即停（端口随进程释放）；失败 `FAILED <原因>`（exit 1）。LLM 参数继承 GUI 持久化配置、命令行可覆盖（传临时 key 不落盘）。**这就是真实代码路径，AI 改完服务逻辑必须用它自测，不要让用户点 GUI 验证。**

## 测试

```bash
cargo test --lib                              # 单元测试（42 个，含真实模型加载约 15s；无 D:\local_models 时自动跳过模型测试）
cargo test --test lifecycle -- --test-threads=1  # 生命周期 E2E（真实 exe + vitepress build + 索引，约 1 分钟）
node --test test/stream_md.test.mjs           # 流式分段解析器（frontend/stream_md.mjs，零依赖）
# 真实 LLM 链路（可选）：设 BGD_TEST_LLM_BASE/KEY/MODEL 三个环境变量后跑 ask_live_llm_streaming
```

E2E 覆盖：起服务/HTTP 200/前台常驻/kill 即释放端口、md 变更自动重建、切项目 A→B→A 内容正确、端口占用明确报错不漂移、域名白名单 403/放行、**语义检索全生命周期（索引就绪→命中→改 md 增量）**、**问答未配置 400 / 真实 LLM SSE 流式**、**MCP 全握手（initialize 发 session → 通知 202 → tools/list → tools/call → 无 session 拒绝 → GET 405）**。**改服务链路代码后必须两个都跑过才允许提交。**

## 修改守则

- 公共基建（单实例/看守线程/日志/配置/窗口壳）禁止在本仓库重复实现；缺能力先改 bgd_appsdk 并升版本。
- 单文件接近 500 行必须按职责拆分（页面进 `src/ui/`，非 UI 逻辑进 `src/core/` 之类按职责建立的目录）。
- 提交规范：Conventional Commits（`feat: / fix: / docs: / ci: / refactor: / chore:` 前缀，Release notes 依赖）。
