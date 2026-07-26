import { createApp } from 'vue'
import { createPinia } from 'pinia'
import { createRouter, createWebHistory } from 'vue-router'
import { getCurrentWindow } from '@tauri-apps/api/window'
import App from './App.vue'
import i18n from './i18n'
import { initTheme } from './utils/theme'
import './styles/global.scss'

// 在 DOM 挂载前应用主题（class 已在 index.html 内联脚本中预设）
initTheme()

const router = createRouter({
  history: createWebHistory(),
  routes: [
    { path: '/', name: 'home', meta: { section: 'home' }, component: () => import('./views/HomeView.vue') },
    { path: '/explore', name: 'explore', meta: { section: 'explore' }, component: () => import('./views/ExploreView.vue') },
    { path: '/library', name: 'library', meta: { section: 'library' }, component: () => import('./views/LibraryView.vue') },
    { path: '/settings', name: 'settings', meta: { section: 'settings' }, component: () => import('./views/SettingsView.vue') },
    { path: '/downloads', name: 'downloads', meta: { section: 'library' }, component: () => import('./views/DownloadsView.vue') },
    { path: '/recent', name: 'recent', meta: { section: 'library' }, component: () => import('./views/RecentView.vue') },
    { path: '/stats', name: 'playback-stats', meta: { section: 'library' }, component: () => import('./views/PlaybackStatsView.vue') },
    { path: '/playlist/netease/:id', name: 'netease-playlist', meta: { section: 'library' }, component: () => import('./views/NeteasePlaylistView.vue') },
    { path: '/album/netease/:id', name: 'netease-album', meta: { section: 'library' }, component: () => import('./views/NeteasePlaylistView.vue'), props: { isAlbum: true } },
    { path: '/playlist/bilibili/:mediaId', name: 'bili-playlist', meta: { section: 'library' }, component: () => import('./views/BiliPlaylistView.vue') },
    { path: '/playlist/youtube/:browseId', name: 'youtube-playlist', meta: { section: 'library' }, component: () => import('./views/YouTubePlaylistView.vue') },
    { path: '/playlist/local/:id', name: 'local-playlist', meta: { section: 'library' }, component: () => import('./views/LocalPlaylistView.vue') },
    { path: '/library/local-artist/:name', name: 'local-artist', meta: { section: 'library' }, component: () => import('./views/LocalArtistView.vue') },
    { path: '/library/favorite/:id', name: 'favorite-playlist', meta: { section: 'library' }, component: () => import('./views/FavoritePlaylistView.vue') },
    { path: '/debug', name: 'debug', meta: { section: 'debug' }, component: () => import('./views/DebugView.vue') },
  ],
})

const app = createApp(App)
app.use(createPinia())
app.use(router)
app.use(i18n)
app.mount('#app')

// Vue 挂载完成后显示窗口，避免闪烁
getCurrentWindow().show().catch(() => {
  // 非 Tauri 环境忽略
})
