//! 服务编排：状态机 + 首次构建链 + 监听重建。
//! 端口由 httpd（exe 自己）持有，构建进程短命且有 Job Object 兜底——无孤儿、无句柄丢失、无端口漂移。
//!
//! 状态机：Idle → InstallingDeps/Building → Serving（Failed 终态，报错可见）
//! 重建流：watcher 事件 → debounce 1s → 构建到备用 dist → 原子翻转服务根 → 删旧 dist（全程旧内容不掉线）

use crate::core::config::{GlobalConfig, ProjectConfig};
use crate::core::{builder, httpd, indexer, site, watcher};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

/// 默认端口（18753：本地文档站，避开 8080 等常用端口冲突）
pub const DEFAULT_PORT: u16 = 18753;

#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Idle,
    /// 正在装 VitePress 依赖（全局 vitepress_home，仅首次）
    InstallingDeps,
    /// 正在构建（vitepress build）
    Building,
    Serving,
    Failed,
}

/// 构建线程回包：新 dist 目录或错误
type BuildMsg = Result<PathBuf, String>;

/// 重建所需上下文（构建线程参数）
struct ServiceCtx {
    home: PathBuf,
    node: PathBuf,
    site: PathBuf,
    /// 当前服务中的 dist 目录名（dist-a / dist-b 轮换）
    cur_dist: &'static str,
    /// 项目根与文档源（重建时重新生成站点 config：sidebar/根 md 副本同步刷新）
    project_root: PathBuf,
    sources: Vec<(String, PathBuf)>,
}

pub struct ServiceState {
    pub phase: Phase,
    pub port: u16,
    pub last_error: String,
    /// 站点目录（读 build.log 用）
    pub site_dir: Option<PathBuf>,
    /// 重建中提示（Serving 期间后台重建；UI 显示用）
    pub rebuilding: bool,
    httpd: Option<httpd::Httpd>,
    dist_root: Option<Arc<RwLock<PathBuf>>>,
    boot_rx: Option<Receiver<BuildMsg>>,
    rebuild_rx: Option<Receiver<BuildMsg>>,
    change_rx: Option<Receiver<()>>,
    change_tx: Option<mpsc::Sender<()>>,
    pending_change: Option<Instant>,
    ctx: Option<ServiceCtx>,
    /// watcher 监听路径（真实路径，不经 junction；start 时算好，Serving 后启用）
    watch_paths: Vec<(PathBuf, bool)>,
    _watcher: Option<watcher::DocWatcher>,
    /// 索引状态（GUI 状态条 / /_api/index_status 读这里）
    pub index_status: indexer::SharedIndexStatus,
    /// API 共享状态（检索上下文在里面）
    api: Option<Arc<httpd::ApiState>>,
}

impl Default for ServiceState {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            port: 0,
            last_error: String::new(),
            site_dir: None,
            rebuilding: false,
            httpd: None,
            dist_root: None,
            boot_rx: None,
            rebuild_rx: None,
            change_rx: None,
            change_tx: None,
            pending_change: None,
            ctx: None,
            watch_paths: Vec::new(),
            _watcher: None,
            index_status: indexer::new_shared_status(),
            api: None,
        }
    }
}

/// 启动服务：生成站点 → 先绑端口（被占立即报错，不漂移）→ 后台构建链。
pub fn start(project_root: &std::path::Path, global: &GlobalConfig, project: &ProjectConfig, st: &mut ServiceState) {
    stop(st);
    st.last_error.clear();

    let Some(node) = crate::core::node::resolve() else {
        st.last_error = "未找到 node.exe（设置页配置或安装 Node.js）".into();
        st.phase = Phase::Failed;
        return;
    };
    let sources = site::effective_sources(project_root, global, project);
    if sources.is_empty() {
        st.last_error = "无可用文档源（api_generated 未构建，且未配置其他源）".into();
        st.phase = Phase::Failed;
        return;
    }
    let home = site::vitepress_home();
    let site_dir = match site::write_site_config(&home, project_root, &sources) {
        Ok(d) => d,
        Err(e) => {
            st.last_error = format!("站点 config 生成失败: {e}");
            st.phase = Phase::Failed;
            return;
        }
    };
    tracing::info!("site_gen ok: {} ← {}", site_dir.display(), project_root.display());

    // 先绑端口：被占立即失败（等效 strictPort），而不是构建几分钟后才发现
    let port = if global.port == 0 { DEFAULT_PORT } else { global.port };
    let dist_root = Arc::new(RwLock::new(site_dir.join("dist-a")));
    let hosts: Vec<String> = if global.allowed_hosts_enabled {
        global.allowed_hosts.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
    } else {
        Vec::new()
    };
    // API 状态：索引状态 + 检索上下文（模型懒加载常驻）
    let api = Arc::new(httpd::ApiState {
        status: st.index_status.clone(),
        search: Mutex::new(indexer::SearchCtx::new(home.clone(), site_dir.clone())),
        use_rerank: global.rerank_enabled,
        llm: crate::core::llm::LlmConfig {
            base_url: global.llm_base_url.clone(),
            api_key: global.llm_api_key.clone(),
            model: global.llm_model.clone(),
        },
        max_rounds: global.max_tool_rounds.clamp(1, 20),
        mcp_sessions: Mutex::new(crate::core::mcp::SessionMgr::new()),
    });
    let httpd = match httpd::start(dist_root.clone(), port, global.lan_access, hosts, Some(api.clone())) {
        Ok(h) => h,
        Err(e) => {
            st.last_error = e;
            st.phase = Phase::Failed;
            return;
        }
    };
    st.api = Some(api);
    tracing::info!("httpd 已监听 端口{}（lan={}）", port, global.lan_access);

    // 后台构建链：装依赖（若缺）→ vitepress build 到 dist-a
    st.phase = if home.join("node_modules").is_dir() { Phase::Building } else { Phase::InstallingDeps };
    st.port = port;
    st.site_dir = Some(site_dir.clone());
    st.httpd = Some(httpd);
    st.dist_root = Some(dist_root);

    let mut watch_paths: Vec<(PathBuf, bool)> = sources.iter().map(|(_, p)| (p.clone(), true)).collect();
    watch_paths.push((project_root.to_path_buf(), false)); // 项目根只管顶层 md（index/README/AGENTS）
    st.watch_paths = watch_paths;
    st.ctx = Some(ServiceCtx {
        home: home.clone(),
        node: node.clone(),
        site: site_dir.clone(),
        cur_dist: "dist-a",
        project_root: project_root.to_path_buf(),
        sources,
    });

    let (tx, rx) = mpsc::channel::<BuildMsg>();
    st.boot_rx = Some(rx);
    std::thread::spawn(move || {
        let r = builder::ensure_deps(&home, &node).and_then(|()| builder::build(&home, &node, &site_dir, "dist-a"));
        let _ = tx.send(r);
    });
}

/// 停止：关 listener（端口立即释放）+ 停监听。无外部进程需要杀。
pub fn stop(st: &mut ServiceState) {
    st.httpd = None; // drop 即停线程、释放端口
    st.phase = Phase::Idle;
    st.port = 0;
    st.site_dir = None;
    st.rebuilding = false;
    st.dist_root = None;
    st.boot_rx = None;
    st.rebuild_rx = None;
    st.change_rx = None;
    st.change_tx = None;
    st.pending_change = None;
    st.ctx = None;
    st.watch_paths.clear();
    st._watcher = None;
    st.api = None;
    st.index_status = indexer::new_shared_status();
}

/// UI/CLI 轮询：收首次构建结果、收监听事件、debounce 触发重建、收重建结果
pub fn tick(st: &mut ServiceState, auto_rebuild: bool) {
    // 首次构建链结果
    if let Some(rx) = &st.boot_rx {
        if let Ok(r) = rx.try_recv() {
            st.boot_rx = None;
            match r {
                Ok(dist) => {
                    flip_root(st, dist);
                    st.phase = Phase::Serving;
                    tracing::info!("build ok（Serving）");
                    // 起 watcher（auto_rebuild 开时）
                    if auto_rebuild && !st.watch_paths.is_empty() {
                        let (tx, rx) = mpsc::channel();
                        if let Some(w) = watcher::start(st.watch_paths.clone(), tx.clone()) {
                            st.change_tx = Some(tx);
                            st.change_rx = Some(rx);
                            st._watcher = Some(w);
                        }
                    }
                    // 后台索引同步（不阻塞 Serving；模型懒加载，无变更时秒过）
                    spawn_index_sync(st);
                }
                Err(e) => {
                    st.last_error = e;
                    st.phase = Phase::Failed;
                    st.httpd = None; // 构建失败就不占端口了
                }
            }
        }
    }
    // watcher 事件 debounce（1s 静默后触发一次重建；构建期间新变更排队一次）
    if let Some(rx) = &st.change_rx {
        while rx.try_recv().is_ok() {
            st.pending_change = Some(Instant::now());
        }
    }
    if st.phase == Phase::Serving && st.rebuild_rx.is_none() {
        if let Some(t) = st.pending_change {
            if t.elapsed() >= Duration::from_secs(1) {
                st.pending_change = None;
                st.rebuilding = true;
                if let Some(ctx) = &st.ctx {
                    let (tx, rx) = mpsc::channel();
                    st.rebuild_rx = Some(rx);
                    let (home, node, site_dir, out) = (ctx.home.clone(), ctx.node.clone(), ctx.site.clone(), other_dist(ctx.cur_dist));
                    let (proj, sources) = (ctx.project_root.clone(), ctx.sources.clone());
                    std::thread::spawn(move || {
                        let _ = tx.send(rebuild_site_and_build(home, node, site_dir, out, proj, sources));
                    });
                }
            }
        }
    }
    // 收重建结果（无论成败都不影响当前 Serving）
    if let Some(rx) = &st.rebuild_rx {
        if let Ok(r) = rx.try_recv() {
            st.rebuild_rx = None;
            st.rebuilding = false;
            match r {
                Ok(dist) => {
                    flip_root(st, dist);
                    spawn_index_sync(st); // 重建完成后索引增量同步
                }
                Err(e) => st.last_error = format!("重建失败：{e}"),
            }
        }
    }
}

/// 后台索引同步线程（共享 SearchCtx 锁，与 /_api/search 互斥排队；低并发无瓶颈）
fn spawn_index_sync(st: &ServiceState) {
    let Some(api) = &st.api else { return };
    let api = api.clone();
    std::thread::spawn(move || {
        match indexer::sync_index(&api.search, &api.status) {
            Ok(n) => tracing::info!("索引同步完成: {} 块", n),
            Err(e) => {
                tracing::info!("索引同步失败: {e}");
                // 失败状态写进 index_status（sync_index 内部对 embed 失败已写，这里兜其他错误）
                if let Ok(mut s) = api.status.write() {
                    if s.state != "failed" {
                        s.state = "failed".into();
                        s.error = e;
                    }
                }
            }
        }
    });
}

/// 重建：先重新生成站点 config（新增/删除 md 要刷新 sidebar 与根 md 副本）再构建。
/// write_site_config 幂等（junction 已存在先删再建，秒级），失败不阻断构建。
fn rebuild_site_and_build(
    home: PathBuf,
    node: PathBuf,
    site_dir: PathBuf,
    out: &'static str,
    project_root: PathBuf,
    sources: Vec<(String, PathBuf)>,
) -> BuildMsg {
    if let Err(e) = site::write_site_config(&home, &project_root, &sources) {
        return Err(format!("重建时站点 config 刷新失败: {e}"));
    }
    builder::build(&home, &node, &site_dir, out)
}

fn other_dist(cur: &str) -> &'static str {
    if cur == "dist-a" { "dist-b" } else { "dist-a" }
}

/// 原子翻转服务根到新 dist，删旧 dist
fn flip_root(st: &mut ServiceState, dist: PathBuf) {
    let new_name: &'static str = if dist.ends_with("dist-a") { "dist-a" } else { "dist-b" };
    if let Some(root) = &st.dist_root {
        if let Ok(mut w) = root.write() {
            *w = dist;
        }
    }
    // 删旧 dist（另一个轮换目录）
    if let Some(ctx) = &mut st.ctx {
        let old = ctx.site.join(ctx.cur_dist);
        ctx.cur_dist = new_name;
        let cur = ctx.site.join(ctx.cur_dist);
        if old != cur {
            let _ = std::fs::remove_dir_all(old);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_without_start_noop() {
        let mut st = ServiceState::default();
        stop(&mut st);
        assert_eq!(st.phase, Phase::Idle);
    }

    #[test]
    fn start_fails_without_sources() {
        let tmp = std::env::temp_dir().join(format!("bgd_docs_svc_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join(".bgd")).unwrap(); // 无任何文档源目录
        let mut st = ServiceState::default();
        start(&tmp, &GlobalConfig::default(), &ProjectConfig::default(), &mut st);
        assert_eq!(st.phase, Phase::Failed);
        assert!(st.last_error.contains("无可用文档源"));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
