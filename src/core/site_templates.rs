//! 站点模板常量（从 site.rs 拆出，单文件 500 行守则）：前端组件等大块静态文本

/// AI 问答组件（毛玻璃风格；明暗走 VitePress CSS 变量；SSE 流式 + 思考链/工具调用折叠 + 出处链接）
pub(crate) const AI_CHAT_VUE: &str = r##"<script setup>
import { ref, reactive, nextTick, computed } from 'vue'
import MarkdownIt from 'markdown-it'

const md = new MarkdownIt({ linkify: true, breaks: true })
const open = ref(false)
const configured = ref(true)
const modelName = ref('')
const messages = ref([])
const input = ref('')
const sending = ref(false)
const listEl = ref(null)
const lastMsg = computed(() => messages.value[messages.value.length - 1])
const lastStage = computed(() => (lastMsg.value && lastMsg.value.stage) || '思考中…')
const lastElapsed = computed(() => (lastMsg.value && lastMsg.value.elapsed) || 0)

async function toggle() {
  open.value = !open.value
  if (open.value) {
    const r = await fetch('/_api/llm_status').then(r => r.json()).catch(() => null)
    configured.value = !!(r && r.configured)
    modelName.value = (r && r.model) || ''
  }
}
async function scrollDown() { await nextTick(); listEl.value && listEl.value.scrollTo({ top: 99999999 }) }
function esc(s) { return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;') }
async function send() {
  const q = input.value.trim()
  if (!q || sending.value) return
  input.value = ''
  const history = messages.value.filter(m => m.role === 'user' || m.role === 'assistant').map(m => ({ role: m.role, content: m.text }))
  messages.value.push({ role: 'user', text: q, html: md.render(q) })
  // 必须 reactive：裸对象 push 进 ref 数组后，经原引用改属性不触发更新（曾致「卡很久一次性出」）
  const ai = reactive({ role: 'assistant', text: '', html: '', think: '', thinkHtml: '', thinkOpen: true, tools: [], sources: [], stage: '发送中…', elapsed: 0 })
  messages.value.push(ai)
  sending.value = true
  scrollDown()
  // 秒表：回答期间每 0.5s 刷新，首 token 再慢也有活感
  const t0 = Date.now()
  const timer = setInterval(() => { ai.elapsed = ((Date.now() - t0) / 1000).toFixed(0) }, 500)
  try {
    const resp = await fetch('/_api/ask', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ question: q, history })
    })
    if (!resp.ok) {
      const t = await resp.json().catch(() => ({}))
      ai.html = '<p class="err">' + esc(t.error || ('HTTP ' + resp.status)) + '</p>'
      clearInterval(timer); sending.value = false
      return
    }
    const reader = resp.body.getReader()
    const dec = new TextDecoder()
    let buf = ''
    let finished = false
    for (;;) {
      const { done, value } = await reader.read()
      if (done || finished) break
      buf += dec.decode(value, { stream: true })
      const lines = buf.split('\n')
      buf = lines.pop() || ''
      for (const line of lines) {
        const t = line.trim()
        if (!t.startsWith('data:')) continue
        let data
        try { data = JSON.parse(t.slice(5)) } catch { continue }
        if (data.type === 'start') { ai.stage = '已连接，等待模型响应…' }
        else if (data.type === 'think') { ai.stage = '思考中…'; ai.think += data.text; ai.thinkHtml = md.render(ai.think) }
        else if (data.type === 'delta') { ai.stage = ''; if (ai.think) ai.thinkOpen = false; ai.text += data.text; ai.html = md.render(ai.text); scrollDown() }
        else if (data.type === 'tool_start') { ai.stage = '正在调用 ' + data.name + '…'; ai.tools.push({ name: data.name, args: data.args, summary: '', running: true }); scrollDown() }
        else if (data.type === 'tool_call') { ai.stage = '整理中…'; const c = ai.tools.find(x => x.name === data.name && x.running); if (c) { c.summary = data.summary; c.running = false } else { ai.tools.push({ name: data.name, summary: data.summary, running: false }) } }
        else if (data.type === 'sources') { ai.sources = data.hits }
        else if (data.type === 'error') { ai.html += '<p class="err">' + esc(data.text) + '</p>' }
        if (data.type === 'done' || data.type === 'error') { finished = true; try { reader.cancel() } catch {}; break } // 服务端 Connection: close 未必真关，收到终止帧主动结束
      }
    }
  } catch (e) {
    ai.html += '<p class="err">' + esc(String(e)) + '</p>'
  }
  clearInterval(timer)
  ai.stage = ''
  sending.value = false
  scrollDown()
}
</script>

<template>
  <button class="ai-fab" :class="{ active: open }" @click="toggle" :title="'文档问答' + (modelName ? '（' + modelName + '）' : '')">
    <svg v-if="!open" viewBox="0 0 24 24" width="22" height="22" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/></svg>
    <svg v-else viewBox="0 0 24 24" width="22" height="22" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M18 6 6 18M6 6l12 12"/></svg>
  </button>
  <Transition name="pop">
    <div v-if="open" class="ai-panel">
      <div class="ai-header">
        <span>文档问答</span>
        <span v-if="modelName" class="ai-model">{{ modelName }}</span>
      </div>
      <div v-if="!configured" class="ai-warn">LLM 未配置：请到应用「AI」设置页填 base_url / api_key / model</div>
      <div ref="listEl" class="ai-list">
        <div v-if="messages.length === 0" class="ai-empty">问我任何关于本项目文档的问题</div>
        <div v-for="(m, i) in messages" :key="i" class="ai-msg" :class="m.role">
          <details v-if="m.think" :open="m.thinkOpen" class="ai-think">
            <summary>思考过程</summary>
            <div v-html="m.thinkHtml"></div>
          </details>
          <div v-for="(t, j) in (m.tools || [])" :key="j" class="ai-tool" :class="{ running: t.running }">
            <span class="ai-tool-name">{{ t.running ? '⏳' : '✓' }} {{ t.name }}</span>
            <span class="ai-tool-sum">{{ t.running ? '执行中…' : t.summary }}</span>
          </div>
          <div class="ai-bubble" v-html="m.html"></div>
          <div v-if="m.sources && m.sources.length" class="ai-sources">
            <div class="ai-sources-title">参考：</div>
            <a v-for="(s, k) in m.sources" :key="k" :href="s.url">{{ s.file }}<template v-if="s.heading"> › {{ s.heading }}</template></a>
          </div>
        </div>
        <div v-if="sending" class="ai-stage"><span class="ai-spin"></span>{{ lastStage }}<span class="ai-elapsed">{{ lastElapsed }}s</span></div>
      </div>
      <div class="ai-input">
        <textarea v-model="input" rows="2" placeholder="输入问题，Enter 发送，Shift+Enter 换行" @keydown.enter.exact.prevent="send"></textarea>
        <button class="ai-send" :disabled="sending || !configured" @click="send">发送</button>
      </div>
    </div>
  </Transition>
</template>

<style scoped>
.ai-fab {
  position: fixed; right: 24px; bottom: 24px; z-index: 100;
  width: 48px; height: 48px; border-radius: 50%; border: none; cursor: pointer;
  display: flex; align-items: center; justify-content: center;
  color: #fff; background: linear-gradient(135deg, #5b8cff, #a06bff);
  box-shadow: 0 8px 24px rgba(91, 140, 255, .35);
  transition: transform .2s, box-shadow .2s;
}
.ai-fab:hover { transform: translateY(-2px); box-shadow: 0 12px 32px rgba(91, 140, 255, .45); }
.ai-panel {
  position: fixed; right: 24px; bottom: 84px; z-index: 100;
  width: 420px; max-width: calc(100vw - 48px); height: 600px; max-height: calc(100vh - 120px);
  display: flex; flex-direction: column; overflow: hidden;
  border-radius: 18px;
  background: color-mix(in srgb, var(--vp-c-bg) 72%, transparent);
  backdrop-filter: blur(20px) saturate(180%);
  -webkit-backdrop-filter: blur(20px) saturate(180%);
  border: 1px solid color-mix(in srgb, var(--vp-c-divider) 60%, transparent);
  box-shadow: 0 24px 64px rgba(0, 0, 0, .18);
}
.pop-enter-active, .pop-leave-active { transition: opacity .18s, transform .18s; }
.pop-enter-from, .pop-leave-to { opacity: 0; transform: translateY(12px) scale(.98); }
.ai-header {
  display: flex; align-items: center; gap: 8px; padding: 14px 18px;
  font-weight: 600; color: var(--vp-c-text-1);
  border-bottom: 1px solid color-mix(in srgb, var(--vp-c-divider) 50%, transparent);
}
.ai-model { margin-left: auto; font-size: 12px; font-weight: 400; color: var(--vp-c-text-3); }
.ai-warn { padding: 10px 18px; font-size: 13px; color: var(--vp-c-warning-text, #b88230); background: color-mix(in srgb, #b88230 12%, transparent); }
.ai-list { flex: 1; overflow-y: auto; padding: 14px; display: flex; flex-direction: column; gap: 10px; }
.ai-empty { margin: auto; color: var(--vp-c-text-3); font-size: 13px; }
.ai-msg.user .ai-bubble {
  background: linear-gradient(135deg, #5b8cff, #a06bff); color: #fff; margin-left: 48px;
  border-radius: 14px 14px 4px 14px;
}
.ai-msg.assistant .ai-bubble {
  background: color-mix(in srgb, var(--vp-c-bg-soft) 80%, transparent);
  color: var(--vp-c-text-1); margin-right: 48px;
  border: 1px solid color-mix(in srgb, var(--vp-c-divider) 50%, transparent);
  border-radius: 14px 14px 14px 4px;
}
.ai-bubble { padding: 10px 14px; font-size: 14px; line-height: 1.7; word-break: break-word; }
.ai-bubble :deep(p) { margin: 4px 0; }
.ai-bubble :deep(pre) { background: var(--vp-c-bg-mute, rgba(0,0,0,.06)); padding: 8px; border-radius: 8px; overflow-x: auto; font-size: 12px; }
.ai-bubble :deep(code) { font-family: var(--vp-font-family-mono); font-size: .9em; }
.ai-bubble :deep(.err) { color: #e05555; }
.ai-think { margin-bottom: 6px; font-size: 12px; color: var(--vp-c-text-3); }
.ai-think summary { cursor: pointer; user-select: none; }
.ai-think > div { padding: 6px 10px; border-left: 2px solid var(--vp-c-divider); margin-top: 4px; opacity: .85; }
.ai-tool { font-size: 12px; color: var(--vp-c-text-3); padding: 4px 10px; margin-bottom: 4px; border-radius: 8px; background: color-mix(in srgb, var(--vp-c-brand) 8%, transparent); display: flex; gap: 8px; }
.ai-tool.running { color: var(--vp-c-text-1); background: color-mix(in srgb, var(--vp-c-brand) 16%, transparent); }
.ai-tool-name { font-weight: 600; white-space: nowrap; }
.ai-tool-sum { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.ai-stage { display: flex; align-items: center; gap: 8px; font-size: 13px; color: var(--vp-c-text-2); padding: 2px 4px; }
.ai-elapsed { margin-left: auto; color: var(--vp-c-text-3); font-variant-numeric: tabular-nums; }
.ai-spin { width: 12px; height: 12px; border-radius: 50%; border: 2px solid var(--vp-c-divider); border-top-color: var(--vp-c-brand); animation: ai-spin 0.8s linear infinite; flex: none; }
@keyframes ai-spin { to { transform: rotate(360deg); } }
.ai-sources { margin-top: 6px; font-size: 12px; display: flex; flex-wrap: wrap; gap: 4px 10px; }
.ai-sources-title { color: var(--vp-c-text-3); }
.ai-sources a { color: var(--vp-c-brand); text-decoration: none; }
.ai-sources a:hover { text-decoration: underline; }
.ai-input { display: flex; gap: 8px; padding: 12px; border-top: 1px solid color-mix(in srgb, var(--vp-c-divider) 50%, transparent); }
.ai-input textarea {
  flex: 1; resize: none; border: 1px solid color-mix(in srgb, var(--vp-c-divider) 70%, transparent);
  background: color-mix(in srgb, var(--vp-c-bg) 60%, transparent); color: var(--vp-c-text-1);
  border-radius: 10px; padding: 8px 10px; font-size: 14px; font-family: inherit; outline: none;
}
.ai-input textarea:focus { border-color: var(--vp-c-brand); }
.ai-send {
  border: none; border-radius: 10px; padding: 0 16px; cursor: pointer;
  color: #fff; background: linear-gradient(135deg, #5b8cff, #a06bff); font-size: 14px;
}
.ai-send:disabled { opacity: .5; cursor: default; }
</style>
"##;
