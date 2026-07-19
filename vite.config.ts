import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import { resolve } from 'path'

const amllCoreSrc = resolve(__dirname, 'vendor/applemusic-like-lyrics/packages/core/src')

export default defineConfig({
  plugins: [vue()],
  resolve: {
    alias: {
      '@': resolve(__dirname, 'src'),
      '@amll-core': amllCoreSrc,
      '#interfaces': resolve(amllCoreSrc, 'interfaces.ts'),
      '#utils': resolve(amllCoreSrc, 'utils'),
      '#styles': resolve(amllCoreSrc, 'styles'),
      '#lyric': resolve(amllCoreSrc, 'lyric-player'),
      '#bg': resolve(amllCoreSrc, 'bg-player'),
    },
  },
  clearScreen: false,
  // 仅以项目自身入口做依赖扫描，避免爬到 vendor 下 AMLL playground 的
  // 多个 html 入口（它们会引用 react/jotai 等未安装的包，触发无关报错）
  optimizeDeps: {
    entries: ['index.html'],
  },
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ['**/src-tauri/**'] },
  },
})
