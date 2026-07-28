(function () {
  var mode = localStorage.getItem('theme-mode') || 'system'
  var isDark = mode === 'dark' || (
    mode === 'system' && window.matchMedia('(prefers-color-scheme: dark)').matches
  )
  var background = isDark ? 'rgb(20,18,24)' : 'rgb(252,248,253)'
  var foreground = isDark ? 'rgb(230,225,229)' : 'rgb(28,27,31)'
  document.documentElement.style.background = background
  document.documentElement.style.color = foreground
  document.documentElement.className = isDark ? 'dark-theme' : 'light-theme'
}())
