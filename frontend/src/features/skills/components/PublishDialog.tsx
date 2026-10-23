import { useTranslation } from 'react-i18next'

import { Check, ShieldCheck, Loader2, X } from 'lucide-react'

import type { SkillDocument } from '../../../types/skill'
import { SOURCES, sourceColor } from '../sources'

export function PublishDialog({
  skill, targets, setTargets, loading, onConfirm, onClose,
}: {
  skill: SkillDocument
  targets: Set<string>
  setTargets: (v: Set<string> | ((prev: Set<string>) => Set<string>)) => void
  loading: boolean
  onConfirm: () => void
  onClose: () => void
}) {
  const { t } = useTranslation()
  const toggleTarget = (id: string) => {
    setTargets((prev) => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })
  }

  return (
    <div className="fixed inset-0 z-40 flex items-center justify-center p-4" role="dialog" aria-modal="true" aria-label={t('skills.publishDialogAria')}>
      <button aria-label={t('skills.dialogCancel')} onClick={onClose} className="absolute inset-0 cursor-default bg-gray-950/40 backdrop-blur-[2px]" />
      <div className="relative max-h-[85vh] w-full max-w-md overflow-y-auto rounded-2xl border border-gray-200 bg-white p-5 shadow-2xl dark:border-gray-700 dark:bg-gray-900">
        <div className="flex items-center gap-2">
          <div className="rounded-lg bg-violet-500/10 p-1.5 text-violet-600 dark:text-violet-400">
            <ShieldCheck size={16} />
          </div>
          <h2 className="text-sm font-semibold text-gray-800 dark:text-gray-100">{t('skills.publishDialogTitle')}</h2>
          <button onClick={onClose} aria-label={t('skills.dialogCancel')} className="ml-auto rounded-md p-1.5 text-gray-500 transition hover:bg-gray-100 hover:text-gray-600 dark:hover:bg-gray-800 dark:hover:text-gray-300">
            <X size={15} />
          </button>
        </div>

        <div className="mt-3 rounded-lg border border-gray-200 p-3 dark:border-gray-700">
          <div className="flex items-center gap-2">
            <span className={`inline-block h-2 w-2 rounded-full ${sourceColor(skill.item.source)}`} />
            <span className="truncate text-sm font-medium text-gray-800 dark:text-gray-100">{skill.item.name}</span>
            <span className="ml-auto shrink-0 text-xs text-gray-500">{skill.item.source}</span>
          </div>
          {skill.item.description && <p className="mt-1.5 line-clamp-2 text-xs text-gray-500 dark:text-gray-400">{skill.item.description}</p>}
        </div>

        <p className="mb-2 mt-4 text-xs font-medium text-gray-600 dark:text-gray-400">{t('skills.publishDialogTargets')}</p>
        <div className="grid grid-cols-2 gap-2">
          {SOURCES.map((agent) => {
            const checked = targets.has(agent.id)
            return (
              <label key={agent.id} className={`flex cursor-pointer items-center gap-2 rounded-lg border px-2.5 py-2 text-xs font-medium transition ${checked ? 'border-violet-500 bg-violet-50 text-violet-700 dark:bg-violet-500/20 dark:text-violet-200' : 'border-gray-300 text-gray-600 hover:bg-gray-50 dark:border-gray-600 dark:text-gray-300 dark:hover:bg-gray-800'}`}>
                <input type="checkbox" checked={checked} onChange={() => toggleTarget(agent.id)} className="h-3.5 w-3.5 accent-violet-600" />
                <span className={`h-1.5 w-1.5 rounded-full ${agent.color}`} />
                {agent.label}
              </label>
            )
          })}
        </div>

        <p className="mt-3 text-xs leading-5 text-gray-500 dark:text-gray-400">{t('skills.publishDialogHint')}</p>

        <div className="mt-4 flex items-center justify-end gap-2">
          <button onClick={onClose} disabled={loading} className="rounded-lg border border-gray-300 px-3 py-1.5 text-xs font-medium text-gray-700 hover:bg-gray-50 disabled:opacity-50 dark:border-gray-600 dark:text-gray-200 dark:hover:bg-gray-800">{t('skills.dialogCancel')}</button>
          <button onClick={onConfirm} disabled={loading} className="inline-flex items-center gap-1.5 rounded-lg bg-violet-600 px-3 py-1.5 text-xs font-medium text-white transition hover:bg-violet-500 disabled:opacity-50">
            {loading ? <Loader2 size={13} className="animate-spin" /> : <Check size={13} />}
            {t('skills.publishAssign')}
          </button>
        </div>
      </div>
    </div>
  )
}
