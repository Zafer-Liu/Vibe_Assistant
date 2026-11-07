type StartupPhase =
  | 'app_shell_painted'
  | 'memory_shell_painted'
  | 'memory_l1'
  | 'memory_stats'
  | 'memory_domains'
  | 'memory_importance'
  | 'memory_l2'
  | 'memory_l3'

const recordedPhases = new Set<StartupPhase>()

/** At most one numeric timing/outcome per fixed phase in this WebView session. */
export function recordStartupPhase(phase: StartupPhase, elapsedMs: number, succeeded: 0 | 1 = 1) {
  if (recordedPhases.has(phase)) return
  recordedPhases.add(phase)
  const duration = Number.isFinite(elapsedMs) ? Math.round(Math.max(0, elapsedMs)) : 0
  console.info(`[startup-perf] phase=${phase} elapsed_ms=${duration} succeeded=${succeeded}`)
}

export async function timeStartupPhase<T>(phase: StartupPhase, task: () => Promise<T>): Promise<T> {
  const started = performance.now()
  let succeeded: 0 | 1 = 0
  try {
    const result = await task()
    succeeded = 1
    return result
  } finally {
    recordStartupPhase(phase, performance.now() - started, succeeded)
  }
}

/** Leave a paint opportunity between the two animation-frame callbacks. */
export function afterNextPaint(callback: () => void): () => void {
  let frame = requestAnimationFrame(() => { frame = requestAnimationFrame(callback) })
  return () => cancelAnimationFrame(frame)
}
