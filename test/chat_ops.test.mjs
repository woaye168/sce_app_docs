// chat_ops 对话操作纯逻辑测试（node:test，零依赖）
// 运行：node --test test/chat_ops.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { truncateAt, retryFrom, toMarkdown } from '../frontend/chat_ops.mjs'

const convo = [
  { role: 'user', text: '问题1' },
  { role: 'assistant', text: '回答1', sources: [{ file: 'api/libs/common/co.md', heading: 'co > 成员一览', url: '/api/libs/common/co#成员一览' }] },
  { role: 'user', text: '问题2' },
  { role: 'assistant', text: '回答2', sources: [] },
]

test('truncateAt：删掉 idx 及之后全部消息', () => {
  const r = truncateAt(convo, 2)
  assert.equal(r.length, 2)
  assert.equal(r[1].text, '回答1')
  assert.equal(convo.length, 4, '原数组不可变')
})

test('retryFrom：找到该 AI 回复对应的用户消息，截到它之前并取出问题', () => {
  const r = retryFrom(convo, 3)
  assert.equal(r.question, '问题2')
  assert.equal(r.messages.length, 2, '截到触发用户消息之前')
})

test('retryFrom：AI 消息前没有用户消息 → null', () => {
  assert.equal(retryFrom([{ role: 'assistant', text: 'x' }], 0), null)
})

test('toMarkdown：用户/助手交替、正文原样、参考追加、代码 fence 保留', () => {
  const md = toMarkdown([
    { role: 'user', text: '给个例子' },
    { role: 'assistant', text: '看这里：\n\n```lua\nprint(1)\n```', sources: [{ file: 'a/b.md', heading: '', url: '/a/b' }] },
  ], new Date(2026, 9, 1, 21, 30))
  assert.ok(md.includes('## 用户'), md)
  assert.ok(md.includes('## 助手'), md)
  assert.ok(md.includes('```lua\nprint(1)\n```'), '代码 fence 原样保留: ' + md)
  assert.ok(md.includes('参考：b'), '参考取短名（去路径去 .md）: ' + md)
  assert.ok(md.includes('2026-10-01'), '导出时间: ' + md)
})

test('toMarkdown：空对话', () => {
  const md = toMarkdown([], new Date())
  assert.ok(md.includes('# '), md)
})
