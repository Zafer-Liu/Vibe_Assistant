import { useEffect, useRef, useSyncExternalStore } from 'react'

function getDocumentVisible(): boolean {
  return typeof document !== 'undefined' && document.visibilityState === 'visible'
}

function subscribeVisibility(onChange: () => void): () => void {
  if (typeof document === 'undefined') return () => {}
  document.addEventListener('visibilitychange', onChange)
  return () => document.removeEventListener('visibilitychange', onChange)
}

function getServerSnapshot(): boolean {
  return false
}

export function useDocumentVisible(): boolean {
  return useSyncExternalStore(subscribeVisibility, getDocumentVisible, getServerSnapshot)
}

/**
 * Callers should stabilize task with useCallback and return all asynchronous work.
 * Poll immediately while visible/enabled, then wait delayMs after each completion.
 * In-flight work is not cancelled; resuming waits for it before refreshing again.
 */
export function useVisiblePolling(
  task: () => void | Promise<unknown>,
  delayMs: number,
  enabled = true,
): void {
  const visible = useDocumentVisible()
  // Survives effect restarts, including StrictMode and hide/show during a request.
  const inFlight = useRef<Promise<void> | null>(null)

  useEffect(() => {
    if (!visible || !enabled || !Number.isFinite(delayMs) || delayMs <= 0) return

    let disposed = false
    let timer: ReturnType<typeof setTimeout> | undefined
    const isActive = () => !disposed && getDocumentVisible()

    const poll = async () => {
      if (inFlight.current) await inFlight.current
      if (!isActive()) return

      // Normalize synchronous throws and rejections without logging user data.
      const pending = Promise.resolve().then(task).then(
        () => {},
        () => {},
      )
      inFlight.current = pending
      await pending
      if (inFlight.current === pending) inFlight.current = null

      if (isActive()) timer = setTimeout(() => { void poll() }, delayMs)
    }

    // Let StrictMode's setup/cleanup replay finish before issuing the first call.
    void Promise.resolve().then(poll)
    return () => {
      disposed = true
      if (timer !== undefined) clearTimeout(timer)
    }
  }, [task, delayMs, enabled, visible])
}
