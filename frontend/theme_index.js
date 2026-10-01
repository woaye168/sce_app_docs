// VitePress 主题入口：默认主题 + AiChat 浮动问答面板（layout-bottom 槽位）+ mermaid viewer 注册
// enhanceMermaid 注册 MermaidViewer 组件（viewer 插件官方注册方式，无版本敏感 hack）；
// 组件内部动态 import mermaid 库，不拖累首开
import DefaultTheme from 'vitepress/theme'
import { h } from 'vue'
import AiChat from './AiChat.vue'
import { enhanceMermaid } from 'vitepress-plugin-mermaid-viewer/client'
import 'vitepress-plugin-mermaid-viewer/client.css'
import './custom.css'

export default {
  extends: DefaultTheme,
  Layout: () => h(DefaultTheme.Layout, null, { 'layout-bottom': () => h(AiChat) }),
  enhanceApp({ app }) {
    enhanceMermaid(app)
    // 滚动时给 html 打 .scrolling 类（滚动条只在滚动中/悬停时浮现；scroll 不冒泡必须 capture）
    // SSR 守卫：构建期无 window
    if (typeof window !== 'undefined') {
      let timer = 0
      window.addEventListener('scroll', () => {
        document.documentElement.classList.add('scrolling')
        clearTimeout(timer)
        timer = setTimeout(() => document.documentElement.classList.remove('scrolling'), 800)
      }, { capture: true, passive: true })
    }
  }
}
