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
let beatEnvelope = 0

// 平滑过渡状态（对齐 Android 逐帧 lerp，消除切歌闪烁）
const LERP_SPEED = 0.032 // 每帧插值速率，过渡更柔和，切歌时更接近网易云式铺开
const smoothColors: number[][] = [
  [0.4, 0.31, 0.64, 1],
  [0.49, 0.36, 0.75, 1],
  [0.56, 0.49, 0.69, 1],
  [0.29, 0.24, 0.43, 1],
]
let smoothLightOffset = 0
let smoothSaturateOffset = 0

function lerpVal(current: number, target: number, t: number): number {
  return current + (target - current) * t
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
    }
  }
  smoothLightOffset = props.lightOffset
  smoothSaturateOffset = props.saturateOffset

  startTime = performance.now() / 1000
  render()
}

function render() {
  if (!gl || !program) return
  const c = canvas.value!
  const dpr = Math.min(window.devicePixelRatio || 1, 1.5)  // 限制 DPR，节省 GPU
  const w = c.clientWidth * dpr
  const h = c.clientHeight * dpr
  if (c.width !== w || c.height !== h) { c.width = w; c.height = h }
  gl.viewport(0, 0, w, h)

  // 逐帧 lerp 颜色（对齐 Android 的平滑过渡，消除切歌时的画面闪烁）
  for (let i = 0; i < 4; i++) {
    for (let j = 0; j < 4; j++) {
      smoothColors[i][j] = lerpVal(smoothColors[i][j], props.colors[i][j], LERP_SPEED)
    }
  }
  smoothLightOffset = lerpVal(smoothLightOffset, props.lightOffset, LERP_SPEED)
  smoothSaturateOffset = lerpVal(smoothSaturateOffset, props.saturateOffset, LERP_SPEED)

  const time = performance.now() / 1000 - startTime
  gl.uniform2f(uResolution, w, h)
  gl.uniform1f(uTime, time)
  gl.uniform1f(uMusicLevel, props.musicLevel)
  // Beat 脉冲衰减：0.92/frame @60fps → ~500ms 自然衰减，对齐 Android beatEnv * 0.92f
  beatEnvelope = Math.max(beatEnvelope * 0.92, props.beatImpulse)
  gl.uniform1f(uBeat, beatEnvelope)
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
