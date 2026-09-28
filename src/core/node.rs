//! node.exe 探测：优先配置路径，未配置时自动扫描本机（PATH → 常见安装位）。
//! 扫描结果缓存到全局配置（下次启动直接命中，不再重扫）。

use std::path::{Path, PathBuf};

/// 校验 node 路径可用（存在且能跑 --version）
pub fn is_valid_node(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    std::process::Command::new(path)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 从 PATH 环境变量找 node.exe
fn find_in_path() -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join("node.exe");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// 常见安装位兜底扫描
fn find_in_common_locations() -> Option<PathBuf> {
    let candidates = [
        // 官方安装包默认
        std::env::var("ProgramFiles")
            .map(|p| PathBuf::from(p).join("nodejs").join("node.exe"))
            .unwrap_or_default(),
        // nvm-windows 当前版本软链
        std::env::var("NVM_SYMLINK")
            .map(|p| PathBuf::from(p).join("node.exe"))
            .unwrap_or_default(),
        // nvm 官方（unix 风格目录在 win 也有人用）
        std::env::var("LOCALAPPDATA")
            .map(|p| PathBuf::from(p).join("nvm").join("current").join("node.exe"))
            .unwrap_or_default(),
    ];
    candidates.into_iter().find(|p| p.is_file())
}

/// 自动扫描：PATH → 常见安装位。找到即返回。
pub fn auto_detect() -> Option<PathBuf> {
    find_in_path().or_else(find_in_common_locations)
}

/// 解析生效 node 路径：配置路径有效 → 用配置；否则自动扫描（命中回写配置）。
/// 返回 None = 本机没装 node。
pub fn resolve() -> Option<PathBuf> {
    let cfg = crate::core::config::read_global();
    if !cfg.node_path.is_empty() {
        let p = PathBuf::from(&cfg.node_path);
        if is_valid_node(&p) {
            return Some(p);
        }
    }
    let found = auto_detect()?;
    // 回写配置（下次直接用）
    let mut cfg = cfg;
    cfg.node_path = found.display().to_string().replace('\\', "/");
    crate::core::config::write_global(&cfg);
    Some(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_path_rejected() {
        assert!(!is_valid_node(Path::new("D:/no/such/node.exe")));
    }

    #[test]
    fn auto_detect_finds_or_none() {
        // 本机有 node（开发机）则必命中；无 node 的环境返回 None 也算通过（不 panic）
        if let Some(p) = auto_detect() {
            assert!(p.is_file());
        }
    }

    #[test]
    fn resolve_returns_valid_node() {
        // 本机装了 node：resolve 必须返回可用路径
        if auto_detect().is_some() {
            let p = resolve().expect("本机有 node 时 resolve 应命中");
            assert!(is_valid_node(&p));
        }
    }
}
