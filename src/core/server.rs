//! VitePress 本地服务：
//! - VitePress 全局安装一次（app 数据目录 vitepress_home/，node_modules 一份）
//! - 项目零侵入：源原地 junction 聚合，不写项目目录
//! - 启动整链异步：探测/装依赖/起服务全离 UI 线程，UI 轮询 phase

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
    /// 后台启动任务的完成信号（整条链的结果）
    boot_done: Option<std::sync::mpsc::Receiver<BootResult>>,
}

impl Default for ServerState {
    fn default() -> Self {
        Self { child: None, port: 0, last_error: String::new(), phase: StartPhase::Idle, boot_done: None }
    }
}

/// 后台启动链的结果（成功带 dev server 子进程与端口）
struct BootResult {
    child: Option<Child>,
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

/// 生效源清单：api_generated 约定源 + 全局/项目合并源（只留真实存在的目录，绝对路径）
fn effective_sources(
    project_root: &Path,
    global: &GlobalConfig,
    project: &ProjectConfig,
) -> Vec<(String, PathBuf)> {
    let mut all = vec![DocSource { name: "api".into(), path: ".bgd/doc/api_generated".into() }];
    all.extend(config::effective_sources(global, project));
    all.into_iter()
        .filter_map(|s| {
            let p = { let x = PathBuf::from(&s.path); if x.is_absolute() { x } else { project_root.join(&s.path) } };
            if p.is_dir() { Some((s.name, p)) } else { None }
        })
        .collect()
}

/// 生成站点目录（vitepress_home/_sites/<项目hash>/）：
/// junction 聚合源 + index.md 首页 + .vitepress/config.mjs。
fn write_site_config(
    home: &Path,
    project_root: &Path,
    sources: &[(String, PathBuf)],
) -> std::io::Result<PathBuf> {
    let hash = {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        project_root.display().to_string().hash(&mut h);
        format!("{:x}", h.finish())
    };
    let dir = home.join("_sites").join(hash);
    std::fs::create_dir_all(&dir)?;

    // 多源聚合：junction 到站点根（VitePress 文件路由能扫到 junction 里的 md，
    // 但 vite 默认 fs.strict 会拦 junction 外部路径——需配 fs.strict: false）。
    for (name, src) in sources {
        let dst = dir.join(name);
        let _ = std::fs::remove_dir_all(&dst);
        link_dir(src, &dst)?;
    }
    // 首页（每次重写：源清单变化要反映在导航上）
    // 链接不带 ./ 前缀（VitePress 路由基于站点根）
    let mut nav = String::from("# 本地文档站\n\n左侧边栏选择文档分类。\n\n## 文档源\n\n");
    for (name, _) in sources {
        nav.push_str(&format!("- [{name}](/{name}/)\n"));
    }
    std::fs::write(dir.join("index.md"), nav)?;

    // VitePress config（ESM；本地搜索内置 minisearch，无需额外插件；
    // preserveSymlinks: 不 realpath junction（防路由被算成真实物理路径而 404）；
    // fs.strict=false: 放行 junction 指向的站点外路径；
    // cleanUrls=false: VitePress 2.x 的 cleanUrls 和 junction 有冲突（/api/ 404）；
    // rewrites: VitePress 2.x 不再把 README.md 当 index 页，需显式映射）
    let cfg = dir.join(".vitepress");
    std::fs::create_dir_all(&cfg)?;
    // 每个源的 README.md 都映射成 index.md（VitePress 2.x 不再自动识别 README）
    let rewrites: Vec<String> = sources.iter().map(|(name, _)| format!("    '{name}/README.md': '{name}/index.md',")).collect();
    let cfg_text = format!("import {{ defineConfig }} from 'vitepress'\n\nexport default defineConfig({{\n  title: '本地文档站',\n  description: '项目文档聚合',\n  lang: 'zh-CN',\n  lastUpdated: false,\n  cleanUrls: false,\n  rewrites: {{\n{}\n  }},\n  themeConfig: {{\n    search: {{ provider: 'local' }},\n    sidebar: [],\n  }},\n  vite: {{\n    resolve: {{ preserveSymlinks: true }},\n    server: {{ fs: {{ strict: false }} }},\n  }},\n}})\n", rewrites.join("\n"));
    std::fs::write(cfg.join("config.mjs"), cfg_text)?;
    Ok(dir)
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
    let site_config = write_site_config(&home, project_root, &sources).ok();
    std::thread::spawn(move || {
        let r = boot_chain(home, node, site_config, port);
        let _ = tx.send(r);
    });
}

/// 后台链本体：装依赖 → spawn dev（全在独立线程，不卡 UI）
fn boot_chain(home: PathBuf, node: PathBuf, site: Option<PathBuf>, port: u16) -> BootResult {
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

    // spawn dev server（全局 vitepress 二进制跑项目站点目录）
    // stderr/stdout 落日志文件（进程退出时 UI 能读到真实原因）
    let vp = home.join("node_modules/.bin/vitepress.cmd");
    let log_path = site.join("dev-server.log");
    let log_file = std::fs::File::create(&log_path).ok();
    let mut cmd = Command::new(vp);
    // vitepress dev <root>：source 即站点根（.vitepress/ 所在目录）；--port 显式指定
    cmd.arg("dev").arg(&site).arg("--port").arg(port.to_string()).current_dir(&site);
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
            r.child = Some(c);
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
                st.child = Some(c);
                st.port = r.port;
                st.phase = StartPhase::Running;
            } else {
                st.last_error = r.error;
                st.phase = StartPhase::Failed;
            }
        }
    }
    // 运行中存活检测：进程死了即失败（读 dev-server.log 尾部拿真实原因）
    if st.phase == StartPhase::Running {
        let alive = st.child.as_mut().map(|c| c.try_wait().ok().flatten().is_none()).unwrap_or(false);
        if !alive {
            st.child = None;
            st.port = 0;
            if st.last_error.is_empty() {
                st.last_error = read_dev_log_tail();
            }
            st.phase = StartPhase::Failed;
        }
    }
}

/// 读 dev server 日志尾部（进程退出原因）
fn read_dev_log_tail() -> String {
    let log = vitepress_home().join("_sites");
    if let Ok(entries) = std::fs::read_dir(&log) {
        for e in entries.flatten() {
            let f = e.path().join("dev-server.log");
            if f.is_file() {
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
            }
        }
    }
    "dev server 进程已退出（查看 vitepress_home/_sites/<项目>/dev-server.log）".into()
}

/// 停止：杀整个进程树（vitepress.cmd 是 cmd 壳，真服务在 node 子进程，
/// child.kill() 只杀壳会留孤儿占端口——必须 taskkill /T 整树）
pub fn stop(st: &mut ServerState) {
    if let Some(c) = st.child.take() {
        kill_tree(&c);
        drop(c);
    }
    st.port = 0;
    st.phase = StartPhase::Idle;
    st.boot_done = None;
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
        let tmp = std::env::temp_dir().join(format!("bgd_docs_srv3_{tag}_{}", std::process::id()));
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
    fn site_config_links_sources() {
        let tmp = setup_project("cfg");
        let home = std::env::temp_dir().join(format!("bgd_docs_home3_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let sources = effective_sources(&tmp, &GlobalConfig::default(), &ProjectConfig::default());
        let dir = write_site_config(&home, &tmp, &sources).unwrap();
        assert!(dir.join(".vitepress/config.mjs").is_file());
        assert!(dir.join("index.md").is_file());
        assert!(dir.join("api").exists()); // junction 到 api_generated
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
