//! VuePress 本地服务（重构后架构）：
//! - VuePress 全局安装一次（app 数据目录 vuepress_home/，node_modules 一份）
//! - 项目零侵入：源原地引用（内存生成 config 指向真实路径），无 docs_site 壳目录
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
    /// 正在装 VuePress 依赖（全局 vuepress_home，仅首次）
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

/// VuePress 全局安装目录（app 数据目录；exe 旁 vuepress_home）
pub fn vuepress_home() -> PathBuf {
    let base = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::env::temp_dir());
    base.join("vuepress_home")
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

/// 内存生成 VuePress config.js（指向各源真实路径；不写项目目录）。
/// 输出到全局 vuepress_home/_sites/<项目hash>/config.js。
fn write_site_config(
    home: &Path,
    project_root: &Path,
    sources: &[(String, PathBuf)],
    port: u16,
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

    // 多源聚合：junction 链接（VuePress 2.x vite 驱动，符号链接/junction 支持完善）。
    let docs = dir.join("docs");
    std::fs::create_dir_all(&docs)?;
    for (name, src) in sources {
        let dst = docs.join(name);
        let _ = std::fs::remove_dir_all(&dst);
        link_dir(src, &dst)?;
    }
    // 首页（每次重写：源清单变化要反映在导航上）
    let mut nav = String::from("# 本地文档站\n\n左侧边栏选择文档分类。\n\n## 文档源\n\n");
    for (name, _) in sources {
        nav.push_str(&format!("- [{name}](/{name}/)\n"));
    }
    std::fs::write(docs.join("README.md"), nav)?;

    let cfg = dir.join(".vuepress");
    std::fs::create_dir_all(&cfg)?;
    // VuePress 2.x config（ESM；vite 驱动；符号链接/junction 原生支持）
    let cfg_text = format!(
        "export default {{\n  title: '本地文档站',\n  description: '项目文档聚合',\n  port: {port},\n  theme: {{ sidebar: 'auto' }},\n}}\n",
        port = if port == 0 { 8080 } else { port },
    );
    std::fs::write(cfg.join("config.js"), cfg_text)?;
    Ok(dir)
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

/// 目录递归复制（文档源聚合用；md 文件小，秒级完成）
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

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
    let port = if global.port == 0 { 8080 } else { global.port };
    let home = vuepress_home();
    let sources = effective_sources(project_root, global, project);
    if sources.is_empty() {
        st.last_error = "无可用文档源（api_generated 未构建，且未配置其他源）".into();
        st.phase = StartPhase::Failed;
        return;
    }

    st.phase = if home.join("node_modules").is_dir() { StartPhase::Starting } else { StartPhase::InstallingDeps };
    let (tx, rx) = std::sync::mpsc::channel();
    st.boot_done = Some(rx);
    let site_config = write_site_config(&home, project_root, &sources, port).ok();
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

    // 全局依赖装一次（VuePress 2.x + vite）
    if !home.join("node_modules").is_dir() {
        let pkg = home.join("package.json");
        if !pkg.exists() {
            let _ = std::fs::write(&pkg, r#"{"name":"bgd-docs-vuepress","private":true,"type":"module","devDependencies":{"vuepress":"^2.0.0-rc.20","@vuepress/bundler-vite":"^2.0.0-rc.20"}}"#);
        }
        let npm = node.with_file_name("npm.cmd");
        let mut cmd = Command::new(npm);
        cmd.arg("install").current_dir(&home);
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        match cmd.output() {
            Ok(o) if o.status.success() => {}
            Ok(o) => {
                r.error = format!("VuePress 依赖安装失败: {}", String::from_utf8_lossy(&o.stderr));
                return r;
            }
            Err(e) => {
                r.error = format!("npm install 启动失败: {e}");
                return r;
            }
        }
    }

    // spawn dev server（全局 vuepress 二进制跑项目站点目录）
    let vp = home.join("node_modules/.bin/vuepress.cmd");
    let mut cmd = Command::new(vp);
    cmd.args(["dev", "."]).current_dir(&site);
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
    // 运行中存活检测：进程死了即失败（状态一致）
    if st.phase == StartPhase::Running {
        let alive = st.child.as_mut().map(|c| c.try_wait().ok().flatten().is_none()).unwrap_or(false);
        if !alive {
            st.child = None;
            st.port = 0;
            if st.last_error.is_empty() {
                st.last_error = "dev server 进程已退出（查看端口占用/站点目录）".into();
            }
            st.phase = StartPhase::Failed;
        }
    }
}

/// 停止：杀整个进程树（vuepress.cmd 是 cmd 壳，真服务在 node 子进程，
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
        let tmp = std::env::temp_dir().join(format!("bgd_docs_srv2_{tag}_{}", std::process::id()));
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
        // 全部绝对路径且存在
        for (_, p) in &srcs {
            assert!(p.is_absolute() && p.is_dir());
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn site_config_links_sources() {
        let tmp = setup_project("cfg");
        let home = std::env::temp_dir().join(format!("bgd_docs_home_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let sources = effective_sources(&tmp, &GlobalConfig::default(), &ProjectConfig::default());
        let dir = write_site_config(&home, &tmp, &sources, 8080).unwrap();
        assert!(dir.join(".vuepress/config.js").is_file());
        assert!(dir.join("docs/api").exists()); // junction 到 api_generated
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
