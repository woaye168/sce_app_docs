// md_guard.mjs —— 构建期防护：把「会炸 Vue 编译的裸标签」转义为实体（markdown-it 插件 + 纯函数，零依赖）
// ============================================================================
// 【干什么】VitePress 把 md 编译成 Vue 组件（vite:vue）。md 表格/正文里 `Sync_<Model>`、
//           `Array<Player>` 这类「看起来像标签」的文本会被 markdown-it 当成行内 HTML 原样
//           透传，Vue 模板解析器遇到未闭合元素直接 build 失败（Element is missing end tag）。
//           本守卫在 core 阶段（inline 之后）扫描 html_inline/html_block token 做**配对追踪**：
//           未闭合/未匹配闭合的标签 → < > 转实体（渲染为字面文本，与作者意图一致），
//           构建不再炸；配对完好/自闭合的标签一律保留。
// 【边界】  合法 HTML（<pre class="ftree">…</pre>、<br/>、<img>）原样保留；fence/代码块
//           token 不动（markdown-it 自己会转义 fence 内容）；mermaid fence 不动（其组件
//           标签是渲染期由插件生成，不经过本守卫）。md 里故意写 Vue 组件（如
//           <MermaidViewer />）配对完好即保留——未闭合的组件写法 Vue 也必炸，转义不亏。
//           配对完好的「假标签文本」（如 <Foo>…</Foo> 本意是字面文本）会按未知组件编译：
//           能构建、浏览器按未知元素渲染出子文本，仅两侧尖括号不显示——可接受的妥协。
// 【怎么用】config.mjs 的 markdown.config(md) 里调 mdAngleGuard(md)；
//           测试：node --test test/md_guard.test.mjs
// ============================================================================

// void 元素（无闭合标签概念，裸写合法，不参与配对追踪）
const VOID_TAGS = new Set(['area', 'base', 'br', 'col', 'embed', 'hr', 'img', 'input', 'link', 'meta', 'source', 'track', 'wbr'])

// 标签记号匹配（容忍属性里的引号与 > 号；注释 <!-- --> 不匹配，原样通过）
const TAG_RE = /<(\/?)([a-zA-Z][\w-]*)((?:"[^"]*"|'[^']*'|[^>"'])*)>/g

function escapeTag(tagText) {
  return tagText.replace(/</g, '&lt;').replace(/>/g, '&gt;')
}

/** 收集 token 流中的全部原始 HTML 标签出现位置（文档顺序） */
function collectTagOccurrences(tokens) {
  const occ = []
  const visit = (token) => {
    if (token.type !== 'html_block' && token.type !== 'html_inline') return
    TAG_RE.lastIndex = 0
    let m
    while ((m = TAG_RE.exec(token.content))) {
      occ.push({
        token,
        start: m.index,
        text: m[0],
        name: m[2].toLowerCase(),
        closing: m[1] === '/',
        selfClosing: /\/\s*>$/.test(m[0]),
      })
    }
  }
  for (const token of tokens) {
    visit(token)
    if (token.type === 'inline' && token.children) {
      for (const child of token.children) visit(child)
    }
  }
  return occ
}

/**
 * 扫描 markdown-it token 流，把会炸 Vue 编译的标签转义为实体（纯函数，原地修改 token.content）。
 * 规则（配对追踪，不看标签名）：闭合标签未匹配栈顶 → 转义；文档结束仍未闭合的开标签 → 转义；
 * 配对完好/自闭合/void 标签一律保留（合法 HTML 与故意写的 Vue 组件都因此不受影响）。
 */
export function guardHtmlTokens(tokens) {
  const occ = collectTagOccurrences(tokens)
  const escapeIdx = new Set()
  const stack = [] // occ 下标：已开未闭的标签
  for (let i = 0; i < occ.length; i++) {
    const o = occ[i]
    if (o.closing) {
      if (stack.length && occ[stack[stack.length - 1]].name === o.name) stack.pop()
      else escapeIdx.add(i)
    } else if (!o.selfClosing && !VOID_TAGS.has(o.name)) {
      stack.push(i)
    }
  }
  for (const i of stack) escapeIdx.add(i) // 文档结束仍未闭合
  // 按 token 分组、从后往前替换（避免索引漂移）
  const byToken = new Map()
  for (const i of escapeIdx) {
    const o = occ[i]
    if (!byToken.has(o.token)) byToken.set(o.token, [])
    byToken.get(o.token).push(o)
  }
  for (const [token, list] of byToken) {
    list.sort((a, b) => b.start - a.start)
    for (const o of list) {
      token.content = token.content.slice(0, o.start) + escapeTag(o.text) + token.content.slice(o.start + o.text.length)
    }
  }
}

/** markdown-it 插件：core 阶段（inline 之后）挂守卫 */
export function mdAngleGuard(md) {
  md.core.ruler.after('inline', 'bgd-angle-guard', (state) => {
    guardHtmlTokens(state.tokens)
  })
}
