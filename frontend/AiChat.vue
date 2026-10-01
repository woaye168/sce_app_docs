<script setup>
import { ref, reactive, nextTick, computed, watch, h } from 'vue'
import { useRouter, useData } from 'vitepress'
import { MermaidViewer } from 'vitepress-plugin-mermaid-viewer/client'
import MarkdownIt from 'markdown-it'
import { scanStream, vpFence } from './stream_md.mjs'

const router = useRouter()
const { page: vpPage } = useData() // 当前页 relativePath（与 kb 文件路径一致，「解释当前文档」上下文）
// 出处显示加工：文件取短名（去路径去 .md）+ 标题链只留最后一段；完整路径悬停可见
function shortFile(f) { return (f.split('/').pop() || f).replace(/\.md$/i, '') }
function shortHeading(h) { const p = h.split(' > '); return p[p.length - 1] || h }
// 出处链接点击：VP 对 CJK hash 的锚点滚动偶发失灵（实测 hash 不 decode → getElementById 落空），
// 这里自己接管：SPA 导航 + 轮询等标题出现手动滚
async function goSource(e) {
  const a = e.target.closest('a'); if (!a) return
  const u = new URL(a.href, location.origin)
  if (u.origin !== location.origin) return // 外链不拦
  e.preventDefault()
  // 点参考跳转后收起大窗：全屏退回浮窗；手机竖屏（面板即全屏）直接关面板，让文档露出来
  if (size.value === 'full') size.value = ''
  if (window.innerWidth <= 640) open.value = false
  const target = decodeURIComponent(u.hash.slice(1))
  // 站点 cleanUrls=false，VP 路由是 .html 风格；出处 url 是 clean 风格，导航前补扩展名
  //（clean 路径不匹配路由会导致整页刷新，SPA 轮询上下文丢失 → 锚点滚动失败）
  let p = u.pathname
  if (!p.endsWith('.html') && !p.endsWith('/')) p += '.html'
  if (p !== location.pathname) router.go(p + u.hash)
  for (let i = 0; i < 30; i++) {
    await new Promise(r => setTimeout(r, 150))
    const el = target && document.getElementById(target)
    if (el) { el.scrollIntoView({ behavior: 'smooth' }); return }
  }
}

const md = new MarkdownIt({ linkify: true, breaks: true })
// 代码块产出 VP 包裹结构（div.language-xxx + lang 角标）→ 气泡挂 vp-doc 即可全套复用
// VitePress 内容样式（代码块/表格/引用/列表/行内 code），不再自绘卡片
md.renderer.rules.fence = vpFence
const open = ref(false)
const configured = ref(true)
const modelName = ref('')
// 模型下拉（自制液态玻璃：原生 select 弹层是 OS 控件，CSS 管不着、暗色下极丑）
const mselOpen = ref(false)
function pickModel(m) { chatModel.value = m; mselOpen.value = false }
watch(mselOpen, v => {
  if (!v) return
  const close = () => { mselOpen.value = false; document.removeEventListener('click', close) }
  setTimeout(() => document.addEventListener('click', close), 0) // 避开本次点击
})
// 可用模型列表 + 本次问答模型（localStorage 记住，不写全局配置——设置页管默认，这里管这次）
const models = ref([])
// localStorage 要 SSR 守卫（构建期无浏览器环境，否则 SSG 渲染报错）
const chatModel = ref(typeof localStorage !== 'undefined' ? (localStorage.getItem('ai_chat_model') || '') : '')
watch(chatModel, v => { try { localStorage.setItem('ai_chat_model', v) } catch {} })
const messages = ref([])
const input = ref('')
const sending = ref(false)
const listEl = ref(null)
// 窗口尺寸三档：'' 浮窗 | 'tall' 放大（高度拉满）| 'full' 全屏（宽高拉满）——解决「高度太短一直拖」
const size = ref('')
// 侧边对话锚点导航（Trae 式：左缘竖点）。
// 交互双模：PC hover 展开（弹层贴 rail 无间隙，鼠标可移入选择）；移动端点击 rail 切换，点外部/点锚点关闭
const dockOpen = ref(false)
function toggleDock(e) { e.stopPropagation(); dockOpen.value = !dockOpen.value }
watch(dockOpen, v => {
  if (!v) return
  const close = (ev) => { if (!ev.target.closest('.ai-dock')) { dockOpen.value = false; document.removeEventListener('click', close) } }
  setTimeout(() => document.addEventListener('click', close), 0)
})
const userMsgs = computed(() => messages.value.map((m, i) => ({ text: m.text, idx: i, role: m.role })).filter(m => m.role === 'user'))
async function jumpTo(idx) {
  dockOpen.value = false
  await nextTick()
  listEl.value?.querySelector(`[data-mi="${idx}"]`)?.scrollIntoView({ behavior: 'smooth', block: 'start' })
}
const lastMsg = computed(() => messages.value[messages.value.length - 1])
const lastStage = computed(() => (lastMsg.value && lastMsg.value.stage) || '思考中…')
const lastElapsed = computed(() => (lastMsg.value && lastMsg.value.elapsed) || 0)

async function toggle() {
  open.value = !open.value
  if (open.value) {
    const r = await fetch('/_api/llm_status').then(r => r.json()).catch(() => null)
    configured.value = !!(r && r.configured)
    modelName.value = (r && r.model) || ''
    if (!chatModel.value) chatModel.value = modelName.value
    // 拉可用模型填下拉（失败静默，保留下拉隐藏只显示当前模型名）
    const mr = await fetch('/_api/models').then(r => r.ok ? r.json() : null).catch(() => null)
    if (mr && Array.isArray(mr.models) && mr.models.length) models.value = mr.models
  }
}
async function scrollDown() { await nextTick(); listEl.value && listEl.value.scrollTo({ top: 99999999 }) }
function esc(s) { return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;') }
// 代码块复制按钮（VP 结构 + VP 样式，行为自己实现：事件委托，copied 态 2s 自动复原）
function onBodyClick(e) {
  const btn = e.target.closest('button.copy')
  if (!btn) return
  const code = btn.parentElement?.querySelector('pre code')
  if (!code) return
  try { navigator.clipboard.writeText(code.textContent) } catch {}
  btn.classList.add('copied')
  setTimeout(() => btn.classList.remove('copied'), 2000)
}
// 流式渲染：scanStream（frontend/stream_md.mjs，纯函数有 node 测试）把正文按「已闭合 fence」切块——
// md 块无状态随流重渲；code/mermaid 块带稳定 key（cc0/mm1…）保组件身份，fence 一闭合即
// 高亮/渲染，之后不再被后续 delta 冲掉。所有块渲染进同一个气泡（视觉上是完整一条消息）。
function renderSegs(text) {
  return scanStream(text).map(s =>
    s.type === 'mermaid' ? { ...s, graph: encodeURIComponent(s.graph) } : { ...s, html: md.render(s.text) }
  )
}
// 代码块组件：静态 html（不重渲），挂载后高亮一次。
// 必须用 render 函数而非 template 字符串——vue 若解析为 runtime-only 构建，运行时 template 编译不了会静默渲染为空（踩过）。
const CodeSeg = {
  props: { html: String },
  render() { return h('div', { class: 'ai-code-seg', innerHTML: this.html }) },
  mounted() { highlightCodeIn(this.$el) }
}
// 代码块语法高亮（highlight.js common 集懒加载，只在有代码块时下发；mermaid 段不走这）
async function highlightCodeIn(el) {
  const blocks = el?.querySelectorAll('pre code:not(.language-mermaid):not(.hljs)') || []
  if (!blocks.length) return
  const hljs = (await import('highlight.js/lib/common')).default
  blocks.forEach(b => { try { hljs.highlightElement(b) } catch { /* 未知语言保持原样 */ } })
}
async function send() {
  const q = input.value.trim()
  if (!q || sending.value) return
  input.value = ''
  // history 过滤空 assistant 消息：失败/空回答进了历史，下一轮 OpenAI 直接 400
  //（"assistant must not be empty"——对话 2-3 次后全挂的元凶之一）
  const history = messages.value
    .filter(m => (m.role === 'user' || m.role === 'assistant') && m.text && m.text.trim())
    .map(m => ({ role: m.role, content: m.text }))
  messages.value.push({ role: 'user', text: q, html: md.render(q) })
  // 必须 reactive：裸对象 push 进 ref 数组后，经原引用改属性不触发更新（曾致「卡很久一次性出」）
  const ai = reactive({ role: 'assistant', text: '', errHtml: '', think: '', thinkHtml: '', thinkOpen: true, tools: [], sources: [], stage: '发送中…', elapsed: 0 })
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
      body: JSON.stringify({ question: q, history, page: vpPage.value?.relativePath || '', model: chatModel.value || '' })
    })
    if (!resp.ok) {
      const t = await resp.json().catch(() => ({}))
      ai.errHtml = '<p class="err">' + esc(t.error || ('HTTP ' + resp.status)) + '</p>'
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
        else if (data.type === 'delta') { ai.stage = ''; if (ai.think) ai.thinkOpen = false; ai.text += data.text; scrollDown() }
        else if (data.type === 'tool_start') { ai.stage = '正在调用 ' + data.name + '…'; ai.tools.push({ name: data.name, args: data.args, summary: '', running: true }); scrollDown() }
        else if (data.type === 'tool_call') { ai.stage = '整理中…'; const c = ai.tools.find(x => x.name === data.name && x.running); if (c) { c.summary = data.summary; c.running = false } else { ai.tools.push({ name: data.name, summary: data.summary, running: false }) } }
        else if (data.type === 'sources') { ai.sources = data.hits }
        else if (data.type === 'error') { ai.errHtml += '<p class="err">' + esc(data.text) + '</p>' }
        if (data.type === 'done' || data.type === 'error') { finished = true; try { reader.cancel() } catch {}; break } // 服务端 Connection: close 未必真关，收到终止帧主动结束
      }
    }
  } catch (e) {
    ai.errHtml += '<p class="err">' + esc(String(e)) + '</p>'
  }
  clearInterval(timer)
  ai.stage = ''
  sending.value = false
  await scrollDown()
  highlightCodeIn(listEl.value) // done 后代码块上语法高亮（mermaid 已由分段渲染接管）
}
</script>

<template>
  <button class="ai-fab" :class="{ active: open }" @click="toggle" :title="'文档问答' + (modelName ? '（' + modelName + '）' : '')">
    <svg v-if="!open" viewBox="0 0 24 24" width="22" height="22" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/></svg>
    <svg v-else viewBox="0 0 24 24" width="22" height="22" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M18 6 6 18M6 6l12 12"/></svg>
  </button>
  <Transition name="pop">
    <div v-if="open" class="ai-panel" :class="size">
      <div class="ai-header">
        <div v-if="models.length" class="ai-msel" title="本次问答使用的模型（默认值在 AI 设置页）">
          <button class="ai-msel-btn" @click.stop="mselOpen = !mselOpen">
            <span>{{ chatModel || modelName }}</span>
            <svg :class="{ open: mselOpen }" viewBox="0 0 24 24" width="12" height="12" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><path d="m6 9 6 6 6-6"/></svg>
          </button>
          <Transition name="pop">
            <div v-if="mselOpen" class="ai-msel-pop">
              <div v-for="m in models" :key="m" class="ai-msel-item" :class="{ active: m === (chatModel || modelName) }" @click.stop="pickModel(m)">
                <span>{{ m }}</span><span v-if="m === (chatModel || modelName)" class="ai-msel-check">✓</span>
              </div>
            </div>
          </Transition>
        </div>
        <span v-else-if="modelName" class="ai-model">{{ modelName }}</span>
        <span class="ai-winctl">
          <button class="ai-wbtn" :class="{ on: size === 'tall' }" :title="size === 'tall' ? '还原浮窗' : '放大（高度拉满）'" @click="size = size === 'tall' ? '' : 'tall'">
            <svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3v18M7 7l5-4 5 4M7 17l5 4 5-4"/></svg>
          </button>
          <button class="ai-wbtn" :class="{ on: size === 'full' }" :title="size === 'full' ? '还原浮窗' : '全屏'" @click="size = size === 'full' ? '' : 'full'">
            <svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M8 3H5a2 2 0 0 0-2 2v3m18 0V5a2 2 0 0 0-2-2h-3m0 18h3a2 2 0 0 0 2-2v-3M3 16v3a2 2 0 0 0 2 2h3"/></svg>
          </button>
          <button class="ai-wbtn" title="关闭" @click="open = false">
            <svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M18 6 6 18M6 6l12 12"/></svg>
          </button>
        </span>
      </div>
      <div v-if="!configured" class="ai-warn">LLM 未配置：请到应用「AI」设置页填 base_url / api_key / model</div>
      <div v-if="userMsgs.length > 1" class="ai-dock" @mouseenter="dockOpen = true" @mouseleave="dockOpen = false">
        <div class="ai-dock-rail" @click="toggleDock"><i v-for="m in userMsgs" :key="m.idx"></i></div>
        <Transition name="pop">
          <div v-if="dockOpen" class="ai-dock-pop">
            <div v-for="m in userMsgs" :key="m.idx" class="ai-dock-item" @click="jumpTo(m.idx)">
              <span class="ai-dock-n">{{ userMsgs.indexOf(m) + 1 }}</span>{{ m.text.slice(0, 26) }}
            </div>
          </div>
        </Transition>
      </div>
      <div ref="listEl" class="ai-list">
        <div v-if="messages.length === 0" class="ai-empty">问我任何关于本项目文档的问题</div>
        <div v-for="(m, i) in messages" :key="i" class="ai-msg" :class="m.role" :data-mi="i">
          <details v-if="m.think" :open="m.thinkOpen" class="ai-think">
            <summary>思考过程</summary>
            <div v-html="m.thinkHtml"></div>
          </details>
          <div v-for="(t, j) in (m.tools || [])" :key="j" class="ai-tool" :class="{ running: t.running }">
            <span class="ai-tool-name">{{ t.running ? '⏳' : '✓' }} {{ t.name }}</span>
            <span class="ai-tool-sum">{{ t.running ? '执行中…' : t.summary }}</span>
          </div>
          <div v-if="m.text" class="ai-bubble ai-body vp-doc" @click="onBodyClick">
            <template v-for="seg in renderSegs(m.text)" :key="seg.key">
              <MermaidViewer v-if="seg.type === 'mermaid'" class="ai-mmd" :graph="seg.graph" :id="seg.key + '-' + i" />
              <CodeSeg v-else-if="seg.type === 'code'" :html="seg.html" />
              <div v-else class="ai-md" v-html="seg.html"></div>
            </template>
          </div>
          <div v-if="m.errHtml" class="ai-bubble" v-html="m.errHtml"></div>
          <div v-if="m.sources && m.sources.length" class="ai-sources" @click="goSource">
            <div class="ai-sources-title">参考：</div>
            <a v-for="(s, k) in m.sources" :key="k" :href="s.url" :title="s.file + (s.heading ? ' › ' + s.heading : '')">{{ shortFile(s.file) }}<template v-if="s.heading"> › {{ shortHeading(s.heading) }}</template></a>
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
  /* 液态玻璃：blur 已真实生效（此前被 -webkit 前缀坑吞掉），透明度可以降下来；
     inset 顶边高光 + 外阴影 = 苹果的镜面高光边手感 */
  background: color-mix(in srgb, var(--vp-c-bg) 62%, transparent);
  backdrop-filter: blur(28px) saturate(180%);
  border: 1px solid color-mix(in srgb, var(--vp-c-divider) 60%, transparent);
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, .35), 0 24px 64px rgba(0, 0, 0, .18);
}
/* 暗色下 .18 的黑影比背景还亮会变「灰晕」——暗色阴影要更深更不透明，高光也压暗 */
html.dark .ai-panel { box-shadow: inset 0 1px 0 rgba(255, 255, 255, .08), 0 24px 64px rgba(0, 0, 0, .55); }
/* 尺寸档：tall 放大（高度拉满，宽度不变）；full 全屏（宽高拉满，留 16px 呼吸边） */
.ai-panel.tall { top: 16px; bottom: 84px; height: auto; max-height: none; }
.ai-panel.full { inset: 16px; width: auto; height: auto; max-width: none; max-height: none; }
/* 手机竖屏：面板即全屏（媒体查询压住所有尺寸档） */
@media (max-width: 640px) {
  .ai-panel, .ai-panel.tall, .ai-panel.full {
    inset: 0; width: 100%; height: 100%; max-width: none; max-height: none; border-radius: 0;
  }
}
/* 窗口控制按钮（Trae 式描线小图标，hover 亮） */
.ai-winctl { display: flex; gap: 2px; margin-left: 6px; }
.ai-wbtn {
  display: flex; align-items: center; justify-content: center; width: 24px; height: 24px;
  border: none; border-radius: 7px; cursor: pointer; color: var(--vp-c-text-3); background: transparent;
  transition: color .15s, background .15s;
}
.ai-wbtn:hover { color: var(--vp-c-text-1); background: color-mix(in srgb, var(--vp-c-brand) 14%, transparent); }
.ai-wbtn.on { color: var(--vp-c-brand); }
/* 侧边锚点导航：左缘竖点轨道（常态极简），hover 展开液态玻璃列表 */
.ai-dock { position: absolute; left: 0; top: 60px; bottom: 70px; z-index: 20; display: flex; align-items: center; }
.ai-dock-rail {
  display: flex; flex-direction: column; gap: 6px; padding: 8px 5px 8px 6px; cursor: pointer;
  background: color-mix(in srgb, var(--vp-c-bg) 40%, transparent);
  border-radius: 0 10px 10px 0;
  backdrop-filter: blur(10px);
  transition: background .15s;
}
.ai-dock-rail i {
  width: 4px; height: 14px; border-radius: 2px;
  background: color-mix(in srgb, var(--vp-c-text-3) 55%, transparent);
  transition: background .15s;
}
/* hover 是「整块玻璃微亮」而不是亮蓝跳色（苹果式克制） */
.ai-dock:hover .ai-dock-rail { background: color-mix(in srgb, var(--vp-c-bg) 70%, transparent); }
.ai-dock:hover .ai-dock-rail i { background: color-mix(in srgb, var(--vp-c-text-2) 85%, transparent); }
.ai-dock-pop {
  min-width: 200px; max-width: 280px; max-height: 320px; overflow-y: auto; padding: 5px; margin-left: 2px;
  border-radius: 14px;
  background: color-mix(in srgb, var(--vp-c-bg) 68%, transparent);
  backdrop-filter: blur(24px) saturate(180%);
  border: 1px solid color-mix(in srgb, var(--vp-c-divider) 55%, transparent);
  box-shadow: 0 12px 40px rgba(0, 0, 0, .18);
}
html.dark .ai-dock-pop { box-shadow: 0 12px 40px rgba(0, 0, 0, .55); }
.ai-dock-item {
  display: flex; align-items: center; gap: 8px; padding: 6px 10px; border-radius: 9px;
  font-size: 12.5px; color: var(--vp-c-text-2); cursor: pointer;
  overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
}
.ai-dock-item:hover { background: color-mix(in srgb, var(--vp-c-brand) 14%, transparent); color: var(--vp-c-text-1); }
/* 序号徽章：液态玻璃小圆片（半透明底+细边+模糊），不要实心渐变球 */
.ai-dock-n {
  flex: none; min-width: 18px; height: 18px; padding: 0 4px; border-radius: 999px; font-size: 10px;
  display: flex; align-items: center; justify-content: center;
  color: var(--vp-c-text-2);
  background: color-mix(in srgb, var(--vp-c-bg-soft) 55%, transparent);
  border: 1px solid color-mix(in srgb, var(--vp-c-divider) 60%, transparent);
  backdrop-filter: blur(8px);
}
.pop-enter-active, .pop-leave-active { transition: opacity .18s, transform .18s; }
.pop-enter-from, .pop-leave-to { opacity: 0; transform: translateY(12px) scale(.98); }
.ai-header {
  display: flex; align-items: center; gap: 8px; padding: 14px 18px;
  font-weight: 600; color: var(--vp-c-text-1);
  border-bottom: 1px solid color-mix(in srgb, var(--vp-c-divider) 50%, transparent);
}
.ai-model { margin-left: auto; font-size: 12px; font-weight: 400; color: var(--vp-c-text-3); }
/* 自制模型下拉（液态玻璃：胶囊触发器 + 毛玻璃弹层，全主题变量明暗自适应） */
.ai-msel { position: relative; margin-left: auto; }
.ai-msel-btn {
  display: flex; align-items: center; gap: 5px; cursor: pointer; max-width: 200px;
  font-size: 12px; color: var(--vp-c-text-2);
  background: color-mix(in srgb, var(--vp-c-bg-soft) 55%, transparent);
  backdrop-filter: blur(12px) saturate(160%);
  border: 1px solid color-mix(in srgb, var(--vp-c-divider) 55%, transparent);
  border-radius: 999px; padding: 3px 10px;
  transition: border-color .15s, color .15s;
}
.ai-msel-btn:hover { border-color: var(--vp-c-brand); color: var(--vp-c-text-1); }
.ai-msel-btn > span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.ai-msel-btn svg { flex: none; transition: transform .18s; }
.ai-msel-btn svg.open { transform: rotate(180deg); }
.ai-msel-pop {
  position: absolute; right: 0; top: calc(100% + 6px); z-index: 30;
  min-width: 180px; max-width: 260px; max-height: 280px; overflow-y: auto; padding: 5px;
  border-radius: 14px;
  background: color-mix(in srgb, var(--vp-c-bg) 68%, transparent);
  backdrop-filter: blur(24px) saturate(180%);
  border: 1px solid color-mix(in srgb, var(--vp-c-divider) 55%, transparent);
  box-shadow: 0 12px 40px rgba(0, 0, 0, .18);
}
html.dark .ai-msel-pop { box-shadow: 0 12px 40px rgba(0, 0, 0, .55); }
.ai-msel-item {
  display: flex; align-items: center; justify-content: space-between; gap: 8px;
  padding: 6px 10px; border-radius: 9px; font-size: 12.5px; color: var(--vp-c-text-2); cursor: pointer;
}
.ai-msel-item:hover { background: color-mix(in srgb, var(--vp-c-brand) 14%, transparent); color: var(--vp-c-text-1); }
.ai-msel-item.active { color: var(--vp-c-brand); font-weight: 600; }
.ai-msel-check { flex: none; }
.ai-warn { padding: 10px 18px; font-size: 13px; color: var(--vp-c-warning-text, #b88230); background: color-mix(in srgb, #b88230 12%, transparent); }
.ai-list { flex: 1; overflow-y: auto; padding: 14px; display: flex; flex-direction: column; gap: 10px; }
/* 分段渲染后一条消息多个气泡/图/代码段，段间留呼吸间隙（原先单气泡没这问题） */
.ai-msg > * + * { margin-top: 6px; }
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
/* 内容样式（代码块/表格/引用/列表/行内 code）全套走 vp-doc 主题变量，明暗自适应；
   下面只留聊天气泡语境的间距微调（vp-doc 默认是给正文文章的，间距偏大） */
.ai-body :deep(p) { margin: 6px 0; }
/* 聊天语境标题统一字号：聊天框里大标题很突兀，一律同正文字号、仅靠加粗区分层级 */
.ai-body :deep(h1), .ai-body :deep(h2), .ai-body :deep(h3), .ai-body :deep(h4) { margin: 10px 0 4px; line-height: 1.4; border: none; padding: 0; font-size: inherit; font-weight: 600; }
.ai-body :deep([class*="language-"]) { margin: 8px 0; }
/* vp-doc 的 pre 只有垂直 padding（横向靠 shiki 的 .line span 撑，我们没有）→ 补横向 padding */
.ai-body :deep([class*="language-"] pre) { padding: 10px 14px; }
.ai-bubble :deep(.err) { color: #e05555; }
.ai-mmd { margin: 6px 0; }
/* 代码高亮 token 色（自绘明暗双色，VSCode 风格近似；不引 hljs 主题 css 避免明暗切换问题） */
.ai-bubble :deep(.hljs-keyword), .ai-bubble :deep(.hljs-literal), .ai-bubble :deep(.hljs-selector-tag) { color: #af4bcf; }
.ai-bubble :deep(.hljs-string), .ai-bubble :deep(.hljs-regexp) { color: #2e7d32; }
.ai-bubble :deep(.hljs-comment), .ai-bubble :deep(.hljs-quote) { color: #8a919e; font-style: italic; }
.ai-bubble :deep(.hljs-number), .ai-bubble :deep(.hljs-symbol) { color: #b86e28; }
.ai-bubble :deep(.hljs-title), .ai-bubble :deep(.hljs-name), .ai-bubble :deep(.hljs-title.function_) { color: #1f6fd6; }
.ai-bubble :deep(.hljs-attr), .ai-bubble :deep(.hljs-attribute), .ai-bubble :deep(.hljs-variable), .ai-bubble :deep(.hljs-template-variable) { color: #b25086; }
.ai-bubble :deep(.hljs-type), .ai-bubble :deep(.hljs-built_in), .ai-bubble :deep(.hljs-title.class_) { color: #0f7b6c; }
html.dark .ai-bubble :deep(.hljs-keyword), html.dark .ai-bubble :deep(.hljs-literal), html.dark .ai-bubble :deep(.hljs-selector-tag) { color: #c586c0; }
html.dark .ai-bubble :deep(.hljs-string), html.dark .ai-bubble :deep(.hljs-regexp) { color: #7ec98a; }
html.dark .ai-bubble :deep(.hljs-comment), html.dark .ai-bubble :deep(.hljs-quote) { color: #7f8b98; }
html.dark .ai-bubble :deep(.hljs-number), html.dark .ai-bubble :deep(.hljs-symbol) { color: #d9a05b; }
html.dark .ai-bubble :deep(.hljs-title), html.dark .ai-bubble :deep(.hljs-name), html.dark .ai-bubble :deep(.hljs-title.function_) { color: #6cb3ff; }
html.dark .ai-bubble :deep(.hljs-attr), html.dark .ai-bubble :deep(.hljs-attribute), html.dark .ai-bubble :deep(.hljs-variable), html.dark .ai-bubble :deep(.hljs-template-variable) { color: #d18bb2; }
html.dark .ai-bubble :deep(.hljs-type), html.dark .ai-bubble :deep(.hljs-built_in), html.dark .ai-bubble :deep(.hljs-title.class_) { color: #4ec9b0; }
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
/* 出处一行一个（原先 flex-wrap 横排，多条时挤成一团读不清） */
.ai-sources { margin-top: 6px; font-size: 12px; display: flex; flex-direction: column; gap: 3px; }
.ai-sources-title { color: var(--vp-c-text-3); }
.ai-sources a { color: var(--vp-c-brand); text-decoration: none; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
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
