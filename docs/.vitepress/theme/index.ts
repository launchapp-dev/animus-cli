import type { Theme } from 'vitepress'
import DefaultTheme from 'vitepress/theme'
import mermaid from 'mermaid'
import elkLayouts from '@mermaid-js/layout-elk'
import Landing from './Landing.vue'
import './custom.css'

export default {
  extends: DefaultTheme,
  enhanceApp({ app }) {
    app.component('Landing', Landing)
    if (!import.meta.env.SSR) {
      mermaid.registerLayoutLoaders(elkLayouts)
    }
  },
} satisfies Theme
