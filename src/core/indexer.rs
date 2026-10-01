//! 索引管线：扫站点 md → chunk → 按文件 hash 增量 → embed 变更文件 → upsert kb。
//! 全程后台线程跑（service 层调度），进度经共享状态暴露（/api/index_status、GUI 状态条）。
//! 索引失败不拖垮文档站——只降级问答能力。

use crate::core::{chunk, embed};
use crate::core::kb::{self, Kb};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// 索引状态（UI/HTTP 读取）
#[derive(Debug, Clone, serde::Serialize)]
pub struct IndexStatus {
    /// idle / indexing / ready / failed
    pub state: String,
    /// 已处理文件数（indexing 时）
    pub done: usize,
    /// 总文件数（indexing 时）
    pub total: usize,
    /// 就绪时的块数
    pub chunks: usize,
    /// 失败原因
    pub error: String,
    /// 最后更新时间（HH:MM:SS）
    pub updated_at: String,
}

impl Default for IndexStatus {
    fn default() -> Self {
        Self {
            state: "idle".into(),
            done: 0,
            total: 0,
            chunks: 0,
            error: String::new(),
            updated_at: String::new(),
        }
    }
}

pub type SharedIndexStatus = Arc<RwLock<IndexStatus>>;

pub fn new_shared_status() -> SharedIndexStatus {
    Arc::new(RwLock::new(IndexStatus::default()))
}

fn set_status(st: &SharedIndexStatus, state: &str, done: usize, total: usize, chunks: usize, error: &str) {
    if let Ok(mut s) = st.write() {
        s.state = state.into();
        s.done = done;
        s.total = total;
        s.chunks = chunks;
        s.error = error.into();
        s.updated_at = chrono_now();
    }
}

/// 简易本地时间 HH:MM:SS（不引入 chrono）
fn chrono_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() % 86400)
        .unwrap_or(0);
    // UTC+8
    let h = (secs / 3600 + 8) % 24;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

/// kb 文件路径（vitepress_home/kb/<站点目录名>.sqlite）
pub fn kb_path(home: &Path, site: &Path) -> PathBuf {
    let name = site.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "unknown".into());
    home.join("kb").join(format!("{name}.sqlite"))
}

/// 递归收集站点目录的 md 文件（相对路径 + 内容 + hash）。
/// 跳过 .vitepress/dist-*/build.log 等站点基建，只收文档内容（junction 源 + _root + index.md）。
fn collect_md_files(site: &Path) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    collect_recursive(site, site, &mut out);
    out
}

fn collect_recursive(base: &Path, dir: &Path, out: &mut Vec<(String, String, String)>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let path = e.path();
        let fname = e.file_name().to_string_lossy().to_string();
        // 跳过站点基建目录
        if fname == ".vitepress" || fname.starts_with("dist-") || fname == "node_modules" {
            continue;
        }
        if path.is_dir() {
            collect_recursive(base, &path, out);
        } else if fname.ends_with(".md") {
            if let Ok(content) = std::fs::read_to_string(&path) {
                let rel = path.strip_prefix(base).unwrap_or(&path).display().to_string().replace('\\', "/");
                let hash = format!("{:x}", content_hash(&content));
                out.push((rel, content, hash));
            }
        }
    }
}

fn content_hash(s: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// 索引同步（增量）：模型懒加载一次复用；只 embed hash 变更/新增的文件。
/// ctx 为共享锁：**逐文件瞬时持有**（embed 一个文件就放锁），检索/问答不被索引卡；
/// 2GB 模型加载在锁外做（双检装回），不阻塞检索。
/// 返回就绪块数。
pub fn sync_index(
    ctx: &std::sync::Mutex<SearchCtx>,
    status: &SharedIndexStatus,
) -> Result<usize, String> {
    let (home, site) = {
        let g = ctx.lock().map_err(|_| "检索上下文锁失败".to_string())?;
        (g.home.clone(), g.site.clone())
    };
    let files = collect_md_files(&site);
    let kb_file = kb_path(&home, &site);
    let mut kb = Kb::open(&kb_file)?;
    let existing = kb.file_hashes();

    // 变更检测：新增/hash 变化 → 待处理；库里多出的（文件已删）→ 移除
    let current_names: std::collections::HashSet<&str> = files.iter().map(|(f, _, _)| f.as_str()).collect();
    let stale: Vec<String> = existing.keys().filter(|f| !current_names.contains(f.as_str())).cloned().collect();
    for f in &stale {
        kb.remove_file(f)?;
    }
    let todo: Vec<&(String, String, String)> = files
        .iter()
        .filter(|(f, _, h)| existing.get(f).map(|old| old != h).unwrap_or(true))
        .collect();

    if todo.is_empty() {
        let n = kb.chunk_count();
        set_status(status, "ready", 0, 0, n, "");
        return Ok(n);
    }

    set_status(status, "indexing", 0, todo.len(), kb.chunk_count(), "");
    // 模型懒加载（只在有变更时才载 2GB 模型）：锁外加载 + 双检装回，不阻塞检索
    let need_load = ctx.lock().map_err(|_| "检索上下文锁失败")?.embedder.is_none();
    if need_load {
        let loaded = embed::load_embedder(&home)?;
        let mut g = ctx.lock().map_err(|_| "检索上下文锁失败")?;
        if g.embedder.is_none() {
            g.embedder = Some(loaded);
        }
    }

    for (i, (file, content, hash)) in todo.iter().enumerate() {
        let chunks = chunk::chunk_markdown(file, content, 500, 50);
        if chunks.is_empty() {
            kb.remove_file(file)?;
            continue;
        }
        let texts: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();
        // 锁只在本文件 embed 期间持有（百毫秒级），检索可穿插执行
        let vecs = {
            let mut g = ctx.lock().map_err(|_| "检索上下文锁失败")?;
            let emb = g.embedder.as_mut().ok_or("embedder 未就绪")?;
            emb.embed(texts, None).map_err(|e| {
                set_status(status, "failed", i, todo.len(), kb.chunk_count(), &e.to_string());
                format!("embed {file} 失败: {e}")
            })?
        };
        let records: Vec<kb::ChunkRecord> = chunks
            .into_iter()
            .zip(vecs)
            .map(|(c, v)| kb::ChunkRecord { file: c.file, heading: c.heading, text: c.text, url: c.url, embedding: v })
            .collect();
        kb.upsert_file(file, hash, &records)?;
        set_status(status, "indexing", i + 1, todo.len(), kb.chunk_count(), "");
    }
    let n = kb.chunk_count();
    set_status(status, "ready", todo.len(), todo.len(), n, "");
    Ok(n)
}

/// 全量重建索引（GUI「重建向量索引」按钮）：清空向量库全部条目后走增量同步——
/// sync_index 会发现所有文件都是「新增」从而全部重 embed。模型已常驻则秒级起跳。
pub fn force_reindex(
    ctx: &std::sync::Mutex<SearchCtx>,
    status: &SharedIndexStatus,
) -> Result<usize, String> {
    let (home, site) = {
        let g = ctx.lock().map_err(|_| "检索上下文锁失败".to_string())?;
        (g.home.clone(), g.site.clone())
    };
    let kb_file = kb_path(&home, &site);
    let mut kb = Kb::open(&kb_file)?;
    let all: Vec<String> = kb.file_hashes().keys().cloned().collect();
    for f in &all {
        kb.remove_file(f)?;
    }
    drop(kb); // 释放连接再进 sync（Windows 下 sqlite 句柄不撒手会互相挡）
    sync_index(ctx, status)
}

/// 检索上下文（模型懒加载常驻复用；httpd 用 Mutex 包住共享）
pub struct SearchCtx {
    pub home: PathBuf,
    pub site: PathBuf,
    pub embedder: Option<fastembed::TextEmbedding>,
    pub reranker: Option<fastembed::TextRerank>,
}

impl SearchCtx {
    pub fn new(home: PathBuf, site: PathBuf) -> Self {
        Self { home, site, embedder: None, reranker: None }
    }
}

/// 语义检索 + 可选 rerank 重排（reranker 懒加载；检索 top_k*4 → rerank → top_k）
pub fn search_with_kb(
    ctx: &mut SearchCtx,
    query: &str,
    k: usize,
    use_rerank: bool,
) -> Result<Vec<kb::SearchHit>, String> {
    let kb_file = kb_path(&ctx.home, &ctx.site);
    let mut kb = Kb::open(&kb_file)?;
    if kb.chunk_count() == 0 {
        return Ok(Vec::new());
    }
    if ctx.embedder.is_none() {
        ctx.embedder = Some(embed::load_embedder(&ctx.home)?);
    }
    let qvec = ctx.embedder.as_mut().unwrap().embed(vec![query], None).map_err(|e| e.to_string())?.remove(0);
    let mut hits = kb.search(&qvec, if use_rerank { k * 4 } else { k });
    if use_rerank && !hits.is_empty() {
        if ctx.reranker.is_none() {
            ctx.reranker = embed::load_reranker(&ctx.home).ok();
        }
        if let Some(rr) = ctx.reranker.as_mut() {
            let docs: Vec<String> = hits.iter().map(|h| h.text.clone()).collect();
            if let Ok(ranked) = rr.rerank(query.to_string(), docs, false, None) {
                // rerank 结果按 score 重排 hits（index 对应原 hits 下标）
                let mut order: Vec<(usize, f32)> = ranked.iter().map(|r| (r.index, r.score)).collect();
                order.sort_by(|a, b| b.1.total_cmp(&a.1));
                let mut reranked: Vec<kb::SearchHit> = Vec::with_capacity(order.len());
                for (idx, score) in order {
                    let mut h = hits[idx].clone();
                    h.score = score;
                    reranked.push(h);
                }
                hits = reranked;
            }
        }
    }
    hits.truncate(k);
    Ok(hits)
}

/// 已索引文件清单（list_sources 工具用）
pub fn list_indexed(home: &Path, site: &Path) -> Vec<String> {
    Kb::open(&kb_path(home, site)).map(|kb| kb.list_files()).unwrap_or_default()
}

/// 读文档全文（get_doc 工具用；路径仅限已索引文件，天然防穿越）
pub fn get_doc(home: &Path, site: &Path, file: &str) -> Option<String> {
    Kb::open(&kb_path(home, site)).ok()?.get_doc(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_skips_infra_dirs() {
        let tmp = std::env::temp_dir().join(format!("bgd_idx_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("api")).unwrap();
        std::fs::create_dir_all(tmp.join(".vitepress")).unwrap();
        std::fs::create_dir_all(tmp.join("dist-a")).unwrap();
        std::fs::write(tmp.join("api/x.md"), "# X").unwrap();
        std::fs::write(tmp.join(".vitepress/config.md"), "# 不应收").unwrap();
        std::fs::write(tmp.join("dist-a/y.md"), "# 不应收").unwrap();
        let files = collect_md_files(&tmp);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, "api/x.md");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn hash_stable() {
        assert_eq!(content_hash("abc"), content_hash("abc"));
        assert_ne!(content_hash("abc"), content_hash("abd"));
    }
}
