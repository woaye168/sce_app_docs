//! VuePress 本地服务：站点脚手架生成（<项目>/.bgd/docs_site/）+ dev server 启停。
//! 源聚合：api_generated（约定）+ 生效文档源（全局+项目合并）→ 软链进站点 docs/ 目录。

use crate::core::config::{self, DocSource, GlobalConfig, ProjectConfig};
use crate::core::node;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

/// 服务状态（UI 读这个渲染）
pub struct ServerState {
    /// 运行中的 dev server 子进程
    pub child: Option<Child>,
    /// 启动后实际端口
    pub port: u16,
    /// 最近一次错误（启动失败等）
    pub last_error: String,
}

impl Default for ServerState {
    fn default() -> Self {
        Self { child: None, port: 0, last_error: String::new() }
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

/// 确保依赖已装（node_modules 缺失时跑 npm install；有则跳过）
fn ensure_deps(site: &Path, node: &Path) -> Result<(), String> {
    if site.join("node_modules").is_dir() {
        return Ok(());
    }
    let npm = node.with_file_name("npm.cmd");
    let out = Command::new(npm)
        .arg("install")
        .current_dir(site)
        .output()
        .map_err(|e| format!("npm install 启动失败: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "npm install 失败: {}",
            String::from_utf8_lossy(&out.stderr)
        ))
    }
}

/// 启动 dev server（前台 UI 调用；spawn 后立即返回，不阻塞）。
/// 流程：解析 node → 聚合源 → 脚手架 → 装依赖 → spawn vuepress dev。
pub fn start(project_root: &Path, global: &GlobalConfig, project: &ProjectConfig, st: &mut ServerState) {
    stop(st);
    st.last_error.clear();

    let Some(node) = node::resolve() else {
        st.last_error = "未找到 node.exe（设置页配置或安装 Node.js）".into();
        return;
    };
    let port = if global.port == 0 { 8080 } else { global.port };

    if let Err(e) = scaffold_site(project_root, port) {
        st.last_error = format!("站点脚手架生成失败: {e}");
        return;
    }
    aggregate_sources(project_root, global, project);

    let site = site_dir(project_root);
    if let Err(e) = ensure_deps(&site, &node) {
        st.last_error = e;
        return;
    }

    // spawn vuepress dev（经 npm run dev，Windows 下走 cmd 包装）
    let npm = node.with_file_name("npm.cmd");
    let child = Command::new(npm)
        .args(["run", "dev"])
        .current_dir(&site)
        .spawn();
    match child {
        Ok(c) => {
            st.child = Some(c);
            st.port = port;
        }
        Err(e) => st.last_error = format!("dev server 启动失败: {e}"),
    }
}

/// 停止 dev server
pub fn stop(st: &mut ServerState) {
    if let Some(mut c) = st.child.take() {
        let _ = c.kill();
        let _ = c.wait();
    }
    st.port = 0;
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
