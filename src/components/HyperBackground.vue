<script setup lang="ts">
import { ref, onMounted, onUnmounted } from 'vue'
import { hyperBackgroundVertexShader, hyperBackgroundFragmentShader } from '@/shaders/hyperBackground'

const props = withDefaults(defineProps<{
  musicLevel?: number
  beatImpulse?: number
  colors?: [number[], number[], number[], number[]]
  isDark?: boolean
  lightOffset?: number
  saturateOffset?: number
}>(), {
  musicLevel: 0,
  beatImpulse: 0,
  colors: () => [
    [0.4, 0.31, 0.64, 1],  // dominant
    [0.49, 0.36, 0.75, 1], // vibrant
    [0.56, 0.49, 0.69, 1], // muted
    [0.29, 0.24, 0.43, 1], // darkMuted
  ],
  isDark: true,
  lightOffset: 0,
  saturateOffset: 0,
})

const canvas = ref<HTMLCanvasElement>()
let gl: WebGLRenderingContext | null = null
let program: WebGLProgram | null = null
let animFrame = 0
let startTime = 0
let lastFrameMs = 0

// —— 音乐律动第二级非对称平滑（对齐 Android HyperBackground 帧循环）——
// Rust analyzer.rs 已做第一级帧率无关衰减，这里补一层 attack/release，
// 并统一换算为与帧率无关的系数，避免 120Hz(ProMotion)/30fps 上手感不同。
const REF_FPS = 60            // 参考帧率：Android 每帧系数按 ~60fps 调校
const LEVEL_ATTACK = 0.12     // level 上升系数（@REF_FPS 每帧）
const LEVEL_RELEASE = 0.045   // level 下降系数
const BEAT_ATTACK = 0.46      // beat 上升系数
const BEAT_RELEASE = 0.12     // beat 下降系数
const BEAT_SCALE = 0.94       // 对齐 Android targetBeat = beat * 0.94
let smoothLevel = 0
let smoothBeat = 0

// —— 调色板过渡：基于时间的 520ms smoothStep（对齐 Android，帧率无关）——
const PALETTE_TRANSITION_MS = 520
const smoothColors: number[][] = [
  [0.4, 0.31, 0.64, 1],
  [0.49, 0.36, 0.75, 1],
  [0.56, 0.49, 0.69, 1],
  [0.29, 0.24, 0.43, 1],
]
let smoothLightOffset = 0
let smoothSaturateOffset = 0
// 过渡起点快照 + 当前目标记录
const transStartColors: number[][] = smoothColors.map((c) => c.slice())
let transStartLight = 0
let transStartSaturate = 0
let transStartMs = 0
let transitioning = false
const targetColors: number[][] = smoothColors.map((c) => c.slice())
let targetLight = 0
let targetSaturate = 0

function lerpVal(current: number, target: number, t: number): number {
  return current + (target - current) * t
}

// 将「@REF_FPS 每帧系数」换算为与帧率无关的等效系数
function frameIndependentRate(perFrameRate: number, dt: number): number {
  return 1 - Math.pow(1 - perFrameRate, dt * REF_FPS)
}

// 3t^2 - 2t^3，对齐 Android smoothStep01
function smoothStep01(t: number): number {
  const x = t < 0 ? 0 : t > 1 ? 1 : t
  return x * x * (3 - 2 * x)
}

// 目标调色板是否变化（切歌）→ 启动一次定时过渡
function colorsChanged(): boolean {
  for (let i = 0; i < 4; i++) {
    for (let j = 0; j < 4; j++) {
      if (targetColors[i][j] !== props.colors[i][j]) return true
    }
  }
  return targetLight !== props.lightOffset || targetSaturate !== props.saturateOffset
}

// Uniform locations
let uResolution: WebGLUniformLocation | null = null
let uTime: WebGLUniformLocation | null = null
let uMusicLevel: WebGLUniformLocation | null = null
let uBeat: WebGLUniformLocation | null = null
let uColor0: WebGLUniformLocation | null = null
let uColor1: WebGLUniformLocation | null = null
let uColor2: WebGLUniformLocation | null = null
let uColor3: WebGLUniformLocation | null = null
let uDarkMode: WebGLUniformLocation | null = null
let uLightOffset: WebGLUniformLocation | null = null
let uSaturateOffset: WebGLUniformLocation | null = null

function compileShader(gl: WebGLRenderingContext, src: string, type: number): WebGLShader | null {
  const shader = gl.createShader(type)
  if (!shader) return null
  gl.shaderSource(shader, src)
  gl.compileShader(shader)
  if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
    console.error('Shader compile error:', gl.getShaderInfoLog(shader))
    gl.deleteShader(shader)
    return null
  }
  return shader
}

function initGL() {
  if (!canvas.value) return
  gl = canvas.value.getContext('webgl', { alpha: true, antialias: false, premultipliedAlpha: false })
  if (!gl) { console.error('WebGL not supported'); return }

  const vs = compileShader(gl, hyperBackgroundVertexShader, gl.VERTEX_SHADER)
  const fs = compileShader(gl, hyperBackgroundFragmentShader, gl.FRAGMENT_SHADER)
  if (!vs || !fs) return

  program = gl.createProgram()!
  gl.attachShader(program, vs)
  gl.attachShader(program, fs)
  gl.linkProgram(program)
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    console.error('Program link error:', gl.getProgramInfoLog(program))
    return
  }
  gl.useProgram(program)

  // 全屏四边形
  const buffer = gl.createBuffer()
  gl.bindBuffer(gl.ARRAY_BUFFER, buffer)
  gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1,-1, 1,-1, -1,1, 1,1]), gl.STATIC_DRAW)
  const aPos = gl.getAttribLocation(program, 'a_position')
  gl.enableVertexAttribArray(aPos)
  gl.vertexAttribPointer(aPos, 2, gl.FLOAT, false, 0, 0)

  // Uniforms
  uResolution = gl.getUniformLocation(program, 'u_resolution')
  uTime = gl.getUniformLocation(program, 'u_time')
  uMusicLevel = gl.getUniformLocation(program, 'u_musicLevel')
  uBeat = gl.getUniformLocation(program, 'u_beat')
  uColor0 = gl.getUniformLocation(program, 'u_color0')
  uColor1 = gl.getUniformLocation(program, 'u_color1')
  uColor2 = gl.getUniformLocation(program, 'u_color2')
  uColor3 = gl.getUniformLocation(program, 'u_color3')
  uDarkMode = gl.getUniformLocation(program, 'u_darkMode')
  uLightOffset = gl.getUniformLocation(program, 'u_lightOffset')
  uSaturateOffset = gl.getUniformLocation(program, 'u_saturateOffset')

  // 初始化平滑颜色为当前 props
  for (let i = 0; i < 4; i++) {
    for (let j = 0; j < 4; j++) {
      smoothColors[i][j] = props.colors[i][j]
      targetColors[i][j] = props.colors[i][j]
    }
  }
  smoothLightOffset = props.lightOffset
  smoothSaturateOffset = props.saturateOffset
  targetLight = props.lightOffset
  targetSaturate = props.saturateOffset

  startTime = performance.now() / 1000
  lastFrameMs = performance.now()
  render()
}

function render() {
  if (!gl || !program) return
  const c = canvas.value!
  // mac/GPU 富余场景允许到 2.0，保证 Retina/HiDPI 清晰度；其余仍限制以省 GPU
  const dprCap = window.devicePixelRatio >= 2 ? 2.0 : 1.5
  const dpr = Math.min(window.devicePixelRatio || 1, dprCap)
  const w = c.clientWidth * dpr
  const h = c.clientHeight * dpr
  if (c.width !== w || c.height !== h) { c.width = w; c.height = h }
  gl.viewport(0, 0, w, h)

  const nowMs = performance.now()
  const dt = Math.min((nowMs - lastFrameMs) / 1000, 0.1) // 秒，钳制避免卡顿后跳变
  lastFrameMs = nowMs

  // —— 调色板过渡：检测目标变化 → 启动 520ms 定时 smoothStep 过渡 ——
  if (colorsChanged()) {
    for (let i = 0; i < 4; i++) transStartColors[i] = smoothColors[i].slice()
    transStartLight = smoothLightOffset
    transStartSaturate = smoothSaturateOffset
    for (let i = 0; i < 4; i++) {
      for (let j = 0; j < 4; j++) targetColors[i][j] = props.colors[i][j]
    }
    targetLight = props.lightOffset
    targetSaturate = props.saturateOffset
    transStartMs = nowMs
    transitioning = true
  }
  if (transitioning) {
    const raw = (nowMs - transStartMs) / PALETTE_TRANSITION_MS
    const f = smoothStep01(raw)
    for (let i = 0; i < 4; i++) {
      for (let j = 0; j < 4; j++) {
        smoothColors[i][j] = lerpVal(transStartColors[i][j], targetColors[i][j], f)
      }
    }
    smoothLightOffset = lerpVal(transStartLight, targetLight, f)
    smoothSaturateOffset = lerpVal(transStartSaturate, targetSaturate, f)
    if (raw >= 1) transitioning = false
  }

  const time = performance.now() / 1000 - startTime
  gl.uniform2f(uResolution, w, h)
  gl.uniform1f(uTime, time)

  // —— 音乐律动第二级非对称平滑（帧率无关）——
  const targetLevel = Math.min(Math.max(props.musicLevel, 0), 1)
  const targetBeat = Math.min(Math.max(props.beatImpulse * BEAT_SCALE, 0), 1)
  const levelRate = frameIndependentRate(
    targetLevel > smoothLevel ? LEVEL_ATTACK : LEVEL_RELEASE, dt)
  const beatRate = frameIndependentRate(
    targetBeat > smoothBeat ? BEAT_ATTACK : BEAT_RELEASE, dt)
  smoothLevel += (targetLevel - smoothLevel) * levelRate
  smoothBeat += (targetBeat - smoothBeat) * beatRate

  gl.uniform1f(uMusicLevel, smoothLevel)
  gl.uniform1f(uBeat, smoothBeat)
  gl.uniform4fv(uColor0, smoothColors[0])
  gl.uniform4fv(uColor1, smoothColors[1])
  gl.uniform4fv(uColor2, smoothColors[2])
  gl.uniform4fv(uColor3, smoothColors[3])
  gl.uniform1f(uDarkMode, props.isDark ? 1.0 : 0.0)
  gl.uniform1f(uLightOffset, smoothLightOffset)
  gl.uniform1f(uSaturateOffset, smoothSaturateOffset)

  gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4)
  animFrame = requestAnimationFrame(render)
}

onMounted(initGL)
onUnmounted(() => cancelAnimationFrame(animFrame))
</script>

<template>
  <canvas ref="canvas" class="hyper-bg" />
</template>

<style scoped>
.hyper-bg {
  position: absolute;
  inset: 0;
  width: 100%;
  height: 100%;
  z-index: 0;
  opacity: 0.80; /* 对齐 Android graphicsLayer { alpha = 0.80f }，让底色透出 */
}
</style>
