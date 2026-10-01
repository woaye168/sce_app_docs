// stream_md.mjs —— 流式 markdown 分段扫描（纯函数，零依赖，node 可测）
// ============================================================================
// 【干什么】把流式增长的消息文本按「已闭合 fence」切成 md/code/mermaid 三类块：
//           已闭合的代码/图块带稳定序号 key（cc0/mm1…），供前端以 key 保组件身份
//           （fence 一闭合即高亮/渲染，之后流式更新不再重碰它）；未闭合尾部 fence
//           留在 md 段（markdown-it 对 EOF 未闭合 fence 天然按代码块渲染，流式中
//           代码也是实时成形的）。
// 【怎么用】import { scanStream } from './stream_md.mjs'；测试：node --test test/stream_md.test.mjs
// 【注意】  key 稳定是契约：后闭合的 fence 不得改变先前块的 key（否则 Vue 重挂载
//           导致已高亮代码/已渲染图被重置）。
// ============================================================================

const FENCE_RE = /```(\w*)[^\S\n]*\n([\s\S]*?)```/g

function escHtml(s) {
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;')
}

/**
 * markdown-it fence 渲染规则：产出 VitePress vp-doc 期望的代码块包裹结构
 *（div.language-xxx + button.copy 复制按钮 + span.lang 语言标签），气泡内代码块
 * 直接吃 VP 主题样式（底色/圆角/内边距/复制按钮/语言角标全主题自适应），不再自绘卡片。
 * 复制按钮的点击行为由 AiChat.vue 事件委托实现（VP 自身的 copy 绑定只管文档页）。
 * 用法：md.renderer.rules.fence = vpFence
 */
export function vpFence(tokens, idx) {
  const t = tokens[idx]
  const lang = (t.info || '').trim().split(/\s+/)[0] || 'text'
  return `<div class="language-${lang}">` +
    `<button title="Copy Code" class="copy" data-copied="已复制"></button><span class="lang">${lang}</span>` +
    `<pre><code class="language-${lang}">${escHtml(t.content)}</code></pre></div>\n`
}

/**
 * @param {string} text 当前累计的消息全文
 * @returns {Array<{type:'md'|'code'|'mermaid', text:string, key:string, graph?:string}>}
 */
export function scanStream(text) {
  const segs = []
  let last = 0, m, fenceIdx = 0
  FENCE_RE.lastIndex = 0
  while ((m = FENCE_RE.exec(text))) {
    if (m.index > last) segs.push({ type: 'md', text: text.slice(last, m.index), key: 'md' + segs.length })
    const lang = (m[1] || '').toLowerCase()
    if (lang === 'mermaid' || lang === 'mmd') {
      segs.push({ type: 'mermaid', text: m[0], graph: m[2].trim(), key: 'mm' + fenceIdx })
    } else {
      segs.push({ type: 'code', text: m[0], key: 'cc' + fenceIdx })
    }
    last = m.index + m[0].length
    fenceIdx++
  }
  const tailText = text.slice(last)
  if (tailText.trim()) segs.push({ type: 'md', text: tailText, key: 'md' + segs.length })
  return segs
}
