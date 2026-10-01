//! 站点生成：junction 零复制聚合文档源 + index.md 首页 + .vitepress/config.mjs。
//! 纯文件操作，不涉及进程/网络。站点产物在 vitepress_home/_sites/<项目名>-docs/。

use crate::core::config::{self, DocSource, GlobalConfig, ProjectConfig};
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// VitePress 全局安装目录（exe 旁 vitepress_home）
pub fn vitepress_home() -> PathBuf {
    let base = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::env::temp_dir());
    base.join("vitepress_home")
}

/// 站点模板配置（vitepress_home/site_template.json；用户可手改覆盖）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SiteTemplate {
    /// 站点标题
    #[serde(default = "default_title")]
    pub title: String,
    /// 站点描述
    #[serde(default = "default_description")]
    pub description: String,
    /// 语言
    #[serde(default = "default_lang")]
    pub lang: String,
    /// 是否 cleanUrls（VitePress 2.x + junction 需 false）
    #[serde(default)]
    pub clean_urls: bool,
    /// 是否显示最后更新时间
    #[serde(default)]
    pub last_updated: bool,
    /// 搜索 provider（local / algolia / 空 = 关闭）
    #[serde(default = "default_search_provider")]
    pub search_provider: String,
    /// vite resolve.preserveSymlinks（junction 必须 true）
    #[serde(default = "default_true")]
    pub preserve_symlinks: bool,
}

fn default_title() -> String { "本地文档站".into() }
fn default_description() -> String { "项目文档聚合".into() }
fn default_lang() -> String { "zh-CN".into() }
fn default_search_provider() -> String { "local".into() }
fn default_true() -> bool { true }

impl Default for SiteTemplate {
    fn default() -> Self {
        Self {
            title: default_title(),
            description: default_description(),
            lang: default_lang(),
            clean_urls: false,
            last_updated: false,
            search_provider: default_search_provider(),
            preserve_symlinks: true,
        }
    }
}

/// 读站点模板（不存在则写默认）
fn read_site_template(home: &Path) -> SiteTemplate {
    let path = home.join("site_template.json");
    if let Ok(content) = std::fs::read_to_string(&path) {
        if let Ok(t) = serde_json::from_str(&content) {
            return t;
        }
    }
    let t = SiteTemplate::default();
    let _ = std::fs::write(&path, serde_json::to_string_pretty(&t).unwrap_or_default());
    t
}

/// bgd 配置链读 api_generated_dir：项目 bgd.json → 项目 libs/bgd_default.json → tools 安装目录 bgd_default.json
fn read_api_generated_dir(project_root: &Path) -> String {
    // 1. 项目 bgd.json（覆盖项）
    let bgd_json = project_root.join(".bgd/bgd.json");
    if let Ok(content) = std::fs::read_to_string(&bgd_json) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(s) = v.get("api_generated_dir").and_then(|v| v.as_str()) {
                return s.to_string();
            }
        }
    }
    // 2. 项目框架侧默认（.bgd/libs/bgd_default.json）
    let libs_default = project_root.join(".bgd/libs/bgd_default.json");
    if let Ok(content) = std::fs::read_to_string(&libs_default) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(s) = v.get("api_generated_dir").and_then(|v| v.as_str()) {
                return s.to_string();
            }
        }
    }
    // 3. tools 安装目录默认（exe 旁 bgd_default.json；docs 安装在 bgd_sce_tools/apps/docs，
    //    所以 tools 安装目录 = exe 上两级）
    if let Ok(exe) = std::env::current_exe() {
        let tools_dir = exe.parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent());
        if let Some(tools) = tools_dir {
            let tools_default = tools.join("bgd_default.json");
            if let Ok(content) = std::fs::read_to_string(&tools_default) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
                    if let Some(s) = v.get("api_generated_dir").and_then(|v| v.as_str()) {
                        return s.to_string();
                    }
                }
            }
        }
    }
    // 兜底默认值
    ".bgd/doc/api_generated".into()
}

/// 生效源清单：api_generated 约定源（bgd 配置链读取路径）+ 全局/项目合并源（只留真实存在的目录，绝对路径）
pub fn effective_sources(
    project_root: &Path,
    global: &GlobalConfig,
    project: &ProjectConfig,
) -> Vec<(String, PathBuf)> {
    let api_dir = read_api_generated_dir(project_root);
    let mut all = vec![DocSource { name: "api".into(), path: api_dir }];
    all.extend(config::effective_sources(global, project));
    all.into_iter()
        .filter_map(|s| {
            let p = { let x = PathBuf::from(&s.path); if x.is_absolute() { x } else { project_root.join(&s.path) } };
            if p.is_dir() { Some((s.name, p)) } else { None }
        })
        .collect()
}

/// 站点目录名：<项目目录名>-docs（可读，冲突时追加短 hash）
fn site_dir_name(project_root: &Path) -> String {
    let name = project_root.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".into());
    // 项目目录名可能含特殊字符，过滤成合法目录名
    let clean: String = name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
    format!("{clean}-docs")
}

/// 生成站点目录（vitepress_home/_sites/<项目名>-docs/）：
/// junction 聚合源 + index.md 首页 + .vitepress/config.mjs（含主题）。
pub fn write_site_config(
    home: &Path,
    project_root: &Path,
    sources: &[(String, PathBuf)],
) -> std::io::Result<PathBuf> {
    let dir = home.join("_sites").join(site_dir_name(project_root));
    // 站点目录已存在且是属于其他项目的（同名不同路径）→ 追加短 hash 区分
    let dir = if dir.exists() && !dir.join(".project_root").exists() {
        dir
    } else if dir.exists() {
        let marker = dir.join(".project_root");
        let existing = std::fs::read_to_string(&marker).unwrap_or_default();
        if existing.trim() != project_root.display().to_string() {
            let hash = {
                use std::collections::hash_map::DefaultHasher;
                use std::hash::{Hash, Hasher};
                let mut h = DefaultHasher::new();
                project_root.display().to_string().hash(&mut h);
                format!("{:x}", h.finish())
            };
            home.join("_sites").join(format!("{}-{}", site_dir_name(project_root), &hash[..8]))
        } else {
            dir
        }
    } else {
        dir
    };
    std::fs::create_dir_all(&dir)?;
    // 标记项目路径（同名不同项目时区分用）
    let _ = std::fs::write(dir.join(".project_root"), project_root.display().to_string());
    // 清 vite 缓存（vitepress 升级/依赖变化后旧缓存会导致构建异常）
    let _ = std::fs::remove_dir_all(dir.join(".vitepress/cache"));

    // 多源聚合：junction 到站点根。不生成任何文件到源目录（显示工具不该改文档）。
    for (name, src) in sources {
        let dst = dir.join(name);
        let _ = std::fs::remove_dir_all(&dst);
        link_dir(src, &dst)?;
    }
    // 首页：项目根的 md 文件复制到 _root/（junction 整个项目根会被 vitepress 全量扫描，
    // srcExclude 否定模式在 build 下不可靠——实测 _root 页面 404；复制几个小 md 最稳，重建时同步刷新）
    let project_name = project_root.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "项目".into());
    let mut root_mds: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(project_root) {
        root_mds = entries.flatten()
            .filter(|e| e.path().is_file() && e.path().extension().map(|x| x == "md").unwrap_or(false))
            .filter_map(|e| e.file_name().to_string_lossy().strip_suffix(".md").map(|s| s.to_string()))
            .collect();
        root_mds.sort_by_key(|m| {
            let l = m.to_lowercase();
            if l == "index" { 0 } else if l == "readme" { 1 } else if l == "agents" { 2 } else { 3 }
        });
    }
    if !root_mds.is_empty() {
        let root_dir = dir.join("_root");
        let _ = std::fs::remove_dir_all(&root_dir);
        std::fs::create_dir_all(&root_dir)?;
        for m in &root_mds {
            let _ = std::fs::copy(project_root.join(format!("{m}.md")), root_dir.join(format!("{m}.md")));
        }
    }
    // 默认导航页：提示文字 + 项目根 md 文件列表 + 文档源列表 + .bgd 结构图
    let mut nav = format!("# {project_name} 文档\n\n点「左侧导航」展开文档源\n\n");
    if !root_mds.is_empty() {
        for m in &root_mds {
            nav.push_str(&format!("- [{m}](/_root/{m}.md)\n"));
        }
        nav.push_str("\n");
    }
    nav.push_str("## 文档源\n\n");
    for (name, src) in sources {
        if find_home_file(src).is_some() {
            nav.push_str(&format!("- [{name}](/{name}/)\n"));
        } else {
            nav.push_str(&format!("- {name}\n"));
        }
    }
    // 结构图：.bgd 目录文件树（<pre> 标签保留换行，markdown 不解析 pre 内容）
    let bgd_dir = project_root.join(".bgd");
    if bgd_dir.is_dir() {
        nav.push_str("\n## 结构图\n\n<pre class=\"ftree\">\n<span class=\"ft-dir\">.bgd/</span>\n");
        write_file_tree(&bgd_dir, &bgd_dir, "", &mut nav);
        nav.push_str("</pre>\n");
    }
    std::fs::write(dir.join("index.md"), nav)?;

    // VitePress config（从 site_template.json 读取，用户可手改覆盖）
    let tpl = read_site_template(home);
    let cfg = dir.join(".vitepress");
    std::fs::create_dir_all(&cfg)?;
    // rewrites：每个源的首页文件映射成 index.md（VitePress 2.x 不自动识别 README/AGENTS）
    let mut rewrites: Vec<String> = Vec::new();
    let mut sidebar_items: Vec<String> = Vec::new();
    for (name, src) in sources {
        if let Some(home_file) = find_home_file(src) {
            rewrites.push(format!("    '{name}/{home_file}': '{name}/index.md',"));
        }
        let mut items = String::new();
        scan_md_recursive(src, src, name, &mut items);
        let dir_link = if find_home_file(src).is_some() {
            format!(", link: '/{name}/'")
        } else {
            String::new()
        };
        if !items.is_empty() {
            sidebar_items.push(format!("      {{ text: '{name}'{dir_link}, items: [\n{items}      ] }},"));
        } else if find_home_file(src).is_some() {
            sidebar_items.push(format!("      {{ text: '{name}', link: '/{name}/' }},"));
        } else {
            sidebar_items.push(format!("      {{ text: '{name}' }},"));
        }
    }
    let search_block = if tpl.search_provider.is_empty() {
        String::new()
    } else {
        format!("    search: {{ provider: '{}' }},\n", tpl.search_provider)
    };
    // ignoreDeadLinks：文档聚合场景死链是常态（md 互链 .lua/相对路径等），构建不能因此被拦
    // mermaid viewer：```mermaid 代码块 → MermaidViewer 组件（点击开全屏 viewer：缩放/拖拽/下载）；
    // 三件套自注册（插件明确不要 withMermaid 包裹；fork 锁 commit；暗色组件自适配）
    let cfg_text = format!(
        "import {{ defineConfig }} from 'vitepress'\nimport {{ mermaidMarkdown, mermaidPlugin }} from 'vitepress-plugin-mermaid-viewer'\n\nexport default defineConfig({{\n  title: '{title}',\n  description: '{description}',\n  lang: '{lang}',\n  lastUpdated: {last_updated},\n  cleanUrls: {clean_urls},\n  ignoreDeadLinks: true,\n  rewrites: {{\n{rewrites}\n  }},\n  markdown: {{\n    config(md) {{ mermaidMarkdown(md) }},\n  }},\n  themeConfig: {{\n{search_block}    sidebar: [\n{sidebar_items}\n    ],\n  }},\n  vite: {{\n    plugins: [mermaidPlugin()],\n    resolve: {{ preserveSymlinks: {preserve_symlinks} }},\n  }},\n}})\n",
        title = project_name,
        description = tpl.description,
        lang = tpl.lang,
        last_updated = tpl.last_updated,
        clean_urls = tpl.clean_urls,
        rewrites = rewrites.join("\n"),
        search_block = search_block,
        sidebar_items = sidebar_items.join("\n"),
        preserve_symlinks = tpl.preserve_symlinks,
    );
    std::fs::write(cfg.join("config.mjs"), cfg_text)?;
    // 自定义主题：结构图样式走 CSS 变量，跟随 VitePress 明暗主题自动切换；
    // Layout 槽位挂 AI 问答组件（layout-bottom 浮动面板）
    let theme_dir = cfg.join("theme");
    std::fs::create_dir_all(&theme_dir)?;
    // enhanceMermaid 注册 MermaidViewer 组件（viewer 插件官方注册方式，无版本敏感 hack）；
    // 组件内部动态 import mermaid 库，不拖累首开
    std::fs::write(theme_dir.join("index.js"),
        "import DefaultTheme from 'vitepress/theme'\nimport { h } from 'vue'\nimport AiChat from './AiChat.vue'\nimport { enhanceMermaid } from 'vitepress-plugin-mermaid-viewer/client'\nimport 'vitepress-plugin-mermaid-viewer/client.css'\nimport './custom.css'\nexport default {\n  extends: DefaultTheme,\n  Layout: () => h(DefaultTheme.Layout, null, { 'layout-bottom': () => h(AiChat) }),\n  enhanceApp({ app }) { enhanceMermaid(app) }\n}\n")?;
    std::fs::write(theme_dir.join("custom.css"),
        "/* 结构图：无背景色，目录/连接线颜色跟随明暗主题 */\n.ftree {\n  font-family: var(--vp-font-family-mono);\n  font-size: 13px;\n  line-height: 1.6;\n  padding: 0;\n  margin: 0;\n  overflow-x: auto;\n  background: transparent;\n}\n.ft-dir { color: var(--vp-c-text-1); font-weight: bold; }\n.ft-line { color: var(--vp-c-text-3); }\n")?;
    std::fs::write(theme_dir.join("AiChat.vue"), crate::core::site_templates::AI_CHAT_VUE)?;
    Ok(dir)
}

/// 文件类型颜色（结构图用；VitePress 支持内联 HTML）
fn file_type_color(fname: &str) -> &'static str {
    let ext = fname.rsplit('.').next().unwrap_or("");
    match ext {
        "md" => "#42b883",      // Vue 绿
        "lua" => "#51a0cf",     // Lua 蓝
        "json" => "#f1e05a",    // JSON 黄
        "toml" => "#9c4221",    // TOML 橙
        "yml" | "yaml" => "#cb171e", // YAML 红
        "ts" | "tsx" => "#3178c6",   // TS 蓝
        "js" | "jsx" => "#f7df1e",   // JS 黄
        "rs" => "#dea584",      // Rust 橙
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" => "#a074c4", // 图片紫
        "txt" | "log" => "#8b949e", // 文本灰
        _ => "#c9d1d9",         // 默认灰白
    }
}

/// 递归写文件树（结构图用；树形缩进线，文件类型着色；white-space: pre 不用 <br>）
fn write_file_tree(_base: &Path, dir: &Path, prefix: &str, out: &mut String) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut items: Vec<_> = entries.flatten()
        .filter(|e| {
            let fname = e.file_name().to_string_lossy().to_string();
            !fname.starts_with('.') && fname != "node_modules" && fname != "target"
        })
        .collect();
    items.sort_by_key(|e| (e.path().is_file(), e.file_name())); // 目录在前，文件在后
    let count = items.len();
    for (i, entry) in items.into_iter().enumerate() {
        let path = entry.path();
        let fname = entry.file_name().to_string_lossy().to_string();
        let is_last = i == count - 1;
        let connector = if is_last { "└── " } else { "├── " };
        let line = format!("<span class=\"ft-line\">{prefix}{connector}</span>");
        if path.is_dir() {
            out.push_str(&format!("{line}<span class=\"ft-dir\">{fname}/</span>\n"));
            let child_prefix = format!("{prefix}{}", if is_last { "    " } else { "│   " });
            write_file_tree(_base, &path, &child_prefix, out);
        } else {
            let color = file_type_color(&fname);
            out.push_str(&format!("{line}<span style=\"color:{color}\">{fname}</span>\n"));
        }
    }
}

/// 目录首页文件查找：index.md > README.md > AGENTS.md（不区分大小写）
fn find_home_file(dir: &Path) -> Option<String> {
    let candidates = ["index.md", "README.md", "AGENTS.md"];
    for c in candidates {
        if dir.join(c).exists() {
            return Some(c.to_string());
        }
    }
    // 不区分大小写兜底（INDEX.md / Readme.md / Agents.md 等）
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let fname = e.file_name().to_string_lossy().to_lowercase();
            if fname == "index.md" || fname == "readme.md" || fname == "agents.md" {
                return Some(e.file_name().to_string_lossy().to_string());
            }
        }
    }
    None
}

/// 递归扫描 md 文件生成 sidebar 项（多级目录嵌套，目录无首页时纯分组不跳转）
fn scan_md_recursive(base: &Path, dir: &Path, source_name: &str, out: &mut String) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<_> = entries.flatten().collect();
    files.sort_by_key(|e| e.file_name());
    for entry in files {
        let path = entry.path();
        let fname = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            let mut sub_items = String::new();
            scan_md_recursive(base, &path, source_name, &mut sub_items);
            if !sub_items.is_empty() {
                let rel = path.strip_prefix(base).unwrap_or(&path).display().to_string().replace('\\', "/");
                let dir_link = if find_home_file(&path).is_some() {
                    format!(", link: '/{source_name}/{rel}/'")
                } else {
                    String::new()
                };
                out.push_str(&format!("        {{ text: '{fname}'{dir_link}, collapsed: true, items: [\n{sub_items}        ] }},\n"));
            }
        } else if path.extension().map(|e| e == "md").unwrap_or(false) {
            let stem = fname.strip_suffix(".md").unwrap_or(&fname);
            // 首页文件不进 sidebar（作为目录索引页，不占导航位）
            let lower = stem.to_lowercase();
            if lower == "readme" || lower == "index" || lower == "agents" { continue; }
            let rel = path.strip_prefix(base).unwrap_or(&path).display().to_string().replace('\\', "/");
            let link = format!("/{source_name}/{}", rel.strip_suffix(".md").unwrap_or(&rel));
            out.push_str(&format!("        {{ text: '{stem}', link: '{link}' }},\n"));
        }
    }
}

/// 目录链接（junction 优先，失败降级 symlink）
#[cfg(windows)]
fn link_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    let dst_s = dst.display().to_string().replace('/', "\\");
    let src_s = src.display().to_string().replace('/', "\\");
    let status = Command::new("cmd")
        .args(["/C", "mklink", "/J", &dst_s, &src_s])
        .creation_flags(CREATE_NO_WINDOW)
        .output()?;
    if status.status.success() {
        Ok(())
    } else {
        std::os::windows::fs::symlink_dir(src, dst)
    }
}

#[cfg(not(windows))]
fn link_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_project(tag: &str) -> PathBuf {
        let tmp = std::env::temp_dir().join(format!("bgd_docs_site_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join(".bgd/doc/api_generated")).unwrap();
        std::fs::create_dir_all(tmp.join(".bgd/doc/research")).unwrap();
        tmp
    }

    #[test]
    fn effective_sources_resolves_existing_only() {
        let tmp = setup_project("src");
        let g = GlobalConfig {
            sources: vec![
                DocSource { name: "research".into(), path: ".bgd/doc/research".into() },
                DocSource { name: "missing".into(), path: ".bgd/doc/no_such".into() },
            ],
            ..Default::default()
        };
        let p = ProjectConfig::default();
        let srcs = effective_sources(&tmp, &g, &p);
        let names: Vec<&str> = srcs.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"api"));
        assert!(names.contains(&"research"));
        assert!(!names.contains(&"missing"));
        for (_, p) in &srcs {
            assert!(p.is_absolute() && p.is_dir());
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn api_generated_dir_reads_bgd_chain() {
        let tmp = setup_project("bgd");
        std::fs::write(tmp.join(".bgd/bgd.json"), r#"{"api_generated_dir": "custom/api"}"#).unwrap();
        assert_eq!(read_api_generated_dir(&tmp), "custom/api");
        let _ = std::fs::remove_file(tmp.join(".bgd/bgd.json"));
        std::fs::create_dir_all(tmp.join(".bgd/libs")).unwrap();
        std::fs::write(tmp.join(".bgd/libs/bgd_default.json"), r#"{"api_generated_dir": "libs/api"}"#).unwrap();
        assert_eq!(read_api_generated_dir(&tmp), "libs/api");
        let _ = std::fs::remove_file(tmp.join(".bgd/libs/bgd_default.json"));
        assert_eq!(read_api_generated_dir(&tmp), ".bgd/doc/api_generated");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn site_dir_name_readable() {
        let p = PathBuf::from("D:/projects/The_Shadow_of_Return");
        assert_eq!(site_dir_name(&p), "The_Shadow_of_Return-docs");
    }

    #[test]
    fn site_config_links_sources() {
        let tmp = setup_project("cfg");
        let home = std::env::temp_dir().join(format!("bgd_docs_home_site_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let sources = effective_sources(&tmp, &GlobalConfig::default(), &ProjectConfig::default());
        let dir = write_site_config(&home, &tmp, &sources).unwrap();
        assert!(dir.join(".vitepress/config.mjs").is_file());
        // 构建模式必须有 ignoreDeadLinks（聚合场景死链是常态）
        let cfg = std::fs::read_to_string(dir.join(".vitepress/config.mjs")).unwrap();
        assert!(cfg.contains("ignoreDeadLinks: true"), "config 必须含 ignoreDeadLinks: true");
        assert!(dir.join("index.md").is_file());
        assert!(dir.join("api").exists()); // junction 到 api_generated
        assert!(dir.join(".project_root").is_file());
        assert!(dir.file_name().unwrap().to_string_lossy().ends_with("-docs"));
        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// mermaid 接入（viewer 版）：config 自注册 markdown helper + vite 插件；主题 enhanceMermaid 注册组件
    ///（viewer 插件明确不用 withMermaid 包裹，三件套自管，无版本敏感 hack）
    #[test]
    fn site_config_has_mermaid() {
        let tmp = setup_project("mmd");
        let home = std::env::temp_dir().join(format!("bgd_docs_home_mmd_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let sources = effective_sources(&tmp, &GlobalConfig::default(), &ProjectConfig::default());
        let dir = write_site_config(&home, &tmp, &sources).unwrap();
        let cfg = std::fs::read_to_string(dir.join(".vitepress/config.mjs")).unwrap();
        assert!(cfg.contains("mermaidMarkdown"), "config 必须注册 markdown helper：{cfg}");
        assert!(cfg.contains("mermaidPlugin"), "config 必须注册 vite 插件：{cfg}");
        let theme = std::fs::read_to_string(dir.join(".vitepress/theme/index.js")).unwrap();
        assert!(theme.contains("enhanceMermaid"), "主题必须注册 Mermaid 组件：{theme}");
        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&home);
    }
}
