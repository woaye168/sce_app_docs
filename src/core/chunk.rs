//! Markdown 分块：按标题层级切段，超长段按段落边界再切（~500 字目标，50 字重叠）。
//! 每块带元数据：源文件相对路径 / 标题链 / 文档站 URL（问答出处链接用）。

/// 一个文档块
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    /// 源文件相对站点根路径（如 api/libs/common/json.md）
    pub file: String,
    /// 标题链（如 "json 模块 > 编码接口"）
    pub heading: String,
    /// 块文本（含标题上下文前缀，embedding 用）
    pub text: String,
    /// 文档站 URL（如 /api/libs/common/json）
    pub url: String,
}

/// 文件路径 → 文档站 URL（index.md → 目录；x.md → /x）
pub fn file_to_url(file: &str) -> String {
    let no_ext = file.strip_suffix(".md").unwrap_or(file);
    let stripped = no_ext.strip_suffix("/index").unwrap_or(no_ext);
    let stripped = if stripped == "index" { "" } else { stripped };
    if stripped.is_empty() {
        "/".to_string()
    } else {
        format!("/{stripped}")
    }
}

/// 分块主入口：一个 md 文件 → 若干块。
/// max_len 目标块长（字符），overlap 重叠（字符）。
pub fn chunk_markdown(file: &str, content: &str, max_len: usize, overlap: usize) -> Vec<Chunk> {
    let url = file_to_url(file);
    // 先按标题切段（保留标题链）
    let sections = split_by_headings(content);
    let mut out = Vec::new();
    for (heading_chain, body) in sections {
        let body = body.trim();
        if body.is_empty() {
            continue;
        }
        // 超长段按段落边界再切
        for piece in split_long_section(body, max_len, overlap) {
            // embedding 文本带标题上下文（检索质量关键：单独一块裸文本语义不全）
            let text = if heading_chain.is_empty() {
                piece.clone()
            } else {
                format!("[{heading_chain}]\n{piece}")
            };
            out.push(Chunk {
                file: file.to_string(),
                heading: heading_chain.clone(),
                text,
                url: url.clone(),
            });
        }
    }
    out
}

/// 按标题切段：返回 [(标题链, 段正文)]。标题行本身不进正文（进了 text 前缀）。
fn split_by_headings(content: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut heading_stack: Vec<(usize, String)> = Vec::new(); // (级别, 标题)
    let mut cur = String::new();
    let mut in_code = false;
    for line in content.lines() {
        let trimmed = line.trim_start();
        // 代码块里的 # 不算标题
        if trimmed.starts_with("```") {
            in_code = !in_code;
            cur.push_str(line);
            cur.push('\n');
            continue;
        }
        let level = if !in_code && trimmed.starts_with('#') {
            let hashes = trimmed.chars().take_while(|c| *c == '#').count();
            if trimmed.chars().nth(hashes) == Some(' ') { Some(hashes) } else { None }
        } else {
            None
        };
        if let Some(level) = level {
            // 收上一段
            if !cur.trim().is_empty() || !out.is_empty() {
                out.push((chain_string(&heading_stack), std::mem::take(&mut cur)));
            } else {
                cur.clear();
            }
            let title = trimmed[level..].trim().to_string();
            heading_stack.retain(|(l, _)| *l < level);
            heading_stack.push((level, title));
        } else {
            cur.push_str(line);
            cur.push('\n');
        }
    }
    if !cur.trim().is_empty() {
        out.push((chain_string(&heading_stack), cur));
    }
    out
}

fn chain_string(stack: &[(usize, String)]) -> String {
    stack.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join(" > ")
}

/// 超长段按段落（空行）边界切成 <= max_len 的块，相邻块重叠 overlap 字符
fn split_long_section(body: &str, max_len: usize, overlap: usize) -> Vec<String> {
    if body.chars().count() <= max_len {
        return vec![body.to_string()];
    }
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for para in body.split("\n\n") {
        let para = para.trim();
        if para.is_empty() {
            continue;
        }
        let cur_len = cur.chars().count();
        let para_len = para.chars().count();
        if cur_len > 0 && cur_len + para_len + 2 > max_len {
            out.push(std::mem::take(&mut cur));
            // 重叠：取上一块尾部 overlap 字符
            let tail: String = out.last().unwrap().chars().rev().take(overlap).collect::<Vec<_>>().into_iter().rev().collect();
            cur = tail.trim_start().to_string();
            if !cur.is_empty() {
                cur.push_str("\n\n");
            }
        }
        cur.push_str(para);
        cur.push_str("\n\n");
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_mapping() {
        assert_eq!(file_to_url("index.md"), "/");
        assert_eq!(file_to_url("api/index.md"), "/api");
        assert_eq!(file_to_url("api/libs/common/json.md"), "/api/libs/common/json");
    }

    #[test]
    fn splits_by_heading_chain() {
        let md = "# 模块A\n\n介绍。\n\n## 接口一\n\n接口一内容。\n\n## 接口二\n\n接口二内容。\n";
        let chunks = chunk_markdown("a/b.md", md, 500, 50);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[1].heading, "模块A > 接口一");
        assert!(chunks[1].text.contains("[模块A > 接口一]"));
        assert!(chunks[1].text.contains("接口一内容"));
        assert_eq!(chunks[1].url, "/a/b");
    }

    #[test]
    fn hash_in_code_block_not_heading() {
        let md = "# 标题\n\n```lua\n-- # 注释不是标题\nprint(1)\n```\n";
        let chunks = chunk_markdown("x.md", md, 500, 50);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].text.contains("# 注释不是标题"));
    }

    #[test]
    fn long_section_splits_with_overlap() {
        // 造一个 3 段落超长文档（每段 300 字）
        let para = "这是一段中文内容，用来测试分块逻辑。".repeat(15); // ~285 字
        let md = format!("# 长文档\n\n{para}\n\n{para}\n\n{para}");
        let chunks = chunk_markdown("long.md", &md, 400, 50);
        assert!(chunks.len() >= 2, "超长应切块，实际 {} 块", chunks.len());
        // 重叠：后一块开头应含前一块尾部内容
        assert!(chunks.windows(2).all(|w| {
            let tail: String = w[0].text.chars().rev().take(30).collect::<Vec<_>>().into_iter().rev().collect();
            w[1].text.contains(tail.trim().chars().take(10).collect::<String>().as_str())
        }), "相邻块应有重叠");
    }

    #[test]
    fn empty_sections_skipped() {
        let md = "# 只有标题\n\n## 子标题\n";
        let chunks = chunk_markdown("e.md", md, 500, 50);
        assert!(chunks.is_empty(), "无正文不产生块");
    }
}
