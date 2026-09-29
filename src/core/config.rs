//! 文档站双配置：全局（exe 旁 config.json：node 路径/端口/全局文档源）
//! + 项目级（<项目>/.bgd/docs.json：本项目追加的文档源）。
//! 两级都是稀疏覆盖：项目级只存与全局不同的项，未配的字段回落全局。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 一条文档源（目录：name 展示名 + path 绝对/相对项目根路径）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DocSource {
    pub name: String,
    pub path: String,
}

/// 全局配置（exe 旁 sce_app_docs.config.json）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GlobalConfig {
    /// node.exe 路径（空 = 未配置，启动时自动扫描）
    #[serde(default)]
    pub node_path: String,
    /// dev server 端口（0 = 默认 18753）
    #[serde(default)]
    pub port: u16,
    /// 局域网访问（true = 绑 0.0.0.0，手机/其他设备可访问；false = 只本机 ::1）
    #[serde(default)]
    pub lan_access: bool,
    /// 允许域名开关（false = allowed_hosts 不生效，域名配置保留）
    #[serde(default)]
    pub allowed_hosts_enabled: bool,
    /// 允许域名（逗号分隔，如 "docs.example.com, app.example.com"；vite server.allowedHosts）
    #[serde(default)]
    pub allowed_hosts: String,
    /// 全局文档源（所有项目共享）
    #[serde(default)]
    pub sources: Vec<DocSource>,
    /// md 变更自动重建（默认 true；关掉则只手动重启服务刷新，浏览场景省 CPU）
    #[serde(default = "default_auto_rebuild")]
    pub auto_rebuild: bool,
}

fn default_auto_rebuild() -> bool { true }

/// 项目级配置（<项目>/.bgd/docs.json；只存与全局不同的项）
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ProjectConfig {
    /// 本项目追加的文档源（与全局 sources 合并）
    #[serde(default)]
    pub sources: Vec<DocSource>,
    /// 是否禁用全局文档源（默认 false：全局 + 项目 合并）
    #[serde(default)]
    pub disable_global_sources: bool,
}

/// 项目配置文件路径
fn project_config_path(project_root: &Path) -> PathBuf {
    project_root.join(".bgd").join("docs.json")
}

/// 读全局配置（不存在返回默认）
pub fn read_global() -> GlobalConfig {
    bgd_appsdk::config::read()
        .as_object()
        .and_then(|o| serde_json::from_value(serde_json::Value::Object(o.clone())).ok())
        .unwrap_or_default()
}

/// 写全局配置（与 appsdk 的 last_project_path 共存：合并写回）
pub fn write_global(cfg: &GlobalConfig) {
    let mut all = bgd_appsdk::config::read();
    let patch = serde_json::to_value(cfg).unwrap_or_default();
    if let (serde_json::Value::Object(base), serde_json::Value::Object(p)) = (&mut all, patch) {
        for (k, v) in p {
            base.insert(k, v);
        }
    }
    bgd_appsdk::config::write(&all);
}

/// 读项目配置（不存在/项目无效返回默认）
pub fn read_project(project_root: &Path) -> ProjectConfig {
    let p = project_config_path(project_root);
    std::fs::read_to_string(p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// 写项目配置
pub fn write_project(project_root: &Path, cfg: &ProjectConfig) -> std::io::Result<()> {
    let p = project_config_path(project_root);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&p, serde_json::to_string_pretty(cfg).unwrap_or_default())
}

/// 生效文档源合并：全局（未禁用）+ 项目追加；同名项目级覆盖全局
pub fn effective_sources(global: &GlobalConfig, project: &ProjectConfig) -> Vec<DocSource> {
    let mut out: Vec<DocSource> = Vec::new();
    if !project.disable_global_sources {
        out.extend(global.sources.iter().cloned());
    }
    for s in &project.sources {
        if let Some(g) = out.iter_mut().find(|g| g.name == s.name) {
            *g = s.clone(); // 项目级同名覆盖全局
        } else {
            out.push(s.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gs(name: &str, path: &str) -> DocSource {
        DocSource { name: name.into(), path: path.into() }
    }

    #[test]
    fn merge_global_plus_project() {
        let g = GlobalConfig { sources: vec![gs("api", ".bgd/doc/api_generated")], ..Default::default() };
        let p = ProjectConfig { sources: vec![gs("research", ".bgd/doc/research")], ..Default::default() };
        let eff = effective_sources(&g, &p);
        assert_eq!(eff.len(), 2);
        assert_eq!(eff[0].name, "api");
        assert_eq!(eff[1].name, "research");
    }

    #[test]
    fn project_overrides_global_same_name() {
        let g = GlobalConfig { sources: vec![gs("api", "old/path")], ..Default::default() };
        let p = ProjectConfig { sources: vec![gs("api", "new/path")], ..Default::default() };
        let eff = effective_sources(&g, &p);
        assert_eq!(eff.len(), 1);
        assert_eq!(eff[0].path, "new/path");
    }

    #[test]
    fn disable_global_sources() {
        let g = GlobalConfig { sources: vec![gs("api", ".bgd/doc/api_generated")], ..Default::default() };
        let p = ProjectConfig { sources: vec![gs("own", "doc/own")], disable_global_sources: true };
        let eff = effective_sources(&g, &p);
        assert_eq!(eff.len(), 1);
        assert_eq!(eff[0].name, "own");
    }

    #[test]
    fn project_config_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("bgd_docs_cfg_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let cfg = ProjectConfig {
            sources: vec![gs("research", ".bgd/doc/research")],
            disable_global_sources: false,
        };
        write_project(&tmp, &cfg).unwrap();
        let back = read_project(&tmp);
        assert_eq!(back, cfg);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn read_missing_project_returns_default() {
        let tmp = std::env::temp_dir().join("bgd_docs_cfg_missing");
        let back = read_project(&tmp);
        assert_eq!(back, ProjectConfig::default());
    }
}
