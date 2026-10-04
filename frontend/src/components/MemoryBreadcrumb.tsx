import { ChevronRight } from 'lucide-react'
import { useTranslation } from 'react-i18next'

interface MemoryBreadcrumbProps {
  /** i18n key for the current page title, e.g. 'memory.pending.title' */
  currentKey: string
  onBack: () => void
}

/** Breadcrumb bar: 记忆中心 / 当前子页面 — gives spatial context on sub-pages. */
export function MemoryBreadcrumb({ currentKey, onBack }: MemoryBreadcrumbProps) {
  const { t } = useTranslation()
  return (
    <nav aria-label={t('common.breadcrumb')} className="flex items-center gap-1 text-sm">
      <button
        type="button"
        onClick={onBack}
        className="rounded text-slate-500 transition-colors hover:text-blue-600 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 dark:text-slate-400 dark:hover:text-blue-400"
      >
        {t('memory.title')}
      </button>
      <ChevronRight size={14} className="text-slate-400 dark:text-slate-500" />
      <span aria-current="page" className="font-medium text-slate-900 dark:text-slate-100">{t(currentKey)}</span>
    </nav>
  )
}
