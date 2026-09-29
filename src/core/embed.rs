//! 本地向量/重排模型管理（fastembed user-defined 加载，零外部服务）：
//! - 模型目录：vitepress_home/models/<模型名>/
//! - 导入本地模型：用户预下载（motrix/下载器）后选目录装入（复制+校验必需文件）
//! - 在线下载兜底：fastembed 自带下载（HF_ENDPOINT 镜像 / HTTPS_PROXY 代理）

use fastembed::{InitOptionsUserDefined, TextEmbedding, TokenizerFiles, UserDefinedEmbeddingModel};
use std::path::{Path, PathBuf};

/// 模型种类
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ModelKind {
    /// 向量化（BAAI/bge-m3，onnx/ 子目录 7 文件）
    Embedding,
    /// 重排（rozgo/bge-reranker-v2-m3，根目录 8 文件）
    Reranker,
}

impl ModelKind {
    pub fn dir_name(&self) -> &'static str {
        match self {
            Self::Embedding => "bge-m3",
            Self::Reranker => "bge-reranker-v2-m3",
        }
    }
    /// 在线下载地址（展示给用户 / 兜底下载用）
    pub fn download_url(&self) -> &'static str {
        match self {
            Self::Embedding => "https://hf-mirror.com/BAAI/bge-m3",
            Self::Reranker => "https://hf-mirror.com/rozgo/bge-reranker-v2-m3",
        }
    }
    /// 必需文件（导入校验用；Embedding 的 onnx/ 子目录会拍平）
    fn required_files(&self) -> &'static [&'static str] {
        match self {
            Self::Embedding => &["model.onnx", "model.onnx_data", "tokenizer.json", "config.json", "special_tokens_map.json", "tokenizer_config.json", "sentencepiece.bpe.model"],
            Self::Reranker => &["model.onnx", "model.onnx.data", "ort_config.json", "tokenizer.json", "config.json", "special_tokens_map.json", "tokenizer_config.json", "sentencepiece.bpe.model"],
        }
    }
}

/// 模型根目录（vitepress_home/models/）
pub fn models_dir(home: &Path) -> PathBuf {
    home.join("models")
}

/// 模型在本机的目标目录
pub fn model_dir(home: &Path, kind: ModelKind) -> PathBuf {
    models_dir(home).join(kind.dir_name())
}

/// 模型是否就绪（必需文件齐全）
pub fn model_ready(home: &Path, kind: ModelKind) -> bool {
    let dir = model_dir(home, kind);
    kind.required_files().iter().all(|f| dir.join(f).is_file())
}

/// 导入本地模型：src 可以是「含 onnx/ 子目录的仓库目录」或「文件平铺目录」。
/// 复制必需文件到 vitepress_home/models/<模型名>/（Embedding 拍平 onnx/ 子目录）。
pub fn import_local(home: &Path, kind: ModelKind, src: &Path) -> Result<PathBuf, String> {
    // 源目录定位：Embedding 优先用 onnx/ 子目录
    let src = if kind == ModelKind::Embedding && src.join("onnx/model.onnx").is_file() {
        src.join("onnx")
    } else {
        src.to_path_buf()
    };
    for f in kind.required_files() {
        if !src.join(f).is_file() {
            return Err(format!("导入目录缺文件 {f}（{} 需要 {:?}）", src.display(), kind.required_files()));
        }
    }
    let dst = model_dir(home, kind);
    std::fs::create_dir_all(&dst).map_err(|e| format!("创建模型目录失败: {e}"))?;
    for f in kind.required_files() {
        std::fs::copy(src.join(f), dst.join(f)).map_err(|e| format!("复制 {f} 失败: {e}"))?;
    }
    Ok(dst)
}

fn read_tokenizer_files(dir: &Path) -> Result<TokenizerFiles, String> {
    let read = |name: &str| std::fs::read(dir.join(name)).map_err(|e| format!("读 {name} 失败: {e}"));
    Ok(TokenizerFiles {
        tokenizer_file: read("tokenizer.json")?,
        config_file: read("config.json")?,
        special_tokens_map_file: read("special_tokens_map.json")?,
        tokenizer_config_file: read("tokenizer_config.json")?,
    })
}

/// 加载 embedding 模型（bge-m3；model.onnx_data 作为 external initializer 挂载）
pub fn load_embedder(home: &Path) -> Result<TextEmbedding, String> {
    let dir = model_dir(home, ModelKind::Embedding);
    if !model_ready(home, ModelKind::Embedding) {
        return Err(format!(
            "embedding 模型未就绪：请先导入或下载（{}）",
            ModelKind::Embedding.download_url()
        ));
    }
    let onnx = std::fs::read(dir.join("model.onnx")).map_err(|e| format!("读 model.onnx 失败: {e}"))?;
    let data = std::fs::read(dir.join("model.onnx_data")).map_err(|e| format!("读 model.onnx_data 失败: {e}"))?;
    let mut model = UserDefinedEmbeddingModel::new(onnx, read_tokenizer_files(&dir)?)
        .with_external_initializer("model.onnx_data".to_string(), data);
    if let Some(pooling) = TextEmbedding::get_default_pooling_method(&fastembed::EmbeddingModel::BGEM3) {
        model = model.with_pooling(pooling);
    }
    TextEmbedding::try_new_from_user_defined(model, InitOptionsUserDefined::default())
        .map_err(|e| format!("加载 embedding 模型失败: {e}"))
}

/// 加载 reranker 模型（bge-reranker-v2-m3；OnnxSource::File 从磁盘加载，外部数据文件自动相对解析）
pub fn load_reranker(home: &Path) -> Result<fastembed::TextRerank, String> {
    let dir = model_dir(home, ModelKind::Reranker);
    if !model_ready(home, ModelKind::Reranker) {
        return Err(format!(
            "reranker 模型未就绪：请先导入或下载（{}）",
            ModelKind::Reranker.download_url()
        ));
    }
    let model = fastembed::UserDefinedRerankingModel::new(dir.join("model.onnx"), read_tokenizer_files(&dir)?);
    fastembed::TextRerank::try_new_from_user_defined(model, fastembed::RerankInitOptionsUserDefined::default())
        .map_err(|e| format!("加载 reranker 模型失败: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 本机预下载的模型（motrix 预下）；不在则跳过（CI/他机兼容）
    fn local_model_dir(kind: ModelKind) -> Option<PathBuf> {
        let p = PathBuf::from(r"D:\local_models").join(kind.dir_name());
        p.is_dir().then_some(p)
    }

    fn test_home(tag: &str) -> PathBuf {
        let h = std::env::temp_dir().join(format!("bgd_embed_home_{tag}"));
        h
    }

    #[test]
    fn import_rejects_incomplete_dir() {
        let home = test_home("reject");
        let src = std::env::temp_dir().join("bgd_embed_src_empty");
        std::fs::create_dir_all(&src).unwrap();
        let r = import_local(&home, ModelKind::Embedding, &src);
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("缺文件"));
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&src);
    }

    #[test]
    fn embedder_loads_and_semantics_work() {
        let Some(src) = local_model_dir(ModelKind::Embedding) else { return };
        let home = test_home("bge");
        let _ = std::fs::remove_dir_all(&home);
        import_local(&home, ModelKind::Embedding, &src).unwrap();
        assert!(model_ready(&home, ModelKind::Embedding));
        let mut emb = load_embedder(&home).unwrap();
        let vecs = emb
            .embed(vec!["sql_toolkit 是数据库工具模块", "今天天气真好", "数据库 SQL 工具集"], None)
            .unwrap();
        assert_eq!(vecs.len(), 3);
        assert_eq!(vecs[0].len(), 1024, "bge-m3 输出 1024 维");
        let sim = |a: &[f32], b: &[f32]| {
            let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
            dot // 已 normalize，点积即余弦
        };
        assert!(
            sim(&vecs[0], &vecs[2]) > sim(&vecs[0], &vecs[1]),
            "相关文本相似度应高于无关文本"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn reranker_loads_and_ranks() {
        let Some(src) = local_model_dir(ModelKind::Reranker) else { return };
        let home = test_home("rerank");
        let _ = std::fs::remove_dir_all(&home);
        import_local(&home, ModelKind::Reranker, &src).unwrap();
        let mut rr = load_reranker(&home).unwrap();
        let results = rr
            .rerank("sql_toolkit 怎么用", vec!["sql_toolkit 提供 SQL 操作的封装", "今天天气很好"], true, None)
            .unwrap();
        assert_eq!(results.len(), 2);
        assert!(results[0].document.as_deref().unwrap_or("").contains("sql_toolkit"), "最相关文档应排第一");
        let _ = std::fs::remove_dir_all(&home);
    }
}
