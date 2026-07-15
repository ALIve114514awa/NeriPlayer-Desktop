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
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ['**/src-tauri/**'] },
  },
})
