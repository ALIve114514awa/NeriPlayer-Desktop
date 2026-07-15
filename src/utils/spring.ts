/**
 * Spring — 移植自 AMLL (applemusic-like-lyrics) core 的解析式弹簧求解器
 * Ref: applemusic-like-lyrics/packages/core/src/utils/spring.ts
 *
 * 与常见的欧拉数值积分不同，这里用弹簧微分方程的「闭式解」：
 * 每次目标/参数变化时求解一个关于时间 t 的解析函数 currentSolver(t)，
 * 之后逐帧仅把累积时间代入即可，天然对掉帧稳定、无累积误差
 */

export interface SpringParams {
  /** 质量，越大越「重」越慢 */
  mass: number
  /** 阻力，越大回弹越少 */
  damping: number
  /** 弹力，越大越「急」 */
  stiffness: number
  /** 强制过阻尼（无回弹），默认 false */
  soft?: boolean
}

type Solver = (t: number) => number

/** 数值求导，用于从解析解反推速度/加速度 */
function derivative(f: Solver): Solver {
  const h = 0.001
  return (x: number) => (f(x + h) - f(x - h)) / (2 * h)
}

/**
 * 求解弹簧微分方程，返回一个「给定时间 t 得到位置」的解析函数
 * @param from 起始位置
 * @param velocity 起始速度
 * @param to 目标位置
 * @param delay 延迟秒数（用于逐行错峰级联）
 * @param params 弹簧参数
 */
function solveSpring(
  from: number,
  velocity: number,
  to: number,
  delay: number,
  params: Partial<SpringParams>,
): Solver {
  const soft = params.soft ?? false
  const stiffness = params.stiffness ?? 100
  const damping = params.damping ?? 10
  const mass = params.mass ?? 1
  const delta = to - from

  // 过阻尼 / soft：临界或以上，无振荡
  if (soft || 1 <= damping / (2 * Math.sqrt(stiffness * mass))) {
    const angularFrequency = -Math.sqrt(stiffness / mass)
    const leftover = -angularFrequency * delta - velocity
    return (t: number) => {
      t -= delay
      if (t < 0) return from
      return to - (delta + t * leftover) * Math.E ** (t * angularFrequency)
    }
  }

  // 欠阻尼：带衰减振荡（Apple Music 的回弹手感）
  const dampingFrequency = Math.sqrt(4 * mass * stiffness - damping ** 2)
  const leftover = (damping * delta - 2 * mass * velocity) / dampingFrequency
  const dfm = (0.5 * dampingFrequency) / mass
  const dm = -(0.5 * damping) / mass
  return (t: number) => {
    t -= delay
    if (t < 0) return from
    return to - (Math.cos(t * dfm) * delta + Math.sin(t * dfm) * leftover) * Math.E ** (t * dm)
  }
}

export class Spring {
  private currentPosition: number
  private targetPosition: number
  private currentTime = 0
  private params: Partial<SpringParams> = {}
  private currentSolver: Solver
  private getV: Solver
  private getV2: Solver
  private queueParams?: Partial<SpringParams> & { time: number }
  private queuePosition?: { position: number; time: number }

  constructor(currentPosition = 0) {
    this.targetPosition = currentPosition
    this.currentPosition = currentPosition
    this.currentSolver = () => this.targetPosition
    this.getV = () => 0
    this.getV2 = () => 0
  }

  private resetSolver(): void {
    const curV = this.getV(this.currentTime)
    this.currentTime = 0
    this.currentSolver = solveSpring(this.currentPosition, curV, this.targetPosition, 0, this.params)
    this.getV = derivative(this.currentSolver)
    this.getV2 = derivative(this.getV)
  }

  /** 是否静止（位置误差与速度/加速度均低于阈值，且无待处理队列） */
  arrived(): boolean {
    return (
      Math.abs(this.targetPosition - this.currentPosition) < 0.01 &&
      Math.abs(this.getV(this.currentTime)) < 0.01 &&
      Math.abs(this.getV2(this.currentTime)) < 0.01 &&
      this.queueParams === undefined &&
      this.queuePosition === undefined
    )
  }

  /** 直接把弹簧「钉」在某位置（无过渡） */
  setPosition(position: number): void {
    this.targetPosition = position
    this.currentPosition = position
    this.currentSolver = () => this.targetPosition
    this.getV = () => 0
    this.getV2 = () => 0
    this.currentTime = 0
    this.queueParams = undefined
    this.queuePosition = undefined
  }

  /** 逐帧推进，delta 单位为秒 */
  update(delta = 0): void {
    this.currentTime += delta
    this.currentPosition = this.currentSolver(this.currentTime)

    if (this.queueParams) {
      this.queueParams.time -= delta
      if (this.queueParams.time <= 0) {
        const p = this.queueParams
        this.queueParams = undefined
        this.updateParams(p)
      }
    }
    if (this.queuePosition) {
      this.queuePosition.time -= delta
      if (this.queuePosition.time <= 0) {
        const pos = this.queuePosition.position
        this.queuePosition = undefined
        this.setTargetPosition(pos)
      }
    }

    if (this.arrived()) this.setPosition(this.targetPosition)
  }

  updateParams(params: Partial<SpringParams>, delay = 0): void {
    if (delay > 0) {
      this.queueParams = { ...(this.queueParams ?? {}), ...params, time: delay }
    } else {
      this.params = { ...this.params, ...params }
      this.resetSolver()
    }
  }

  /**
   * 设置目标位置
   * @param delay 延迟秒数，>0 时用于逐行错峰级联
   */
  setTargetPosition(targetPosition: number, delay = 0): void {
    if (delay > 0) {
      this.queuePosition = { position: targetPosition, time: delay }
    } else {
      this.queuePosition = undefined
      this.targetPosition = targetPosition
      this.resetSolver()
    }
  }

  getCurrentPosition(): number {
    return this.currentPosition
  }
}
