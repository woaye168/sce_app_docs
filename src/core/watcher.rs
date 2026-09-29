//! 文档监听：notify 监听文档源目录（真实路径，不经 junction），md 变更投事件。
//! debounce 在 service 层做（聚合 1s 静默后触发一次重建）。
//! 监听失败（如 junction/权限问题）返回 None 降级——服务照常，只是不自动重建。

use notify::{RecursiveMode, Watcher};
use std::path::PathBuf;
use std::sync::mpsc;

pub struct DocWatcher {
    _watcher: notify::RecommendedWatcher,
}

/// 监听一组路径（path, 是否递归）。md 文件 创建/修改/删除 时往 tx 投 ()。
pub fn start(paths: Vec<(PathBuf, bool)>, tx: mpsc::Sender<()>) -> Option<DocWatcher> {
    let cb = move |res: Result<notify::Event, notify::Error>| {
        let Ok(ev) = res else { return };
        use notify::EventKind::*;
        if !matches!(ev.kind, Create(_) | Modify(_) | Remove(_)) {
            return;
        }
        let is_md = ev.paths.iter().any(|p| {
            p.extension().map(|e| e == "md").unwrap_or(false)
        });
        if is_md {
            let _ = tx.send(());
        }
    };
    let mut watcher = notify::recommended_watcher(cb).ok()?;
    let mut any = false;
    for (p, recursive) in paths {
        let mode = if recursive { RecursiveMode::Recursive } else { RecursiveMode::NonRecursive };
        if watcher.watch(&p, mode).is_ok() {
            any = true;
        }
    }
    any.then_some(DocWatcher { _watcher: watcher })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md_change_fires_event() {
        let tmp = std::env::temp_dir().join(format!("bgd_docs_watch_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let (tx, rx) = mpsc::channel();
        let Some(_w) = start(vec![(tmp.clone(), true)], tx) else {
            panic!("监听创建失败");
        };
        std::thread::sleep(std::time::Duration::from_millis(300)); // 等 watcher 就绪
        std::fs::write(tmp.join("a.md"), "# hello").unwrap();
        let got = rx.recv_timeout(std::time::Duration::from_secs(5));
        assert!(got.is_ok(), "md 变更应触发事件");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
