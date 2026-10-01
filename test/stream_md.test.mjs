// stream_md 流式分段解析器测试（node:test，零依赖）
// 运行：node --test test/stream_md.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { scanStream, vpFence } from '../frontend/stream_md.mjs'

test('无 fence：全文一个 md 段', () => {
  const segs = scanStream('你好\n\n世界')
  assert.deepEqual(segs, [{ type: 'md', text: '你好\n\n世界', key: 'md0' }])
})

test('闭合 lua fence：md + code + md 三段，代码段带 fence 原文', () => {
  const t = '介绍\n\n```lua\nprint(1)\n```\n\n收尾'
  const segs = scanStream(t)
  assert.equal(segs.length, 3)
  assert.equal(segs[0].type, 'md'); assert.equal(segs[0].text, '介绍\n\n')
  assert.equal(segs[1].type, 'code'); assert.equal(segs[1].text, '```lua\nprint(1)\n```')
  assert.equal(segs[1].key, 'cc0')
  assert.equal(segs[2].type, 'md'); assert.equal(segs[2].text, '\n\n收尾')
})

test('mermaid fence 识别为 mermaid 段', () => {
  const segs = scanStream('```mermaid\ngraph LR\nA-->B\n```\n')
  assert.equal(segs.length, 1)
  assert.equal(segs[0].type, 'mermaid')
  assert.equal(segs[0].graph, 'graph LR\nA-->B')
  assert.equal(segs[0].key, 'mm0')
})

test('未闭合 fence 留在 md 尾部（流式中途不切代码段）', () => {
  const segs = scanStream('前面\n\n```lua\nprint(1)\n还在写')
  assert.equal(segs.length, 1)
  assert.equal(segs[0].type, 'md')
  assert.ok(segs[0].text.endsWith('还在写'))
})

test('多个 fence：序号 key 稳定——后闭合的 fence 不影响先前的 key', () => {
  const t1 = '```lua\na\n```\n\n中间\n\n```lua\nb\n还在写'
  const s1 = scanStream(t1)
  assert.equal(s1.filter(s => s.type === 'code').length, 1)
  assert.equal(s1.find(s => s.type === 'code').key, 'cc0')

  const t2 = '```lua\na\n```\n\n中间\n\n```lua\nb\n```\n\n后面'
  const s2 = scanStream(t2)
  const codes = s2.filter(s => s.type === 'code')
  assert.equal(codes.length, 2)
  assert.equal(codes[0].key, 'cc0') // 关键：第一个代码段 key 不变（Vue 复用组件不重挂载）
  assert.equal(codes[1].key, 'cc1')
})

test('fence 在开头/结尾的边界', () => {
  const segs = scanStream('```lua\nx\n```')
  assert.equal(segs.length, 1)
  assert.equal(segs[0].type, 'code')
})

test('空文本', () => {
  assert.deepEqual(scanStream(''), [])
})

// vpFence：markdown-it fence 渲染规则，产出 VitePress vp-doc 期望的包裹结构
//（div.language-xxx + span.lang 语言标签），让气泡内代码块吃 VP 主题样式
test('vpFence 产出 VP 结构：language- 包裹 + lang 标签 + 转义', () => {
  const html = vpFence([{ info: 'lua', content: 'local a = "<x>"\n' }], 0)
  assert.ok(html.startsWith('<div class="language-lua">'), html)
  assert.ok(html.includes('<span class="lang">lua</span>'), html)
  assert.ok(html.includes('&lt;x&gt;'), '内容必须转义: ' + html)
  assert.ok(html.includes('<pre><code class="language-lua">'), html)
})

test('vpFence 无语言标记时落 text', () => {
  const html = vpFence([{ info: '', content: 'hi\n' }], 0)
  assert.ok(html.startsWith('<div class="language-text">'), html)
})
