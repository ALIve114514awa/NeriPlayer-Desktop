/**
 * KaraokeLine — 移植 AMLL 核心逻辑
 * 使用 Web Animation API 驱动逐字高亮和浮动动画
 * 零 Vue 响应式开销，所有动画由浏览器原生调度
 */

import type { LyricWord } from '@/stores/player'

interface RealWord {
  word: LyricWord
  element: HTMLSpanElement
  floatAnimation: Animation | null
  maskAnimation: Animation | null
  width: number
  height: number
}

/**
 * 生成 mask-image 渐变字符串和总宽度比例
 * 对齐 AMLL generateFadeGradient
 */
function generateFadeGradient(
  widthRatio: number,
  bright = 'rgba(0,0,0,1)',
  dark = 'rgba(0,0,0,0.2)',
): [string, number] {
  const totalAspect = 2 + widthRatio
  const widthInTotal = widthRatio / totalAspect
  const leftPos = (1 - widthInTotal) / 2
  return [
    `linear-gradient(to right, ${bright} ${leftPos * 100}%, ${dark} ${(leftPos + widthInTotal) * 100}%)`,
    totalAspect,
  ]
}

export class KaraokeLine {
  private words: RealWord[] = []
  private lineStartTime = 0
  private totalDuration = 0
  private isEnabled = false

  /**
   * 构建歌词行 DOM 和 Animation
   * @param container 父容器元素
   * @param lyricWords 逐字数据
   * @param lineStart 行开始时间 ms
   * @param lineEnd 行结束时间 ms
   */
  build(container: HTMLElement, lyricWords: LyricWord[], lineStart: number, lineEnd: number): void {
    this.dispose()
    this.lineStartTime = lineStart
    this.totalDuration = Math.max(lineEnd, lyricWords.length > 0
      ? Math.max(...lyricWords.map(w => w.startMs + w.durationMs))
      : lineEnd) - lineStart

    // 创建每个 word 的 <span>
    for (const w of lyricWords) {
      const span = document.createElement('span')
      span.textContent = w.text
      span.className = 'kw'
      container.appendChild(span)

      const realWord: RealWord = {
        word: w,
        element: span,
        floatAnimation: null,
        maskAnimation: null,
        width: 0,
        height: 0,
      }
      this.words.push(realWord)
    }

    // DOM 挂载后测量尺寸，生成动画
    requestAnimationFrame(() => {
      for (const rw of this.words) {
        rw.width = rw.element.offsetWidth
        rw.height = rw.element.offsetHeight
      }
      this.initFloatAnimations()
      this.initMaskAnimations()
    })
  }

  /**
   * 浮动动画 — 对齐 AMLL initFloatAnimation
   * translateY(0) → translateY(-0.05em)
   */
  private initFloatAnimations(): void {
    for (const rw of this.words) {
      const delay = rw.word.startMs - this.lineStartTime
      const duration = Math.max(1000, rw.word.durationMs)

      // 对齐参考项目：4px 浮动幅度
      const anim = rw.element.animate(
        [
          { transform: 'translateY(0)' },
          { transform: 'translateY(-4px)' },
        ],
        {
          duration,
          delay,
          fill: 'both',
          easing: 'ease-out',
          composite: 'add',
        },
      )
      anim.pause()
      rw.floatAnimation = anim
    }
  }

  /**
   * 逐字高亮 mask 动画 — 对齐 AMLL generateWebAnimationBasedMaskImage
   * 每个 word 有独立的 mask-position 动画
   */
  private initMaskAnimations(): void {
    if (this.totalDuration <= 0) return

    for (const rw of this.words) {
      const el = rw.element
      const fadeWidth = rw.height * 0.5

      // 设置 mask-image
      const [maskImage, totalAspect] = generateFadeGradient(
        fadeWidth / Math.max(1, rw.width),
      )
      el.style.maskImage = maskImage
      el.style.webkitMaskImage = maskImage
      el.style.maskRepeat = 'no-repeat'
      el.style.maskSize = `${totalAspect * 100}% 100%`

      // 计算所有 word 在此 word 之前的总宽度
      const idx = this.words.indexOf(rw)
      const widthBefore = this.words.slice(0, idx).reduce((s, w) => s + w.width, 0)
        + (idx > 0 ? fadeWidth : 0)

      const minOffset = -(rw.width + fadeWidth)
      const clamp = (x: number) => Math.max(minOffset, Math.min(0, x))

      // 预计算关键帧
      const frames: Keyframe[] = []
      let curPos = -widthBefore - rw.width - fadeWidth
      let timeOffset = 0
      let lastTimeStamp = 0

      // 初始帧
      frames.push({ offset: 0, maskPosition: `${clamp(curPos)}px 0` })

      // 遍历所有 word 的时序生成帧
      for (let j = 0; j < this.words.length; j++) {
        const otherWord = this.words[j].word

        // 停顿段
        const curTimeStamp = otherWord.startMs - this.lineStartTime
        const staticDuration = curTimeStamp - lastTimeStamp
        timeOffset += staticDuration / this.totalDuration
        if (staticDuration > 0 && timeOffset > 0 && timeOffset <= 1) {
          frames.push({ offset: Math.min(1, timeOffset), maskPosition: `${clamp(curPos)}px 0` })
        }
        lastTimeStamp = curTimeStamp

        // 移动段
        const fadeDuration = Math.max(0, otherWord.durationMs)
        timeOffset += fadeDuration / this.totalDuration
        curPos += this.words[j].width
        if (j === 0) curPos += fadeWidth * 1.5
        if (j === this.words.length - 1) curPos += fadeWidth * 0.5

        if (fadeDuration > 0 && timeOffset > 0 && timeOffset <= 1) {
          frames.push({ offset: Math.min(1, timeOffset), maskPosition: `${clamp(curPos)}px 0` })
        }
        lastTimeStamp += fadeDuration
      }

      // 确保最后一帧
      if (frames.length > 0 && frames[frames.length - 1].offset !== 1) {
        frames.push({ offset: 1, maskPosition: `${clamp(curPos)}px 0` })
      }

      // 去重和排序
      const cleanFrames = dedupeFrames(frames)

      if (cleanFrames.length >= 2) {
        try {
          const anim = el.animate(cleanFrames, {
            duration: this.totalDuration || 1,
            fill: 'both',
          })
          anim.pause()
          rw.maskAnimation = anim
        } catch (e) {
          console.warn('[KaraokeLine] mask animation error:', e)
        }
      }
    }
  }

  /** 激活行 — 设置所有动画到指定时间并播放 */
  enable(currentTimeMs: number, shouldPlay = true): void {
    this.isEnabled = true
    const relativeTime = Math.max(0, currentTimeMs - this.lineStartTime)

    for (const rw of this.words) {
      if (rw.floatAnimation) {
        rw.floatAnimation.currentTime = relativeTime
        if (shouldPlay) rw.floatAnimation.play()
        else rw.floatAnimation.pause()
      }
      if (rw.maskAnimation) {
        rw.maskAnimation.currentTime = Math.min(this.totalDuration, relativeTime)
        if (shouldPlay) rw.maskAnimation.play()
        else rw.maskAnimation.pause()
      }
    }
  }

  /** 停用行 — 反向播放 float，暂停 mask */
  disable(): void {
    this.isEnabled = false
    for (const rw of this.words) {
      if (rw.floatAnimation) {
        rw.floatAnimation.playbackRate = -1
        rw.floatAnimation.play()
      }
      if (rw.maskAnimation) {
        rw.maskAnimation.pause()
      }
    }
  }

  /** seek 时快速定位 */
  seek(currentTimeMs: number): void {
    if (!this.isEnabled) return
    const t = Math.max(0, currentTimeMs - this.lineStartTime)
    for (const rw of this.words) {
      if (rw.maskAnimation) {
        rw.maskAnimation.currentTime = Math.min(this.totalDuration, t)
      }
    }
  }

  pause(): void {
    if (!this.isEnabled) return
    for (const rw of this.words) {
      rw.floatAnimation?.pause()
      rw.maskAnimation?.pause()
    }
  }

  resume(): void {
    if (!this.isEnabled) return
    for (const rw of this.words) {
      if (rw.floatAnimation && rw.floatAnimation.playbackRate > 0) {
        rw.floatAnimation.play()
      }
      rw.maskAnimation?.play()
    }
  }

  /** 获取当前行的高亮进度 (0..1)，用于辉光跟踪 */
  getProgress(): number {
    if (!this.isEnabled || this.totalDuration <= 0) return 0
    // 取第一个 mask animation 的 currentTime 作为参考
    for (const rw of this.words) {
      if (rw.maskAnimation) {
        const ct = (rw.maskAnimation.currentTime as number) || 0
        return Math.max(0, Math.min(1, ct / this.totalDuration))
      }
    }
    return 0
  }

  dispose(): void {
    for (const rw of this.words) {
      rw.floatAnimation?.cancel()
      rw.maskAnimation?.cancel()
      rw.element.remove()
    }
    this.words = []
  }
}

/** 去除重复 offset 的帧并排序 */
function dedupeFrames(frames: Keyframe[]): Keyframe[] {
  const seen = new Map<number, Keyframe>()
  for (const f of frames) {
    const offset = f.offset as number
    if (offset >= 0 && offset <= 1) {
      seen.set(offset, f)
    }
  }
  return Array.from(seen.values()).sort((a, b) => (a.offset as number) - (b.offset as number))
}
