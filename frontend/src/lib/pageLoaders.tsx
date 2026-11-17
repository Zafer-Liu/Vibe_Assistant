import { lazy, Suspense, useEffect, useRef, type ReactNode } from 'react'
import { useTranslation } from 'react-i18next'
import { ErrorBoundary } from '../components/ErrorBoundary'
import { afterNextPaint } from './startupPerformance'

const loadSkillLibrary = () => import('../pages/SkillLibrary').then(module => ({ default: module.SkillLibrary }))

export const PortManager = lazy(() => import('../pages/PortManager').then(module => ({ default: module.PortManager })))
export const NetworkManager = lazy(() => import('../pages/NetworkManager').then(module => ({ default: module.NetworkManager })))
export const EnvManager = lazy(() => import('../pages/EnvManager').then(module => ({ default: module.EnvManager })))
export const Settings = lazy(() => import('../pages/Settings').then(module => ({ default: module.Settings })))
export const SkillLibrary = lazy(loadSkillLibrary)
export const PublishedSkills = lazy(() => import('../pages/PublishedSkills').then(module => ({ default: module.PublishedSkills })))
export const SkillMarketplace = lazy(() => import('../pages/SkillMarketplace').then(module => ({ default: module.SkillMarketplace })))
export const McpLibrary = lazy(() => import('../pages/McpLibrary').then(module => ({ default: module.McpLibrary })))
export const UsageAnalytics = lazy(() => import('../pages/UsageAnalytics').then(module => ({ default: module.UsageAnalytics })))
export const PendingMemories = lazy(() => import('../pages/PendingMemories').then(module => ({ default: module.PendingMemories })))
export const OrganizedConversations = lazy(() => import('../pages/OrganizedConversations').then(module => ({ default: module.OrganizedConversations })))
export const MemoryInjection = lazy(() => import('../pages/MemoryInjection').then(module => ({ default: module.MemoryInjection })))
export const MemoryCapture = lazy(() => import('../pages/MemoryCapture').then(module => ({ default: module.MemoryCapture })))
export const MemoryHelp = lazy(() => import('../pages/MemoryHelp').then(module => ({ default: module.MemoryHelp })))
export const MemoryBrainGraph = lazy(() => import('../pages/MemoryBrainGraph').then(module => ({ default: module.MemoryBrainGraph })))

/** Keep each retained page's loading and error state inside its own subtree. */
export function PageLoadBoundary({ children }: { children: ReactNode }) {
  const { t } = useTranslation()
  return <ErrorBoundary>
    <Suspense fallback={<div role="status" aria-live="polite" className="flex flex-1 items-center justify-center p-6 text-sm text-gray-500 dark:text-gray-400">{t('common.loading')}</div>}>
      {children}
    </Suspense>
  </ErrorBoundary>
}

/** Warm only Skill Library, the next core module after Memory, once per mount. */
export function useLikelyPagePreload(enabled: boolean) {
  const started = useRef(false)
  useEffect(() => {
    if (!enabled || started.current) return

    let cancelled = false
    let idleHandle: number | undefined
    let timer: number | undefined
    const preload = () => {
      if (cancelled || document.visibilityState !== 'visible') return
      started.current = true
      // Speculation must not surface an unhandled rejection or poison React.lazy.
      void loadSkillLibrary().catch(() => {})
    }
    const cancelPaint = afterNextPaint(() => {
      if (cancelled || document.visibilityState !== 'visible') return
      if (typeof window.requestIdleCallback === 'function' && typeof window.cancelIdleCallback === 'function') {
        idleHandle = window.requestIdleCallback(preload, { timeout: 2000 })
      } else {
        timer = window.setTimeout(preload, 1000)
      }
    })

    return () => {
      cancelled = true
      cancelPaint()
      if (idleHandle !== undefined) window.cancelIdleCallback(idleHandle)
      if (timer !== undefined) window.clearTimeout(timer)
    }
  }, [enabled])
}
