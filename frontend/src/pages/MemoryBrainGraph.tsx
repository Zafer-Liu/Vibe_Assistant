import { memo, useCallback, useDeferredValue, useEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useMemoryFields } from '../store/memoryStore'
import { useTheme } from '../theme'
import { buildGraphTopology } from '../features/memory-graph/buildGraphTopology'
import { useKeywordCorpus } from '../features/memory-graph/useKeywordCorpus'
import { GraphCanvasViewport } from '../features/memory-graph/GraphCanvasViewport'
import { GraphInspectorDrawer } from '../features/memory-graph/GraphInspectorDrawer'
import { GraphToolbar } from '../features/memory-graph/GraphToolbar'
import { UNCATEGORIZED_ID, UNCATEGORIZED_PALETTE, type EffectiveLobe, type GraphViewportController } from '../features/memory-graph/types'

export const MemoryBrainGraph = memo(function MemoryBrainGraph({
  onBack,
  active = true,
}: {
  onBack?: () => void
  active?: boolean
}) {
  const { t } = useTranslation()
  const { theme } = useTheme()
  const {
    memories, memoryDomains, importance, memoryCacheReady,
    listMemories, loadMemoryDomains, updateMemory, deleteMemory, setMemoryPinned, refreshImportance,
  } = useMemoryFields(
    'memories', 'memoryDomains', 'importance', 'memoryCacheReady',
    'listMemories', 'loadMemoryDomains', 'updateMemory', 'deleteMemory', 'setMemoryPinned', 'refreshImportance',
  )
  const [searchQuery, setSearchQuery] = useState('')
  const deferredSearch = useDeferredValue(searchQuery)
  const [activeLobeId, setActiveLobeId] = useState<string | null>(null)
  const [selectedNodeId, setSelectedNodeId] = useState<string | null>(null)
  const [showCrossSynapses, setShowCrossSynapses] = useState(true)
  const [refreshing, setRefreshing] = useState(false)
  const [keywordRevision, setKeywordRevision] = useState(0)
  const viewportControllerRef = useRef<GraphViewportController | null>(null)

  useEffect(() => {
    if (!active) return
    if (!memoryCacheReady) void listMemories()
    void loadMemoryDomains()
  }, [active, memoryCacheReady, listMemories, loadMemoryDomains])

  const handleRefresh = useCallback(async () => {
    if (refreshing) return
    setRefreshing(true)
    try {
      await Promise.allSettled([loadMemoryDomains(), listMemories(true)])
    } finally {
      setRefreshing(false)
      setKeywordRevision((value) => value + 1)
    }
  }, [refreshing, loadMemoryDomains, listMemories])

  const { corpus, loading: keywordLoading, failed: keywordFailed } = useKeywordCorpus(memories, active, keywordRevision)
  const effectiveLobes = useMemo<EffectiveLobe[]>(() => {
    const byId = new Map(memoryDomains.map((domain) => [domain.id, domain]))
    for (const memory of memories) {
      for (const domain of memory.domains ?? []) {
        if (!byId.has(domain.id)) byId.set(domain.id, domain)
      }
    }
    if (corpus.domains.has(UNCATEGORIZED_ID)) {
      byId.set(UNCATEGORIZED_ID, {
        id: UNCATEGORIZED_ID,
        name: t('memory.brainGraph.uncategorized'),
        description: t('memory.brainGraph.uncategorizedDesc'),
        memory_count: 0,
      })
    }
    return [...byId.values()]
      .filter((domain) => corpus.domains.has(domain.id))
      .map((domain, index) => ({
        item: { ...domain, memory_count: corpus.domains.get(domain.id)!.memoryIds.size },
        palette: UNCATEGORIZED_PALETTE,
        slotIndex: index,
      }))
  }, [memoryDomains, memories, corpus, t])

  useEffect(() => {
    if (activeLobeId && !effectiveLobes.some((domain) => domain.item.id === activeLobeId)) {
      setActiveLobeId(null)
    }
  }, [activeLobeId, effectiveLobes])

  const topology = useMemo(
    () => buildGraphTopology(corpus, effectiveLobes, activeLobeId, deferredSearch),
    [corpus, effectiveLobes, activeLobeId, deferredSearch],
  )
  const adjacencyMap = useMemo(() => {
    const map = new Map<string, Set<string>>()
    if (!showCrossSynapses) return map
    for (const edge of topology.edges) {
      for (const [source, target] of [[edge.sourceId, edge.targetId], [edge.targetId, edge.sourceId]]) {
        let neighbors = map.get(source)
        if (!neighbors) {
          neighbors = new Set()
          map.set(source, neighbors)
        }
        neighbors.add(target)
      }
    }
    return map
  }, [topology.edges, showCrossSynapses])

  useEffect(() => {
    if (selectedNodeId && !topology.nodeById.has(selectedNodeId)) setSelectedNodeId(null)
  }, [topology.nodeById, selectedNodeId])

  const handleSelectLobeFilter = useCallback((domainId: string | null) => {
    setActiveLobeId((current) => current === domainId ? null : domainId)
    setSelectedNodeId(null)
  }, [])
  const handleUpdateMemory = useCallback(async (id: string, content: string) => {
    await updateMemory(id, content)
  }, [updateMemory])
  const handleDeleteMemory = useCallback(async (id: string) => {
    await deleteMemory(id)
  }, [deleteMemory])
  const handleTogglePin = useCallback(async (id: string, pinned: boolean) => {
    await setMemoryPinned(id, pinned)
    await refreshImportance()
  }, [setMemoryPinned, refreshImportance])
  const selectedNode = selectedNodeId ? topology.nodeById.get(selectedNodeId) ?? null : null

  return (
    <div className="flex h-full min-h-0 flex-1 flex-col gap-3 overflow-hidden p-4">
      <GraphToolbar
        onBack={onBack}
        effectiveLobes={effectiveLobes}
        neuronCount={corpus.memoryById.size}
        keywordCount={topology.keywordCount}
        sampledMemoryCount={topology.sampledMemoryCount}
        cloudCount={topology.cloudCount}
        totalCloudCount={topology.totalCloudCount}
        crossSynapseCount={topology.edges.length}
        searchQuery={searchQuery}
        onSearchChange={setSearchQuery}
        showCrossSynapses={showCrossSynapses}
        onToggleCrossSynapses={() => setShowCrossSynapses((value) => !value)}
        onZoomIn={() => viewportControllerRef.current?.zoomIn()}
        onZoomOut={() => viewportControllerRef.current?.zoomOut()}
        onResetView={() => viewportControllerRef.current?.resetCamera()}
        refreshing={refreshing || keywordLoading}
        onRefresh={() => { void handleRefresh() }}
        activeLobeId={activeLobeId}
        onSelectLobeFilter={handleSelectLobeFilter}
      />
      {keywordFailed && (
        <p role="alert" className="text-xs text-red-600 dark:text-red-400">
          {t('memory.brainGraph.keywordLoadFailed')}
          <button type="button" onClick={() => setKeywordRevision((value) => value + 1)} className="ml-2 underline underline-offset-2">{t('common.refresh')}</button>
        </p>
      )}
      <div className="relative flex min-h-0 flex-1 overflow-hidden rounded-lg border border-gray-200 bg-white dark:border-gray-700 dark:bg-neutral-800" aria-busy={keywordLoading || deferredSearch !== searchQuery}>
        <GraphCanvasViewport
          active={active}
          isDark={theme === 'dark'}
          topology={topology}
          selectedNodeId={selectedNodeId}
          adjacencyMap={adjacencyMap}
          showCrossSynapses={showCrossSynapses}
          searchActive={deferredSearch.trim().length > 0}
          loading={keywordLoading}
          onSelectNode={setSelectedNodeId}
          controllerRef={viewportControllerRef}
        />
        {selectedNode && (
          <GraphInspectorDrawer
            key={selectedNode.id}
            selectedNode={selectedNode}
            topology={topology}
            memoryById={corpus.memoryById}
            importance={importance}
            onClose={() => setSelectedNodeId(null)}
            onJumpToNode={(node) => {
              setSelectedNodeId(node.id)
              viewportControllerRef.current?.focusNodeById(node.id, 1.15)
            }}
            onTogglePin={handleTogglePin}
            onUpdateMemory={handleUpdateMemory}
            onDeleteMemory={handleDeleteMemory}
          />
        )}
      </div>
    </div>
  )
})
