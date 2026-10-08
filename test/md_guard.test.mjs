// md_guard 纯函数测试：node --test test/md_guard.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { guardHtmlTokens } from '../frontend/md_guard.mjs'

// 构造极简 token（对齐 markdown-it 形态：顶层 html_block / inline.children 里 html_inline）
const htmlBlock = (content) => ({ type: 'html_block', content })
const inlineWith = (...children) => ({ type: 'inline', children })
const htmlInline = (content) => ({ type: 'html_inline', content })
const text = (content) => ({ type: 'text', content })

test('表格单元格里的 <Model>（本次实炸场景）→ 转义', () => {
  const cell = inlineWith(text('server/sync_push 唯一调用点（Sync_'), htmlInline('<Model>'), text(' 集中生成）'))
  guardHtmlTokens([cell])
  assert.equal(cell.children[1].content, '&lt;Model&gt;')
  assert.equal(cell.children[0].content, 'server/sync_push 唯一调用点（Sync_')
})

test('未闭合的未知标签 <model> → 转义', () => {
  const t = inlineWith(text('models.'), htmlInline('<name>'), text(' 客户端操作面'))
  guardHtmlTokens([t])
  assert.equal(t.children[1].content, '&lt;name&gt;')
})

test('配对完好的未知标签按 Vue 组件保留（<MermaidViewer /> 等故意组件写法不受防护影响）', () => {
  const t = inlineWith(htmlInline('<Foo>'), text('bar'), htmlInline('</Foo>'))
  const s = htmlBlock('<MermaidViewer chart="graph TD;" />')
  guardHtmlTokens([t, s])
  assert.equal(t.children[0].content, '<Foo>')
  assert.equal(t.children[2].content, '</Foo>')
  assert.equal(s.content, '<MermaidViewer chart="graph TD;" />')
})

test('配对完好的合法 HTML 块（ftree 场景）→ 不动', () => {
  const b = htmlBlock('<pre class="ftree">\n<span class="ft-dir">.bgd/</span>\n</pre>\n')
  guardHtmlTokens([b])
  assert.equal(b.content, '<pre class="ftree">\n<span class="ft-dir">.bgd/</span>\n</pre>\n')
})

test('自闭合 <br/> 与 void <img> → 不动', () => {
  const t = inlineWith(htmlInline('<br/>'), text('x'), htmlInline('<img src="a.png">'))
  guardHtmlTokens([t])
  assert.equal(t.children[0].content, '<br/>')
  assert.equal(t.children[2].content, '<img src="a.png">')
})

test('白名单但未闭合 <b> → 转义', () => {
  const t = inlineWith(htmlInline('<b>'), text('bold'))
  guardHtmlTokens([t])
  assert.equal(t.children[0].content, '&lt;b&gt;')
})

test('未匹配的闭合 </b> → 转义', () => {
  const t = inlineWith(text('text'), htmlInline('</b>'))
  guardHtmlTokens([t])
  assert.equal(t.children[1].content, '&lt;/b&gt;')
})

test('跨 token 配对：html_block 开 <div>、后续块闭合 → 不动', () => {
  const open = htmlBlock('<div class="x">')
  const close = htmlBlock('</div>')
  guardHtmlTokens([open, inlineWith(text('中间')), close])
  assert.equal(open.content, '<div class="x">')
  assert.equal(close.content, '</div>')
})

test('属性里含 > 号的合法标签 → 不动', () => {
  const t = inlineWith(htmlInline('<a title="a>b">'), text('x'), htmlInline('</a>'))
  guardHtmlTokens([t])
  assert.equal(t.children[0].content, '<a title="a>b">')
})

test('fence token 不动（markdown-it 自己会转义 fence 内容）', () => {
  const f = { type: 'fence', content: "rpc.call('<model>.<action>', args)" }
  guardHtmlTokens([f])
  assert.equal(f.content, "rpc.call('<model>.<action>', args)")
})

test('交错误嵌套 <b><i></b></i>：未匹配闭合与残留开标签都转义', () => {
  const t = inlineWith(htmlInline('<b>'), htmlInline('<i>'), htmlInline('</b>'), htmlInline('</i>'))
  guardHtmlTokens([t])
  assert.equal(t.children[2].content, '&lt;/b&gt;') // 栈顶是 i，</b> 未匹配
  assert.equal(t.children[3].content, '</i>')       // 栈顶 i 正常闭合
  assert.equal(t.children[0].content, '&lt;b&gt;')  // <b> 残留未闭合
  assert.equal(t.children[1].content, '<i>')
})
