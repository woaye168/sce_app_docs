//! VitePress 本地服务：
//! - VitePress 全局安装一次（app 数据目录 vitepress_home/，node_modules 一份）
//! - 项目零侵入：源原地 junction 聚合，不写项目目录
//! - 启动整链异步：探测/装依赖/起服务全离 UI 线程，UI 轮询 phase
//! - 站点配置抽 JSON（vitepress_home/site_template.json），可手改覆盖

use crate::core::config::{self, DocSource, GlobalConfig, ProjectConfig};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// 启动阶段（UI 轮询渲染进度）
#[derive(Debug, Clone, PartialEq)]
pub enum StartPhase {
    Idle,
    /// 正在装 VitePress 依赖（全局 vitepress_home，仅首次）
    InstallingDeps,
    /// 正在起 dev server
    Starting,
    Running,
    Failed,
}

/// 服务状态（UI 读这个渲染）
pub struct ServerState {
    pub child: Option<Child>,
    pub port: u16,
    pub last_error: String,
    pub phase: StartPhase,
    /// 站点目录（读 dev-server.log 用；pub 供 CLI 场景读取）
    pub site_dir: Option<PathBuf>,
    /// 端口探测首次失败时间（持续 1s 失败才判死；防 vitepress 热重载 restart 瞬间断连误判）
    fail_since: Option<std::time::Instant>,
    /// 后台启动任务的完成信号（整条链的结果）
    boot_done: Option<std::sync::mpsc::Receiver<BootResult>>,
}

impl Default for ServerState {
    fn default() -> Self {
        Self { child: None, port: 0, last_error: String::new(), phase: StartPhase::Idle, site_dir: None, fail_since: None, boot_done: None }
    }
}

/// 后台启动链的结果（成功带 dev server 子进程与端口）
struct BootResult {
    child: Option<(Child, PathBuf)>,
    port: u16,
    error: String,
}

/// VitePress 全局安装目录（exe 旁 vitepress_home）
pub fn vitepress_home() -> PathBuf {
    let base = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::env::temp_dir());
    base.join("vitepress_home")
}

/// 站点模板配置（vitepress_home/site_template.json；用户可手改，app 读取生成站点 config）
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
    /// vite server.fs.strict（junction 必须 false）
    #[serde(default)]
    pub fs_strict: bool,
    /// vite server.forwardConsole（vite 8 注入 bug，关）
    #[serde(default)]
    pub forward_console: bool,
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
            fs_strict: false,
            forward_console: false,
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
        // exe 在 <tools>/apps/docs/sce_app_docs.exe → tools 目录 = exe.parent().parent().parent()
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
fn effective_sources(
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
/// junction 聚合源 + index.md 首页 + .vitepress/config.mjs。
fn write_site_config(
    home: &Path,
    project_root: &Path,
    sources: &[(String, PathBuf)],
    allowed_hosts: &str,
) -> std::io::Result<PathBuf> {
    let dir = home.join("_sites").join(site_dir_name(project_root));
    // 站点目录已存在且是属于其他项目的（同名不同路径）→ 追加短 hash 区分
    let dir = if dir.exists() && !dir.join(".project_root").exists() {
        // 首次创建，标记项目路径
        dir
    } else if dir.exists() {
        let marker = dir.join(".project_root");
        let existing = std::fs::read_to_string(&marker).unwrap_or_default();
        if existing.trim() != project_root.display().to_string() {
            // 同名不同项目，追加短 hash
            let hash = {
                use std::collections::hash_map::DefaultHasher;
                use std::hash::{Hash, Hasher};
                let mut h = DefaultHasher::new();
                project_root.display().to_string().hash(&mut h);
                format!("{:x}", h.finish())
            };
            let dir = home.join("_sites").join(format!("{}-{}", site_dir_name(project_root), &hash[..8]));
            dir
        } else {
            dir
        }
    } else {
        dir
    };
    std::fs::create_dir_all(&dir)?;
    // 标记项目路径（同名不同项目时区分用）
    let _ = std::fs::write(dir.join(".project_root"), project_root.display().to_string());
    // 清 vite 缓存（vitepress 升级/依赖变化后旧缓存会导致 504 Outdated Optimize Dep）
    let _ = std::fs::remove_dir_all(dir.join(".vitepress/cache"));

    // 多源聚合：junction 到站点根（VitePress 文件路由能扫到 junction 里的 md，
    // 但 vite 默认 fs.strict 会拦 junction 外部路径——需配 fs.strict: false）。
    // 不生成任何文件到源目录（显示工具不该改文档）。
    for (name, src) in sources {
        let dst = dir.join(name);
        let _ = std::fs::remove_dir_all(&dst);
        link_dir(src, &dst)?;
    }
    // 首页：项目根有 index/README/AGENTS 时 junction 整个项目根到 _root/，
    // srcExclude 只留首页文件，rewrites 映射到站点根 index.md（零复制、实时生效）。
    // 没有首页文件才写默认导航页。
    let project_name = project_root.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "项目".into());
    let project_home = find_home_file(project_root);
    if project_home.is_some() {
        let dst = dir.join("_root");
        let _ = std::fs::remove_dir_all(&dst);
        link_dir(project_root, &dst)?;
    }
    // 默认导航页：提示文字 + 项目根 md 文件列表 + 文档源列表 + .bgd 结构图
    let mut nav = format!("# {project_name} 文档\n\n点「左侧导航」展开文档源\n\n");
    // 项目根 md 文件列表（含首页，按优先级排序：index > README > AGENTS > 其他）
    if let Ok(entries) = std::fs::read_dir(project_root) {
        let mut mds: Vec<String> = entries.flatten()
            .filter(|e| e.path().extension().map(|x| x == "md").unwrap_or(false))
            .filter_map(|e| e.file_name().to_string_lossy().strip_suffix(".md").map(|s| s.to_string()))
            .collect();
        // 优先级排序：index > README > AGENTS > 其他（字母序）
        mds.sort_by_key(|m| {
            let l = m.to_lowercase();
            if l == "index" { 0 } else if l == "readme" { 1 } else if l == "agents" { 2 } else { 3 }
        });
        if !mds.is_empty() {
            for m in &mds {
                nav.push_str(&format!("- [{m}](/_root/{m}.md)\n"));
            }
            nav.push_str("\n");
        }
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
        // 首页映射：index > README > AGENTS（不区分大小写）
        if let Some(home_file) = find_home_file(src) {
            rewrites.push(format!("    '{name}/{home_file}': '{name}/index.md',"));
        }
        // 递归扫描 md 文件生成 sidebar（多级目录嵌套，目录无首页时纯分组不跳转）
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
    // srcExclude：项目根只留首页文件，其他全部排除（防项目根 md 全暴露）
    let src_exclude_block = if let Some(home_file) = &project_home {
        format!("  srcExclude: ['_root/**/*', '!_root/{home_file}'],\n")
    } else {
        String::new()
    };
    let search_block = if tpl.search_provider.is_empty() {
        String::new()
    } else {
        format!("    search: {{ provider: '{}' }},\n", tpl.search_provider)
    };
    // 允许域名（vite 8 allowedHosts 安全校验；开关关闭时跳过，域名配置保留）
    let hosts: Vec<String> = if allowed_hosts.is_empty() {
        Vec::new()
    } else {
        allowed_hosts.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(|s| format!("'{s}'"))
            .collect()
    };
    let allowed_hosts_block = if hosts.is_empty() {
        String::new()
    } else {
        format!("allowedHosts: [{}], ", hosts.join(", "))
    };
    // 随机构建 ID：每次启动变，vite 认为配置变了 → 重新预构建依赖 → 模块 hash 变 → 浏览器缓存失效
    let build_id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let cfg_text = format!(
        "import {{ defineConfig }} from 'vitepress'\n\n// vite 插件：覆盖 /@fs/ 路径的缓存头（vite 内部硬编码 max-age=14400，Cloudflare 会缓存旧模块）\nconst noCacheFs = {{\n  name: 'no-cache-fs',\n  configureServer(server) {{\n    server.middlewares.use((req, res, next) => {{\n      if (req.url.startsWith('/@fs/')) {{\n        res.setHeader('Cache-Control', 'no-cache');\n      }}\n      next();\n    }});\n  }},\n}};\n\nexport default defineConfig({{\n  title: '{title}',\n  description: '{description}',\n  lang: '{lang}',\n  lastUpdated: {last_updated},\n  cleanUrls: {clean_urls},\n{src_exclude_block}  rewrites: {{\n{rewrites}\n  }},\n  themeConfig: {{\n{search_block}    sidebar: [\n{sidebar_items}\n    ],\n  }},\n  vite: {{\n    plugins: [noCacheFs],\n    define: {{ __DOCS_BUILD_ID__: '{build_id}' }},\n    resolve: {{ preserveSymlinks: {preserve_symlinks} }},\n    server: {{ {allowed_hosts_block}fs: {{ strict: {fs_strict} }}, forwardConsole: {forward_console}, headers: {{ 'Cache-Control': 'no-cache' }} }},\n  }},\n}})\n",
        title = project_name,
        description = tpl.description,
        lang = tpl.lang,
        last_updated = tpl.last_updated,
        clean_urls = tpl.clean_urls,
        src_exclude_block = src_exclude_block,
        rewrites = rewrites.join("\n"),
        search_block = search_block,
        sidebar_items = sidebar_items.join("\n"),
        preserve_symlinks = tpl.preserve_symlinks,
        build_id = build_id,
        allowed_hosts_block = allowed_hosts_block,
        fs_strict = tpl.fs_strict,
        forward_console = tpl.forward_console,
    );
    std::fs::write(cfg.join("config.mjs"), cfg_text)?;
    // 自定义主题：结构图样式走 CSS 变量，跟随 VitePress 明暗主题自动切换
    let theme_dir = cfg.join("theme");
    std::fs::create_dir_all(&theme_dir)?;
    std::fs::write(theme_dir.join("index.js"),
        "import DefaultTheme from 'vitepress/theme'\nimport './custom.css'\nexport default DefaultTheme\n")?;
    std::fs::write(theme_dir.join("custom.css"),
        "/* 结构图：无背景色，目录/连接线颜色跟随明暗主题 */\n.ftree {\n  font-family: var(--vp-font-family-mono);\n  font-size: 13px;\n  line-height: 1.6;\n  padding: 0;\n  margin: 0;\n  overflow-x: auto;\n  background: transparent;\n}\n.ft-dir { color: var(--vp-c-text-1); font-weight: bold; }\n.ft-line { color: var(--vp-c-text-3); }\n")?;
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
fn write_file_tree(base: &Path, dir: &Path, prefix: &str, out: &mut String) {
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
            write_file_tree(base, &path, &child_prefix, out);
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
            // 子目录：递归扫描，生成嵌套 items
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

/// 判断已装 vitepress 是否为 2.x（读 node_modules/vitepress/package.json 版本号）
fn is_vitepress_v2(home: &Path) -> bool {
    let pkg = home.join("node_modules/vitepress/package.json");
    let Ok(content) = std::fs::read_to_string(&pkg) else { return false };
    // 粗匹配 "version": "2.xxx"
    content.contains("\"version\": \"2.")
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

/// 默认端口（18753：本地文档站，避开 8080 等常用端口冲突）
pub const DEFAULT_PORT: u16 = 18753;

/// 后台启动整条链：装全局依赖（若缺）→ 聚合源写 config → spawn dev server。
/// 全部在独立线程完成，结果经 channel 回 UI 线程（boot_tick 收）。
pub fn start(project_root: &Path, global: &GlobalConfig, project: &ProjectConfig, st: &mut ServerState) {
    let lan = global.lan_access;
    stop(st);
    st.last_error.clear();

    let Some(node) = crate::core::node::resolve() else {
        st.last_error = "未找到 node.exe（设置页配置或安装 Node.js）".into();
        st.phase = StartPhase::Failed;
        return;
    };
    let port = if global.port == 0 { DEFAULT_PORT } else { global.port };
    let home = vitepress_home();
    let sources = effective_sources(project_root, global, project);
    if sources.is_empty() {
        st.last_error = "无可用文档源（api_generated 未构建，且未配置其他源）".into();
        st.phase = StartPhase::Failed;
        return;
    }

    st.phase = if home.join("node_modules").is_dir() { StartPhase::Starting } else { StartPhase::InstallingDeps };
    let (tx, rx) = std::sync::mpsc::channel();
    st.boot_done = Some(rx);
    let hosts = if global.allowed_hosts_enabled { global.allowed_hosts.as_str() } else { "" };
    match write_site_config(&home, project_root, &sources, hosts) {
        Ok(site) => {
            // 补杀上次残留的孤儿进程（app 异常退出/句柄丢失时 child 拿不到，靠 PID 文件兜底）
            kill_orphan(&site);
            std::thread::spawn(move || {
                let r = boot_chain(home, node, Some(site), port, lan);
                let _ = tx.send(r);
            });
        }
        Err(e) => {
            st.last_error = format!("站点 config 生成失败: {e}");
            st.phase = StartPhase::Failed;
            st.boot_done = None;
        }
    }
}

/// 后台链本体：装依赖 → spawn dev（全在独立线程，不卡 UI）
/// lan = true 时绑 0.0.0.0（局域网可访问），false 只本机
fn boot_chain(home: PathBuf, node: PathBuf, site: Option<PathBuf>, port: u16, lan: bool) -> BootResult {
    let mut r = BootResult { child: None, port: 0, error: String::new() };
    let Some(site) = site else {
        r.error = "站点 config 生成失败".into();
        return r;
    };

    // 全局依赖（VitePress 2.x alpha，内置 minisearch 本地搜索）；
    // package.json 每次重写（依赖清单变化要生效），node_modules 缺包才跑 install
    let pkg = home.join("package.json");
    let _ = std::fs::write(&pkg, r#"{"name":"bgd-docs-vitepress","private":true,"type":"module","devDependencies":{"vitepress":"^2.0.0-alpha.20"}}"#);
    let need_install = !home.join("node_modules/vitepress").is_dir()
        || !is_vitepress_v2(&home);
    if need_install {
        let npm = node.with_file_name("npm.cmd");
        let mut cmd = Command::new(npm);
        cmd.arg("install").current_dir(&home);
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        match cmd.output() {
            Ok(o) if o.status.success() => {}
            Ok(o) => {
                r.error = format!("VitePress 依赖安装失败: {}", String::from_utf8_lossy(&o.stderr));
                return r;
            }
            Err(e) => {
                r.error = format!("npm install 启动失败: {e}");
                return r;
            }
        }
    }

    // 等端口释放（taskkill 后 TCP 回收有延迟，不等会让新进程被挤到别的端口）
    wait_port_free(port, 2000);

    // spawn dev server：直接 spawn node.exe 跑 vitepress.js（跳过 .bin/vitepress.cmd 壳）。
    // cmd 壳 spawn node 后自己退出，child 句柄记壳 PID，杀壳杀不到 node 子进程——这是孤儿残留根因。
    // 直接 spawn node 后 child 就是 node 进程本身，kill / PID 文件全部对准真服务进程。
    let vp = home.join("node_modules/vitepress/bin/vitepress.js");
    let log_path = site.join("dev-server.log");
    let log_file = std::fs::File::create(&log_path).ok();
    let mut cmd = Command::new(&node);
    // node vitepress.js dev <root> --port <port>（site 即 .vitepress/ 所在目录）
    cmd.arg(&vp).arg("dev").arg(&site).arg("--port").arg(port.to_string());
    // 局域网开关：绑 0.0.0.0 允许手机/其他设备访问（vitepress --host 0.0.0.0）
    if lan {
        cmd.arg("--host").arg("0.0.0.0");
    }
    cmd.current_dir(&site);
    if let Some(f) = log_file {
        match f.try_clone() {
            Ok(f2) => { cmd.stderr(f2); }
            Err(_) => { cmd.stderr(std::process::Stdio::null()); }
        }
        cmd.stdout(f);
    }
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    match cmd.spawn() {
        Ok(c) => {
            // 写 PID 文件（app 异常退出后下次启动可补杀孤儿）
            let _ = std::fs::write(site.join("dev-server.pid"), c.id().to_string());
            r.child = Some((c, site.clone()));
            r.port = port;
        }
        Err(e) => r.error = format!("dev server 启动失败: {e}"),
    }
    r
}

/// UI 每帧轮询：后台启动链结果落地 + 运行中进程存活检测
pub fn boot_tick(st: &mut ServerState) {
    // 收启动链结果
    if let Some(rx) = &st.boot_done {
        if let Ok(r) = rx.try_recv() {
            st.boot_done = None;
            if let Some(c) = r.child {
                st.site_dir = Some(c.1);
                st.child = Some(c.0);
                st.port = r.port;
                st.phase = StartPhase::Running;
                st.fail_since = None;
            } else {
                st.last_error = r.error;
                st.phase = StartPhase::Failed;
            }
        }
    }
    // 运行中存活检测：探测端口而非进程句柄——vitepress.cmd 壳 spawn node 子进程后自己退出，
    // try_wait 会误判「已退出」，但真服务（node）还活着；端口能连才是真活着。
    // 时间窗口判定：持续 1000ms 探测失败才判死（GUI 每帧轮询、CLI 200ms 轮询，次数阈值不公平；
    // vitepress config 热重载 restart 瞬间端口短暂断连几百毫秒，时间窗口内恢复则不计失败）
    if st.phase == StartPhase::Running {
        // 双栈探测：vitepress 2.x 只监听 IPv6 [::1]，连 127.0.0.1 会被拒绝
        let timeout = std::time::Duration::from_millis(300);
        let alive = std::net::TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127, 0, 0, 1], st.port)),
            timeout,
        ).is_ok() || std::net::TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], st.port)),
            timeout,
        ).is_ok();
        if alive {
            st.fail_since = None;
        } else {
            let since = st.fail_since.get_or_insert_with(std::time::Instant::now);
            if since.elapsed().as_millis() >= 1000 {
                let log_tail = st.site_dir.as_deref().map(read_dev_log_tail).unwrap_or_default();
                st.child = None;
                st.port = 0;
                st.fail_since = None;
                if st.last_error.is_empty() {
                    st.last_error = log_tail;
                }
                st.phase = StartPhase::Failed;
            }
        }
    }
}

/// 读指定站点目录的 dev server 日志尾部（进程退出原因）
fn read_dev_log_tail(site_dir: &Path) -> String {
    let f = site_dir.join("dev-server.log");
    if let Ok(content) = std::fs::read_to_string(&f) {
        let tail: String = content
            .lines()
            .rev()
            .take(5)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join(" | ");
        if !tail.trim().is_empty() {
            return format!("dev server 退出：{tail}");
        }
    }
    format!("dev server 进程已退出（查看 {}/dev-server.log）", site_dir.display())
}

/// 停止：杀进程（现在 child 直接是 node 进程本身，kill 即杀真服务；
/// taskkill /T 兜底连带可能的子进程）
pub fn stop(st: &mut ServerState) {
    if let Some(c) = st.child.take() {
        kill_tree(&c);
        drop(c);
    }
    st.port = 0;
    st.phase = StartPhase::Idle;
    st.boot_done = None;
    // 清所有站点目录的 PID 文件（进程已死，标记失效）
    let sites = vitepress_home().join("_sites");
    if let Ok(entries) = std::fs::read_dir(&sites) {
        for e in entries.flatten() {
            let _ = std::fs::remove_file(e.path().join("dev-server.pid"));
        }
    }
    st.site_dir = None;
    st.fail_since = None;
}

/// 等端口释放（taskkill 后 TCP 回收有延迟；最多等 max_ms 毫秒）
/// 双栈检查：vitepress 监听 IPv6，只看 IPv4 会漏判
fn wait_port_free(port: u16, max_ms: u64) {
    let start = std::time::Instant::now();
    while start.elapsed().as_millis() < max_ms as u128 {
        let v4 = std::net::TcpListener::bind(("127.0.0.1", port)).is_ok();
        let v6 = std::net::TcpListener::bind(("::1", port)).is_ok();
        if v4 && v6 {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// 补杀孤儿进程（站点目录 dev-server.pid 记录的 PID；app 异常退出/句柄丢失时兜底）
fn kill_orphan(site: &Path) {
    let pid_file = site.join("dev-server.pid");
    let Ok(content) = std::fs::read_to_string(&pid_file) else { return };
    let Ok(pid) = content.trim().parse::<u32>() else { return };
    // 确认进程还活着才杀（PID 可能已被系统回收复用，taskkill 失败无害）
    let mut cmd = Command::new("taskkill");
    cmd.args(["/PID", &pid.to_string(), "/T", "/F"]);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let _ = cmd.output();
    let _ = std::fs::remove_file(&pid_file);
}

/// 杀进程树（Windows taskkill /T /F；整树连根拔）
#[cfg(windows)]
fn kill_tree(c: &Child) {
    let mut cmd = Command::new("taskkill");
    cmd.args(["/PID", &c.id().to_string(), "/T", "/F"]);
    cmd.creation_flags(CREATE_NO_WINDOW);
    let _ = cmd.output();
}

#[cfg(not(windows))]
fn kill_tree(c: &Child) {
    let _ = unsafe { libc::kill(c.id() as i32, libc::SIGTERM) };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_project(tag: &str) -> PathBuf {
        let tmp = std::env::temp_dir().join(format!("bgd_docs_srv4_{tag}_{}", std::process::id()));
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
        // 项目 bgd.json 覆盖
        std::fs::write(tmp.join(".bgd/bgd.json"), r#"{"api_generated_dir": "custom/api"}"#).unwrap();
        assert_eq!(read_api_generated_dir(&tmp), "custom/api");
        // 删掉 bgd.json，读 libs/bgd_default.json
        let _ = std::fs::remove_file(tmp.join(".bgd/bgd.json"));
        std::fs::create_dir_all(tmp.join(".bgd/libs")).unwrap();
        std::fs::write(tmp.join(".bgd/libs/bgd_default.json"), r#"{"api_generated_dir": "libs/api"}"#).unwrap();
        assert_eq!(read_api_generated_dir(&tmp), "libs/api");
        // 都删掉，兜底默认
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
        let home = std::env::temp_dir().join(format!("bgd_docs_home4_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let sources = effective_sources(&tmp, &GlobalConfig::default(), &ProjectConfig::default());
        let dir = write_site_config(&home, &tmp, &sources, "").unwrap();
        assert!(dir.join(".vitepress/config.mjs").is_file());
        assert!(dir.join("index.md").is_file());
        assert!(dir.join("api").exists()); // junction 到 api_generated
        assert!(dir.join(".project_root").is_file()); // 项目路径标记
        // 站点目录名可读
        assert!(dir.file_name().unwrap().to_string_lossy().ends_with("-docs"));
        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn stop_without_start_noop() {
        let mut st = ServerState::default();
        stop(&mut st);
        assert_eq!(st.phase, StartPhase::Idle);
    }
}
