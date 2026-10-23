import { useTranslation } from 'react-i18next'

import type { SkillDriftFile } from '../../../types/skill'
import { lineDiff, type DiffLine } from '../lineDiff'

/** 文件级 tab + 红绿行级 diff 渲染：DriftDialog 与已发布页仲裁对话框共用。 */
export function SkillDiffPane({ files, selected, onSelect }: {
  files: SkillDriftFile[]
  selected: number
  onSelect: (index: number) => void
}) {
  const { t } = useTranslation()
  const file = files.length > 0 ? files[Math.min(selected, files.length - 1)] : null
  let diffLines: DiffLine[] = []
  let diffNote: string | null = null
  if (file) {
    if (file.shared_text === null && file.local_text === null) {
      diffNote = t('skills.driftBinary')
    } else if (file.shared_text === null) {
      diffNote = t('skills.driftOnlyLocal')
      diffLines = (file.local_text ?? '').split('\n').map(text => ({ kind: 'add' as const, text }))
    } else if (file.local_text === null) {
      diffNote = t('skills.driftOnlyShared')
      diffLines = (file.shared_text ?? '').split('\n').map(text => ({ kind: 'del' as const, text }))
    } else {
      diffLines = lineDiff(file.shared_text, file.local_text)
    }
  }
  return (
    <div className="space-y-3">
      <div className="flex flex-wrap gap-1.5">
        {files.map((f, index) => (
          <button
            key={f.path}
            onClick={() => onSelect(index)}
            className={`rounded-full px-2.5 py-1 font-mono text-[11px] font-medium transition ${
              index === selected
                ? 'bg-violet-600 text-white'
                : 'bg-gray-100 text-gray-600 hover:bg-gray-200 dark:bg-gray-800 dark:text-gray-300 dark:hover:bg-gray-700'
            }`}
          >
            {f.path}
          </button>
        ))}
      </div>
      <div className="flex items-center gap-3 text-[10px] text-gray-500 dark:text-gray-400">
        <span className="inline-flex items-center gap-1"><span className="inline-block h-2 w-2 rounded-sm bg-rose-400" />{t('skills.driftLegendShared')}</span>
        <span className="inline-flex items-center gap-1"><span className="inline-block h-2 w-2 rounded-sm bg-emerald-400" />{t('skills.driftLegendLocal')}</span>
      </div>
      {diffNote && <p className="text-xs text-amber-600 dark:text-amber-400">{diffNote}</p>}
      <pre className="max-h-72 overflow-auto rounded-xl bg-gray-50 p-3 font-mono text-[11px] leading-5 dark:bg-gray-800/60">
        {diffLines.map((line, index) => (
          <div
            key={index}
            className={
              line.kind === 'del'
                ? 'bg-rose-500/10 text-rose-700 dark:text-rose-300'
                : line.kind === 'add'
                  ? 'bg-emerald-500/10 text-emerald-700 dark:text-emerald-300'
                  : 'text-gray-600 dark:text-gray-300'
            }
          >
            <span className="mr-1.5 inline-block w-3 select-none text-center opacity-50">{line.kind === 'del' ? '-' : line.kind === 'add' ? '+' : ' '}</span>
            {line.text || ' '}
          </div>
        ))}
      </pre>
    </div>
  )
}
