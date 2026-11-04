import { memo, useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Star, Pencil, Trash2, X } from 'lucide-react'
import { MemoryMarkdown } from '../../components/MemoryMarkdown'
import type { MemoryImportance, MemoryItem } from '../../types/memory'
import type { GraphNode, GraphTopology } from './types'

interface MemoryActions {
  onTogglePin: (memoryId: string, pinned: boolean) => Promise<void>
  onUpdateMemory: (memoryId: string, content: string) => Promise<void>
  onDeleteMemory: (memoryId: string) => Promise<void>
}

interface GraphInspectorDrawerProps extends MemoryActions {
  selectedNode: GraphNode
  topology: GraphTopology
  memoryById: Map<string, MemoryItem>
  importance: Record<string, MemoryImportance>
  onClose: () => void
  onJumpToNode: (node: GraphNode) => void
}

function SourceMemory({ memory, importance, onTogglePin, onUpdateMemory, onDeleteMemory }: MemoryActions & {
  memory: MemoryItem
  importance?: MemoryImportance
}) {
  const { t } = useTranslation()
  const [expanded, setExpanded] = useState(false)
  const [editing, setEditing] = useState(false)
  const [content, setContent] = useState(memory.memory)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const run = async (action: () => Promise<void>) => {
    if (busy) return
    setBusy(true)
    setError(null)
    try {
      await action()
    } catch (cause) {
      setError(String(cause))
    } finally {
      setBusy(false)
    }
  }
  const buttonClass = 'inline-flex items-center gap-1 rounded-md px-2 py-1 text-xs text-gray-600 hover:bg-gray-100 disabled:opacity-40 dark:text-gray-300 dark:hover:bg-gray-700'

  return (
    <details className="border-b border-gray-200 py-2 dark:border-gray-700" onToggle={(event) => setExpanded(event.currentTarget.open)}>
      <summary className="cursor-pointer text-xs leading-relaxed text-gray-800 focus-visible:outline-blue-500 dark:text-gray-100">
        {memory.memory.replace(/\s+/g, ' ').slice(0, 120)}{memory.memory.length > 120 ? '…' : ''}
      </summary>
      {expanded && (
        <div className="mt-3 space-y-2">
          {editing ? (
            <>
              <textarea
                value={content}
                onChange={(event) => setContent(event.target.value)}
                aria-label={t('memory.brainGraph.editSource')}
                rows={6}
                disabled={busy}
                className="w-full rounded-md border border-gray-300 bg-white p-2 text-xs text-gray-800 focus:outline-blue-500 dark:border-gray-600 dark:bg-neutral-900 dark:text-gray-100"
              />
              <div className="flex justify-end gap-2">
                <button type="button" disabled={busy} className={buttonClass} onClick={() => setEditing(false)}>{t('common.cancel')}</button>
                <button type="button" disabled={busy || !content.trim()} className={buttonClass} onClick={() => {
                  void run(async () => {
                    await onUpdateMemory(memory.id, content.trim())
                    setEditing(false)
                  })
                }}>{busy ? t('common.saving') : t('common.save')}</button>
              </div>
            </>
          ) : <div className="text-xs text-gray-800 dark:text-gray-100"><MemoryMarkdown content={memory.memory} /></div>}
          <div className="flex flex-wrap gap-1 text-[11px] text-gray-500 dark:text-gray-400">
            {(memory.domains ?? []).map((domain) => <span key={domain.id}>{domain.name}</span>)}
          </div>
          {!editing && (
            <div className="flex flex-wrap justify-end gap-1">
              <button type="button" disabled={busy} className={buttonClass} onClick={() => { void run(() => onTogglePin(memory.id, !importance?.pinned)) }}>
                <Star size={12} className={importance?.pinned ? 'fill-current' : ''} />
                {t(importance?.pinned ? 'memory.importanceUnpin' : 'memory.importancePin')}
              </button>
              <button type="button" disabled={busy} className={buttonClass} onClick={() => { setContent(memory.memory); setEditing(true) }}><Pencil size={12} />{t('common.edit')}</button>
              <button type="button" disabled={busy} className={buttonClass} onClick={() => { void run(() => onDeleteMemory(memory.id)) }}><Trash2 size={12} />{t('common.delete')}</button>
            </div>
          )}
          {error && <p role="alert" className="break-words text-xs text-red-600 dark:text-red-400">{error}</p>}
        </div>
      )}
    </details>
  )
}

export const GraphInspectorDrawer = memo(function GraphInspectorDrawer({
  selectedNode,
  topology,
  memoryById,
  importance,
  onClose,
  onJumpToNode,
  ...actions
}: GraphInspectorDrawerProps) {
  const { t } = useTranslation()
  const [memoryPage, setMemoryPage] = useState(0)
  const sources = useMemo(() => selectedNode.memoryIds
    .map((id) => memoryById.get(id))
    .filter((memory): memory is MemoryItem => Boolean(memory)), [selectedNode.memoryIds, memoryById])
  const neighbors = useMemo(() => topology.edges.flatMap((edge) => {
    const id = edge.sourceId === selectedNode.id ? edge.targetId : edge.targetId === selectedNode.id ? edge.sourceId : null
    const node = id ? topology.nodeById.get(id) : undefined
    return node?.kind === 'keyword' ? [{ node, kind: edge.kind, count: edge.weight }] : []
  }), [selectedNode.id, topology])
  const pageSize = 12
  const pageCount = Math.max(1, Math.ceil(sources.length / pageSize))
  const page = Math.min(memoryPage, pageCount - 1)
  const visibleSources = sources.slice(page * pageSize, (page + 1) * pageSize)

  return (
    <aside className="flex h-full w-80 max-w-[45%] shrink-0 flex-col border-l border-gray-200 bg-white dark:border-gray-700 dark:bg-neutral-800">
      <div className="flex items-start justify-between gap-2 border-b border-gray-200 px-4 py-3 dark:border-gray-700">
        <div className="min-w-0">
          <h2 className="break-words text-base font-semibold text-gray-900 dark:text-gray-100">{selectedNode.label}</h2>
          <p className="mt-1 text-xs text-gray-500 dark:text-gray-400">{selectedNode.domainItem.name}</p>
        </div>
        <button type="button" onClick={onClose} className="rounded-md p-1 text-gray-500 hover:bg-gray-100 dark:text-gray-300 dark:hover:bg-gray-700" aria-label={t('common.close')}><X size={16} /></button>
      </div>
      <div className="min-h-0 flex-1 space-y-5 overflow-y-auto overscroll-contain p-4">
        {selectedNode.kind === 'keyword' ? (
          <div className="space-y-1 text-xs text-gray-600 dark:text-gray-300">
            <p>{t('memory.brainGraph.keywordCoverage', { count: sources.length })}</p>
            <p>{t('memory.brainGraph.keywordFrequency', { count: selectedNode.frequency })}</p>
          </div>
        ) : <p className="text-xs leading-relaxed text-gray-600 dark:text-gray-300">{selectedNode.kind === 'memory'
          ? t('memory.brainGraph.sampleMemoryDescription')
          : selectedNode.domainItem.description || t('memory.brainGraph.lobeDefaultDesc')}</p>}
        {selectedNode.kind === 'keyword' && (
          <section className="space-y-2">
            <h3 className="text-xs font-semibold text-gray-800 dark:text-gray-100">{t('memory.brainGraph.relatedKeywords')}</h3>
            {neighbors.length ? neighbors.map(({ node, kind, count }) => (
              <button key={node.id} type="button" onClick={() => onJumpToNode(node)} className="flex w-full items-center justify-between gap-2 rounded-md px-2 py-1.5 text-left text-xs text-gray-700 hover:bg-gray-100 dark:text-gray-200 dark:hover:bg-gray-700">
                <span>{node.label}<span className="ml-1 text-[11px] text-gray-500 dark:text-gray-400">{node.domainItem.name}</span></span>
                <span className="shrink-0 text-[11px] text-gray-500 dark:text-gray-400">{kind === 'shared-keyword'
                  ? t('memory.brainGraph.sharedKeywordLegend')
                  : t('memory.brainGraph.cooccurrenceCount', { count })}</span>
              </button>
            )) : <p className="text-xs text-gray-500 dark:text-gray-400">{t('memory.brainGraph.noRelatedKeywords')}</p>}
          </section>
        )}
        <section>
          <h3 className="mb-2 text-xs font-semibold text-gray-800 dark:text-gray-100">{t('memory.brainGraph.sourceMemories', { count: sources.length })}</h3>
          {visibleSources.map((memory) => <SourceMemory key={memory.id} memory={memory} importance={importance[memory.id]} {...actions} />)}
          {pageCount > 1 && (
            <div className="flex items-center justify-between gap-2 pt-3 text-xs text-gray-600 dark:text-gray-300">
              <button type="button" disabled={page === 0} onClick={() => setMemoryPage(page - 1)} className="rounded-md border border-gray-200 px-2 py-1 disabled:opacity-40 dark:border-gray-700">{t('memory.brainGraph.previousPage')}</button>
              <span aria-live="polite">{page + 1} / {pageCount}</span>
              <button type="button" disabled={page === pageCount - 1} onClick={() => setMemoryPage(page + 1)} className="rounded-md border border-gray-200 px-2 py-1 disabled:opacity-40 dark:border-gray-700">{t('memory.brainGraph.nextPage')}</button>
            </div>
          )}
        </section>
      </div>
    </aside>
  )
})
