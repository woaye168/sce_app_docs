# sce_app_docs · 本地文档站

星火编辑器（SCE）项目的本地文档聚合阅读工具：一个 egui 桌面应用，内嵌 VitePress dev server，把项目的框架 API 文档（api_generated）与自定义文档源聚合成一个带左导航、全文搜索的文档站，浏览器阅读体验。

## 功能特性

- **零复制聚合**：文档源通过 Windows junction 原地引用，改源文件实时生效，不做任何文件拷贝
- **多文档源**：框架 API 文档 + `.bgd/docs.json` 配置的任意多个自定义源目录
- **左侧导航**：按目录结构多级递归生成 sidebar；目录含 index/README/AGENTS 时作为目录首页，否则纯分组
- **全文搜索**：VitePress 本地搜索，无需外部服务
- **智能首页**：项目根有 index/README/AGENTS 时直接作为站点首页；否则生成导航页（项目 md 列表 + 文档源列表 + `.bgd` 结构图）
- **明暗主题**：结构图与文档站整体跟随 VitePress 明暗主题自动切换
- **配置化**：站点模板（`vitepress_home/site_template.json`）可手改覆盖标题/搜索/链接风格等

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
- **文档渲染**：VitePress 2.x（Vite + Vue 3），dev server 模式，全局安装于应用旁 `vitepress_home/`
- **目录链接**：Windows junction（无需管理员特权）

## 从源码构建

```bash
cargo build --release
```

## 发布

版本号唯一来源是 git tag；打 tag 触发 CI 构建并上传 exe 与 `app-release.json`，宿主应用市场负责分发更新。

```bash
git tag v0.x.0 && git push origin v0.x.0
```
