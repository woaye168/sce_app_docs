// chat_ops.mjs —— 对话操作纯逻辑（截断/重试/导出 markdown；零依赖，node 可测）
// ============================================================================
// 【干什么】聊天气泡按钮组与头部按钮背后的纯逻辑：
//           truncateAt（删除=截断到 idx 前）、retryFrom（重试=找触发用户消息并截回）、
//           toMarkdown（导出整段对话为 md 文件内容）。
// 【怎么用】AiChat.vue import；测试：node --test test/chat_ops.test.mjs
// 【注意】  全部不可变操作（不动入参数组）；消息对象形态见 AiChat.vue（role/text/sources?）。
// ============================================================================

/** 删除：截掉 idx 及之后的全部消息（返回新数组） */
export function truncateAt(messages, idx) {
  return messages.slice(0, idx)
}

/**
 * 重试：aiIdx 是一条 assistant 消息的下标。找到它前面最近的 user 消息作为触发问题，
 * 对话截到该 user 消息之前。返回 { messages, question }；前面没有 user 消息返回 null。
 */
export function retryFrom(messages, aiIdx) {
  for (let j = aiIdx - 1; j >= 0; j--) {
    if (messages[j].role === 'user') {
      return { messages: messages.slice(0, j), question: messages[j].text }
    }
  }
  return null
}

/** 文件短名（去路径去 .md），与 AiChat.vue shortFile 同规则 */
function shortFile(f) {
  return (f.split('/').pop() || f).replace(/\.md$/i, '')
}

function pad(n) { return String(n).padStart(2, '0') }

/** 导出整段对话为 markdown 文本（now 可注入便于测试） */
export function toMarkdown(messages, now = new Date()) {
  const stamp = `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())} ${pad(now.getHours())}:${pad(now.getMinutes())}`
  const parts = [`# 文档问答对话\n\n> 导出时间：${stamp}\n`]
  for (const m of messages) {
    if (m.role === 'user') {
      parts.push(`## 用户\n\n${m.text}\n`)
    } else if (m.role === 'assistant' && m.text && m.text.trim()) {
      let s = `## 助手\n\n${m.text}\n`
      if (m.sources && m.sources.length) {
        const refs = m.sources.map(x => shortFile(x.file) + (x.heading ? ` › ${x.heading.split(' > ').pop()}` : ''))
        s += `\n参考：${refs.join('；')}\n`
      }
      parts.push(s)
    }
  }
  return parts.join('\n')
}
