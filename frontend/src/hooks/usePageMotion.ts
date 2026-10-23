import { useEffect, useRef } from 'react'
export function usePageMotion(page: string) {
  const host = useRef<HTMLDivElement>(null)
  useEffect(() => {
    if (window.matchMedia('(prefers-reduced-motion: reduce)').matches) return
    let animation: Animation | undefined
    const frame = requestAnimationFrame(() => {
      const visible = [...host.current?.children ?? []].find(child => getComputedStyle(child).display !== 'none')
      if (visible instanceof HTMLElement) animation = visible.animate([
        { opacity: .7, transform: 'translateY(7px)' },
        { opacity: 1, transform: 'translateY(0)' },
      ], { duration: 180, easing: 'cubic-bezier(.16,1,.3,1)' })
    })
    return () => { cancelAnimationFrame(frame); animation?.cancel() }
  }, [page])
  return host
}
