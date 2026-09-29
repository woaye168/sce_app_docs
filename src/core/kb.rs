//! 向量知识库：sqlite 持久化 + 内存暴力余弦检索 + 按文件 hash 增量。
//! 文档量小（几千 chunk），暴力检索 <10ms，不引入向量数据库服务。
//!
//! 增量策略：调用方传入「文件 → (hash, chunks)」全量快照，sync 对比已有 hash——
//! 没变的文件不重嵌（零成本），变/删/新增的文件才动。

use rusqlite::Connection;
use std::collections::HashMap;
use std::path::Path;

/// 待索引的块（embedding 由调用方填）
#[derive(Debug, Clone)]
pub struct ChunkRecord {
    pub file: String,
    pub heading: String,
    pub text: String,
    pub url: String,
    pub embedding: Vec<f32>,
}

/// 检索命中
#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchHit {
    pub file: String,
    pub heading: String,
    pub text: String,
    pub url: String,
    pub score: f32,
}

/// 内存中的索引项（检索用）
struct MemChunk {
    file: String,
    heading: String,
    text: String,
    url: String,
    vec: Vec<f32>,
}

pub struct Kb {
    conn: Connection,
    /// 内存索引（sync 后失效重建）
    mem: Option<Vec<MemChunk>>,
}

impl Kb {
    /// 打开/创建知识库
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).map_err(|e| format!("创建 kb 目录失败: {e}"))?;
        }
        let conn = Connection::open(path).map_err(|e| format!("打开 kb 失败: {e}"))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS chunks (
                id INTEGER PRIMARY KEY,
                file TEXT NOT NULL,
                heading TEXT NOT NULL,
                text TEXT NOT NULL,
                url TEXT NOT NULL,
                embedding BLOB NOT NULL
            );
            CREATE TABLE IF NOT EXISTS files (
                file TEXT PRIMARY KEY,
                hash TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_chunks_file ON chunks(file);",
        )
        .map_err(|e| format!("建表失败: {e}"))?;
        Ok(Self { conn, mem: None })
    }

    /// 已有文件的 hash 快照（file → hash）
    pub fn file_hashes(&self) -> HashMap<String, String> {
        let mut out = HashMap::new();
        if let Ok(mut stmt) = self.conn.prepare("SELECT file, hash FROM files") {
            if let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))) {
                for r in rows.flatten() {
                    out.insert(r.0, r.1);
                }
            }
        }
        out
    }

    /// 删除文件的所有块 + hash 记录
    pub fn remove_file(&mut self, file: &str) -> Result<(), String> {
        self.conn.execute("DELETE FROM chunks WHERE file = ?1", [file]).map_err(|e| e.to_string())?;
        self.conn.execute("DELETE FROM files WHERE file = ?1", [file]).map_err(|e| e.to_string())?;
        self.mem = None;
        Ok(())
    }

    /// 写入文件的新块（先删旧），并登记 hash
    pub fn upsert_file(&mut self, file: &str, hash: &str, chunks: &[ChunkRecord]) -> Result<(), String> {
        self.remove_file(file)?;
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        for c in chunks {
            let blob = vec_to_blob(&c.embedding);
            tx.execute(
                "INSERT INTO chunks (file, heading, text, url, embedding) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![c.file, c.heading, c.text, c.url, blob],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.execute("INSERT OR REPLACE INTO files (file, hash) VALUES (?1, ?2)", [file, hash])
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(())
    }

    /// 块总数
    pub fn chunk_count(&self) -> usize {
        self.conn
            .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get::<_, i64>(0))
            .unwrap_or(0) as usize
    }

    /// 语义检索：暴力余弦 top-k
    pub fn search(&mut self, query: &[f32], k: usize) -> Vec<SearchHit> {
        self.ensure_mem();
        let mem = self.mem.as_deref().unwrap_or(&[]);
        let mut hits: Vec<SearchHit> = mem
            .iter()
            .map(|c| SearchHit {
                file: c.file.clone(),
                heading: c.heading.clone(),
                text: c.text.clone(),
                url: c.url.clone(),
                score: cosine(query, &c.vec),
            })
            .collect();
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        hits.truncate(k);
        hits
    }

    /// 读文档全文（get_doc 工具用）：按 file 聚合所有块文本（路径严格限定在已索引文件内）
    pub fn get_doc(&self, file: &str) -> Option<String> {
        let mut stmt = self.conn.prepare("SELECT text FROM chunks WHERE file = ?1 ORDER BY id").ok()?;
        let texts: Vec<String> = stmt.query_map([file], |r| r.get(0)).ok()?.flatten().collect();
        if texts.is_empty() {
            None
        } else {
            Some(texts.join("\n\n"))
        }
    }

    /// 列出已索引文件（list_sources/调试用）
    pub fn list_files(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(mut stmt) = self.conn.prepare("SELECT file FROM files ORDER BY file") {
            if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
                out.extend(rows.flatten());
            }
        }
        out
    }

    /// 内存索引失效重建
    fn ensure_mem(&mut self) {
        if self.mem.is_some() {
            return;
        }
        let mut items = Vec::new();
        if let Ok(mut stmt) = self.conn.prepare("SELECT file, heading, text, url, embedding FROM chunks") {
            if let Ok(rows) = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Vec<u8>>(4)?,
                ))
            }) {
                for r in rows.flatten() {
                    items.push(MemChunk { file: r.0, heading: r.1, text: r.2, url: r.3, vec: blob_to_vec(&r.4) });
                }
            }
        }
        self.mem = Some(items);
    }
}

/// f32 vec → LE bytes
fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// LE bytes → f32 vec
fn blob_to_vec(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

/// 余弦相似度
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    dot / ((na.sqrt() * nb.sqrt()) + 1e-12)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(file: &str, text: &str, vec: Vec<f32>) -> ChunkRecord {
        ChunkRecord {
            file: file.into(),
            heading: "H".into(),
            text: text.into(),
            url: "/x".into(),
            embedding: vec,
        }
    }

    fn tmp_kb(tag: &str) -> (Kb, std::path::PathBuf) {
        let p = std::env::temp_dir().join(format!("bgd_kb_{}_{}.sqlite", tag, std::process::id()));
        let _ = std::fs::remove_file(&p);
        (Kb::open(&p).unwrap(), p)
    }

    #[test]
    fn upsert_and_search_ranking() {
        let (mut kb, path) = tmp_kb("rank");
        kb.upsert_file("a.md", "h1", &[
            rec("a.md", "苹果", vec![1.0, 0.0, 0.0]),
            rec("a.md", "香蕉", vec![0.0, 1.0, 0.0]),
        ])
        .unwrap();
        kb.upsert_file("b.md", "h2", &[rec("b.md", "苹果派", vec![0.9, 0.1, 0.0])]).unwrap();
        assert_eq!(kb.chunk_count(), 3);
        let hits = kb.search(&[1.0, 0.0, 0.0], 2);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].text, "苹果");
        assert_eq!(hits[1].text, "苹果派"); // 0.9 方向次之
        assert!(hits[0].score > hits[1].score);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn incremental_by_file_hash() {
        let (mut kb, path) = tmp_kb("incr");
        kb.upsert_file("a.md", "h1", &[rec("a.md", "v1", vec![1.0, 0.0])]).unwrap();
        // 模拟增量决策：hash 没变的不动，变了的才 upsert
        let hashes = kb.file_hashes();
        assert_eq!(hashes.get("a.md").map(|s| s.as_str()), Some("h1"));
        // 文件变了 → 调用方 upsert 新 hash
        kb.upsert_file("a.md", "h2", &[rec("a.md", "v2", vec![0.0, 1.0])]).unwrap();
        assert_eq!(kb.chunk_count(), 1, "同文件 upsert 应替换旧块");
        let hits = kb.search(&[0.0, 1.0], 1);
        assert_eq!(hits[0].text, "v2");
        // 删除文件
        kb.remove_file("a.md").unwrap();
        assert_eq!(kb.chunk_count(), 0);
        assert!(kb.get_doc("a.md").is_none());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn get_doc_aggregates_chunks() {
        let (mut kb, path) = tmp_kb("doc");
        kb.upsert_file("a.md", "h", &[rec("a.md", "第一段", vec![1.0]), rec("a.md", "第二段", vec![1.0])]).unwrap();
        let doc = kb.get_doc("a.md").unwrap();
        assert!(doc.contains("第一段") && doc.contains("第二段"));
        assert!(kb.get_doc("no.md").is_none());
        assert_eq!(kb.list_files(), vec!["a.md"]);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn blob_roundtrip() {
        let v = vec![1.5f32, -2.25, 3.125];
        assert_eq!(blob_to_vec(&vec_to_blob(&v)), v);
    }
}
