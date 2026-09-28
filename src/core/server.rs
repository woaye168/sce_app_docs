//! VuePress 本地服务：站点脚手架生成（<项目>/.bgd/docs_site/）+ dev server 启停。
//! 源聚合：api_generated（约定）+ 生效文档源（全局+项目合并）→ 软链进站点 docs/ 目录。

use crate::core::config::{self, DocSource, GlobalConfig, ProjectConfig};
use crate::core::node;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

/// Windows 下不弹控制台窗口
#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// 后台启动阶段（UI 轮询这个状态渲染进度）
#[derive(Debug, Clone, PartialEq)]
pub enum StartPhase {
    /// 空闲（未启动）
    Idle,
    /// 正在装依赖（npm install，首次几分钟）
    InstallingDeps,
    /// 依赖就绪，正在起 dev server
    Starting,
    /// 运行中
    Running,
    /// 失败（last_error 有原因）
    Failed,
}

/// 服务状态（UI 读这个渲染）
pub struct ServerState {
    /// 运行中的 dev server 子进程
    pub child: Option<Child>,
    /// 启动后实际端口
    pub port: u16,
    /// 最近一次错误（启动失败等）
    pub last_error: String,
    /// 当前启动阶段（UI 进度展示）
    pub phase: StartPhase,
    /// 后台安装线程的完成信号（npm install 结果）
    pub install_done: Option<std::sync::mpsc::Receiver<Result<(), String>>>,
    /// 安装完成后待续的启动参数（项目根/端口）
    pending_start: Option<(PathBuf, u16)>,
}

impl Default for ServerState {
    fn default() -> Self {
        Self {
            child: None,
            port: 0,
            last_error: String::new(),
            phase: StartPhase::Idle,
            install_done: None,
            pending_start: None,
        }
    }
}

/// 站点目录（<项目>/.bgd/docs_site/）
pub fn site_dir(project_root: &Path) -> PathBuf {
    project_root.join(".bgd").join("docs_site")
}

/// 文档源聚合目标（站点 docs/ 下按名分目录）
fn docs_dir(project_root: &Path) -> PathBuf {
    site_dir(project_root).join("docs")
}

/// 聚合文档源：api_generated 约定源 + 生效源，逐个软链进站点 docs/。
/// 相对路径按项目根解析。已存在的同名链接先删再建（内容变化随 dev server 热更）。
/// 返回聚合成功的源清单（跳过了不存在的目录）。
pub fn aggregate_sources(
    project_root: &Path,
    global: &GlobalConfig,
    project: &ProjectConfig,
) -> Vec<DocSource> {
    let mut all = vec![DocSource {
        name: "api".into(),
        path: ".bgd/doc/api_generated".into(),
    }];
    all.extend(config::effective_sources(global, project));

    let docs = docs_dir(project_root);
    let _ = std::fs::create_dir_all(&docs);
    let mut linked = Vec::new();
    for s in all {
        let src = {
            let p = PathBuf::from(&s.path);
            if p.is_absolute() { p } else { project_root.join(&s.path) }
        };
        if !src.is_dir() {
            continue; // 源不存在跳过（约定源 api_generated 没 build 过也跳过）
        }
        let dst = docs.join(&s.name);
        let _ = std::fs::remove_dir_all(&dst);
        match symlink_dir(&src, &dst) {
            Ok(()) => linked.push(s),
            Err(e) => eprintln!("[warn] 文档源链接失败 {} -> {}: {e}", src.display(), dst.display()),
        }
    }
    linked
}

/// 目录软链（Windows junction 兜底：symlink_dir 需权限，失败降级 junction）
#[cfg(windows)]
fn symlink_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    match std::os::windows::fs::symlink_dir(src, dst) {
        Ok(()) => Ok(()),
        Err(_) => {
            // 降级：cmd mklink /J（junction 不需要特权，Win7+ 通用）；
            // cmd 不认正斜杠路径，统一转反斜杠
            let dst_s = dst.display().to_string().replace('/', "\\");
            let src_s = src.display().to_string().replace('/', "\\");
            let status = Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&dst_s)
                .arg(&src_s)
                .output()?;
            if status.status.success() {
                Ok(())
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("mklink /J 失败: {}", String::from_utf8_lossy(&status.stderr)),
                ))
            }
        }
    }
}

#[cfg(not(windows))]
fn symlink_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

/// 生成 VuePress 站点脚手架（package.json + docs/.vuepress/config.js + 首页）。
/// 幂等：已存在只补缺失文件，不覆盖用户手改。
pub fn scaffold_site(project_root: &Path, port: u16) -> std::io::Result<()> {
    let site = site_dir(project_root);
    std::fs::create_dir_all(site.join("docs/.vuepress"))?;

    let pkg = site.join("package.json");
    if !pkg.exists() {
        std::fs::write(&pkg, r#"{
  "name": "bgd-docs-site",
  "private": true,
  "scripts": { "dev": "vuepress dev docs" },
  "devDependencies": { "vuepress": "^1.9.10" }
}"#)?;
    }

    let cfg = site.join("docs/.vuepress/config.js");
    let cfg_text = format!(
        r#"module.exports = {{
  title: '文档站',
  description: '项目文档聚合',
  dest: 'dist',
  port: {port},
  themeConfig: {{ sidebar: 'auto', searchMaxSuggestions: 20 }},
}}
"#,
        port = if port == 0 { 8080 } else { port },
    );
    std::fs::write(&cfg, cfg_text)?; // config 每次重写（端口/源变化要生效）

    let readme = site.join("docs/README.md");
    if !readme.exists() {
        std::fs::write(&readme, "# 文档站\n\n左侧边栏选择文档分类。\n")?;
    }
    Ok(())
}

/// 后台装依赖（npm install 几分钟，必须离 UI 线程）。
/// 完成后经 channel 发结果；UI 轮询 start_tick 续上 dev server 启动。
fn spawn_install(site: PathBuf, node: PathBuf, st: &mut ServerState) {
    let (tx, rx) = std::sync::mpsc::channel();
    st.install_done = Some(rx);
    std::thread::spawn(move || {
        let npm = node.with_file_name("npm.cmd");
        let mut cmd = Command::new(npm);
        cmd.arg("install").current_dir(&site);
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        let result = match cmd.output() {
            Ok(out) if out.status.success() => Ok(()),
            Ok(out) => Err(format!("npm install 失败: {}", String::from_utf8_lossy(&out.stderr))),
            Err(e) => Err(format!("npm install 启动失败: {e}")),
        };
        let _ = tx.send(result);
    });
}

/// 启动 dev server（UI 调用，立即返回不阻塞）。
/// 流程：解析 node → 聚合源 → 脚手架 → node_modules 缺失则后台装依赖（poll_start 续）→ spawn dev。
pub fn start(project_root: &Path, global: &GlobalConfig, project: &ProjectConfig, st: &mut ServerState) {
    stop(st);
    st.last_error.clear();
    st.phase = StartPhase::Starting;

    let Some(node) = node::resolve() else {
        st.last_error = "未找到 node.exe（设置页配置或安装 Node.js）".into();
        st.phase = StartPhase::Failed;
        return;
    };
    let port = if global.port == 0 { 8080 } else { global.port };

    if let Err(e) = scaffold_site(project_root, port) {
        st.last_error = format!("站点脚手架生成失败: {e}");
        st.phase = StartPhase::Failed;
        return;
    }
    aggregate_sources(project_root, global, project);

    let site = site_dir(project_root);
    if site.join("node_modules").is_dir() {
        spawn_dev(&site, port, st); // 依赖已装直接起
    } else {
        st.phase = StartPhase::InstallingDeps;
        st.pending_start = Some((site, port));
        spawn_install(site_dir(project_root), node, st);
    }
}

/// spawn vuepress dev server（不弹黑框）
fn spawn_dev(site: &Path, port: u16, st: &mut ServerState) {
    let npm = site.join("node_modules/.bin/vuepress.cmd");
    let mut cmd = Command::new(npm);
    cmd.args(["dev", "docs"]).current_dir(site);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    match cmd.spawn() {
        Ok(c) => {
            st.child = Some(c);
            st.port = port;
            st.phase = StartPhase::Running;
        }
        Err(e) => {
            st.last_error = format!("dev server 启动失败: {e}");
            st.phase = StartPhase::Failed;
        }
    }
}

/// UI 每帧轮询：后台装依赖完成后续上 dev server 启动
pub fn start_tick(st: &mut ServerState) {
    if st.phase != StartPhase::InstallingDeps {
        return;
    }
    let done = st.install_done.as_ref().and_then(|rx| rx.try_recv().ok());
    let Some(result) = done else { return };
    st.install_done = None;
    match result {
        Ok(()) => {
            if let Some((site, port)) = st.pending_start.take() {
                spawn_dev(&site, port, st);
            }
        }
        Err(e) => {
            st.last_error = e;
            st.phase = StartPhase::Failed;
            st.pending_start = None;
        }
    }
}

/// 停止 dev server
pub fn stop(st: &mut ServerState) {
    if let Some(mut c) = st.child.take() {
        let _ = c.kill();
        let _ = c.wait();
    }
    st.port = 0;
    st.phase = StartPhase::Idle;
    st.pending_start = None;
    // 装依赖线程让它自己跑完（结果丢弃；进程退出时随 kill 清理）
    st.install_done = None;
}

/// 服务是否在运行
pub fn is_running(st: &mut ServerState) -> bool {
    match st.child.as_mut() {
        Some(c) => c.try_wait().ok().flatten().is_none(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每测试唯一临时目录（测试并发时互踩同名目录会导致间歇性失败）
    fn setup_project(tag: &str) -> PathBuf {
        let tmp = std::env::temp_dir().join(format!("bgd_docs_srv_test_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join(".bgd/doc/api_generated")).unwrap();
        std::fs::create_dir_all(tmp.join(".bgd/doc/research")).unwrap();
        tmp
    }

    #[test]
    fn scaffold_creates_site_files() {
        let tmp = setup_project("scaffold");
        scaffold_site(&tmp, 8080).unwrap();
        let site = site_dir(&tmp);
        assert!(site.join("package.json").is_file());
        assert!(site.join("docs/.vuepress/config.js").is_file());
        assert!(site.join("docs/README.md").is_file());
        let cfg = std::fs::read_to_string(site.join("docs/.vuepress/config.js")).unwrap();
        assert!(cfg.contains("port: 8080"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn aggregate_links_existing_sources_only() {
        let tmp = setup_project("aggregate");
        let g = GlobalConfig {
            sources: vec![DocSource { name: "research".into(), path: ".bgd/doc/research".into() }],
            ..Default::default()
        };
        let p = ProjectConfig {
            sources: vec![DocSource { name: "missing".into(), path: ".bgd/doc/no_such".into() }],
            ..Default::default()
        };
        let linked = aggregate_sources(&tmp, &g, &p);
        // api（约定源）+ research 存在 → 2；missing 不存在跳过
        assert_eq!(linked.len(), 2);
        let docs = docs_dir(&tmp);
        // junction 的 exists() 经符号链接解析目标——目标存在即 true
        assert!(docs.join("api").is_dir(), "api junction 应存在且可解析");
        assert!(docs.join("research").is_dir(), "research junction 应存在且可解析");
        assert!(!docs.join("missing").exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn stop_without_start_is_noop() {
        let mut st = ServerState::default();
        stop(&mut st); // 不应 panic
        assert!(!is_running(&mut st));
    }
}
