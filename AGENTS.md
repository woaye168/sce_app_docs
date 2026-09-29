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
  site.rs              # 站点生成：junction 聚合源 + index.md 导航页 + config.mjs（纯文件操作）
  builder.rs           # vitepress build 短命 spawn（Job Object KILL_ON_JOB_CLOSE 兜底防孤儿）
  httpd.rs             # 内嵌静态 HTTP 服务（tiny_http；Host 白名单；缓存策略）
  watcher.rs           # notify 监听 md 变更（真实路径，不经 junction）
  service.rs           # 服务编排：状态机 Idle/Building/Serving/Failed + debounce 重建 + dist 原子翻转
src/ui/                # main_page.rs / settings.rs（impl App 分散定义）
test/lifecycle.rs      # 生命周期 E2E 测试（黑盒 spawn 真实 exe serve；见「测试」节）
app.json               # 应用市场静态元数据（不含版本；CI 合成 app-release.json）
.github/workflows/release.yml  # tag 触发构建发布
doc/research/          # 设计文档（架构重构方案等）
```

## 架构要点（v0.2 重构，2026-09-29）

**静态构建 + 内嵌服务**：vitepress 只在「构建」时短命 spawn（起→跑→自己退出，Job Object 双保险）；HTTP 服务由 exe 内嵌（tiny_http），端口随进程生命周期——无孤儿进程、无句柄丢失、无端口漂移。md 变更经 watcher debounce 1s 后重建（dist-a/dist-b 轮换 + 原子翻转，重建期间旧内容不掉线），手动刷新可见。

**禁止**回到「外部长寿 dev server 进程」架构（病史见 doc/research/2026-09-29-架构重构方案）。

**Windows 端口陷阱**：tiny_http::Server::http 的 bind 带 SO_REUSEADDR 语义能被挤占同端口——必须 std TcpListener 先 bind（默认独占）再 from_listener；lan 模式绑 0.0.0.0 前要先探测 127.0.0.1（wildcard 与具体地址 bind 可共存，具体地址抢流量）。

**CLI 确定性**：serve 的 lan/白名单只看命令行参数，不继承 GUI 持久化配置（防脏配置污染自测）。

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
```

前台常驻：构建完成后打印 `OK http://localhost:<port>` 并持续服务（md 变更自动重建），kill/Ctrl+C 即停（端口随进程释放）；失败 `FAILED <原因>`（exit 1）。**这就是真实代码路径，AI 改完服务逻辑必须用它自测，不要让用户点 GUI 验证。**

## 测试

```bash
cargo test --lib                              # 单元测试（20 个，秒级）
cargo test --test lifecycle -- --test-threads=1  # 生命周期 E2E（真实 exe + vitepress build，约 30s）
```

E2E 覆盖：起服务/HTTP 200/前台常驻/kill 即释放端口、md 变更自动重建、切项目 A→B→A 内容正确、端口占用明确报错不漂移、域名白名单 403/放行。**改服务链路代码后必须两个都跑过才允许提交。**

## 修改守则

- 公共基建（单实例/看守线程/日志/配置/窗口壳）禁止在本仓库重复实现；缺能力先改 bgd_appsdk 并升版本。
- 单文件接近 500 行必须按职责拆分（页面进 `src/ui/`，非 UI 逻辑进 `src/core/` 之类按职责建立的目录）。
- 提交规范：Conventional Commits（`feat: / fix: / docs: / ci: / refactor: / chore:` 前缀，Release notes 依赖）。
