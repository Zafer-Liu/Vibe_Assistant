import { memo } from 'react'
import { useTranslation } from 'react-i18next'
import {
  ArrowLeft, Search, ZoomIn, ZoomOut, RotateCcw,
  Network, RefreshCw, X,
} from 'lucide-react'
import type { EffectiveLobe } from './types'
import { getDomainSurface } from './domainSurface'

interface GraphToolbarProps {
  onBack?: () => void
  effectiveLobes: EffectiveLobe[]
  neuronCount: number
  keywordCount: number
  sampledMemoryCount: number
  cloudCount: number
  totalCloudCount: number
  crossSynapseCount: number
  searchQuery: string
  onSearchChange: (q: string) => void
  showCrossSynapses: boolean
  onToggleCrossSynapses: () => void
  onZoomIn: () => void
  onZoomOut: () => void
  onResetView: () => void
  refreshing: boolean
  onRefresh: () => void
  activeLobeId: string | null
  onSelectLobeFilter: (domainId: string | null) => void
}

export const GraphToolbar = memo(function GraphToolbar({
  onBack,
  effectiveLobes,
  neuronCount,
  keywordCount,
  sampledMemoryCount,
  cloudCount,
  totalCloudCount,
  crossSynapseCount,
  searchQuery,
  onSearchChange,
  showCrossSynapses,
  onToggleCrossSynapses,
  onZoomIn,
  onZoomOut,
  onResetView,
  refreshing,
  onRefresh,
  activeLobeId,
  onSelectLobeFilter,
}: GraphToolbarProps) {
  const { t } = useTranslation()

  return (
    <>
      {/* Top Header & View Controls */}
      <div className="fluent-page-header flex shrink-0 flex-wrap items-center gap-3 rounded-xl">
        {onBack && (
          <button
            type="button"
            onClick={onBack}
            className="inline-flex items-center gap-1 rounded-lg border border-gray-200 bg-white px-2.5 py-1.5 text-xs font-medium text-gray-700 transition hover:bg-gray-50 dark:border-gray-700 dark:bg-gray-800 dark:text-gray-200 dark:hover:bg-gray-700"
          >
            <ArrowLeft size={14} />
            {t('common.back')}
          </button>
        )}
        <div className="rounded-md bg-blue-500/10 p-2 text-blue-700 dark:text-sky-300">
          <Network size={20} />
        </div>
        <div className="min-w-0">
          <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
            <h1 className="font-semibold text-gray-800 dark:text-gray-100">
              {t('memory.brainGraph.title')}
            </h1>
            <span className="text-xs text-gray-600 dark:text-gray-300">
              {t('memory.brainGraph.statsSummary', {
                shown: cloudCount,
                total: totalCloudCount,
                keywords: keywordCount,
                memories: sampledMemoryCount,
                synapses: crossSynapseCount,
              })}
            </span>
          </div>
          <p className="text-xs text-gray-500 dark:text-gray-400">
            {t('memory.brainGraph.subtitle')}
          </p>
        </div>

        <div className="ml-auto flex flex-wrap items-center gap-2">
          <div className="relative">
            <Search size={13} className="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-gray-400" />
            <input
              type="search"
              value={searchQuery}
              onChange={(e) => onSearchChange(e.target.value)}
              placeholder={t('memory.brainGraph.searchPlaceholder')}
              aria-label={t('memory.brainGraph.searchPlaceholder')}
              className="w-48 rounded-lg border border-gray-200 bg-white py-1.5 pl-8 pr-7 text-xs text-gray-800 placeholder-gray-400 focus:border-blue-500 focus:outline-none dark:border-gray-700 dark:bg-gray-800 dark:text-gray-100"
            />
            {searchQuery && (
              <button
                type="button"
                onClick={() => onSearchChange('')}
                className="absolute right-2 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-200"
                aria-label={t('common.clear')}
              >
                <X size={12} />
              </button>
            )}
          </div>

          <button
            type="button"
            onClick={onToggleCrossSynapses}
            className={`inline-flex items-center gap-1 rounded-lg border px-2.5 py-1.5 text-xs font-medium transition ${
              showCrossSynapses
                ? 'border-blue-500/50 bg-blue-500/10 text-blue-700 dark:text-blue-300'
                : 'border-gray-200 bg-white text-gray-600 dark:border-gray-700 dark:bg-gray-800 dark:text-gray-300'
            }`}
            title={t('memory.brainGraph.toggleSynapses')}
            aria-pressed={showCrossSynapses}
          >
            <Network size={13} />
            {t('memory.brainGraph.synapsesToggleLabel')}
          </button>

          <div className="inline-flex items-center rounded-lg border border-gray-200 bg-white dark:border-gray-700 dark:bg-gray-800">
            <button
              type="button"
              onClick={onZoomIn}
              className="p-1.5 text-gray-600 hover:text-blue-600 dark:text-gray-300 dark:hover:text-blue-300"
              title={t('memory.brainGraph.zoomIn')}
              aria-label={t('memory.brainGraph.zoomIn')}
            >
              <ZoomIn size={14} />
            </button>
            <button
              type="button"
              onClick={onZoomOut}
              className="p-1.5 text-gray-600 hover:text-blue-600 dark:text-gray-300 dark:hover:text-blue-300"
              title={t('memory.brainGraph.zoomOut')}
              aria-label={t('memory.brainGraph.zoomOut')}
            >
              <ZoomOut size={14} />
            </button>
            <button
              type="button"
              onClick={onResetView}
              className="p-1.5 text-gray-600 hover:text-blue-600 dark:text-gray-300 dark:hover:text-blue-300"
              title={t('memory.brainGraph.resetView')}
              aria-label={t('memory.brainGraph.resetView')}
            >
              <RotateCcw size={14} />
            </button>
          </div>

          <button
            type="button"
            onClick={onRefresh}
            disabled={refreshing}
            className="inline-flex items-center gap-1 rounded-lg border border-gray-200 bg-white px-2.5 py-1.5 text-xs font-medium text-gray-700 hover:bg-gray-50 disabled:opacity-50 dark:border-gray-700 dark:bg-gray-800 dark:text-gray-200 dark:hover:bg-gray-700"
          >
            <RefreshCw size={13} className={refreshing ? 'animate-spin' : ''} />
            {t('common.refresh')}
          </button>
        </div>
      </div>

      <div className="flex max-h-28 shrink-0 flex-wrap items-center gap-1.5 overflow-y-auto rounded-lg border border-gray-200 bg-white px-3 py-2 dark:border-gray-700 dark:bg-gray-800">
        <span className="mr-1 text-xs font-medium text-gray-500 dark:text-gray-400">
          {t('memory.brainGraph.lobeFilterLabel')}
        </span>
        <button
          type="button"
          onClick={() => onSelectLobeFilter(null)}
          aria-pressed={activeLobeId === null}
          className={`inline-flex items-center gap-1.5 rounded-full px-2.5 py-1 text-xs font-medium transition ${
            activeLobeId === null
              ? 'bg-blue-600 text-white shadow-sm'
              : 'bg-gray-100 text-gray-700 hover:bg-gray-200 dark:bg-gray-700 dark:text-gray-200 dark:hover:bg-gray-600'
          }`}
        >
          {t('memory.brainGraph.allLobes')}
          <span className="opacity-80">({neuronCount})</span>
        </button>

        {effectiveLobes.map(({ item }) => {
          const isSelected = activeLobeId === item.id
          return (
            <button
              key={item.id}
              type="button"
              onClick={() => onSelectLobeFilter(item.id)}
              aria-pressed={isSelected}
              className={`inline-flex items-center gap-1.5 rounded-full px-2.5 py-1 text-xs font-medium transition ${
                isSelected
                  ? 'bg-gray-900 text-white ring-2 ring-blue-400 dark:bg-white dark:text-gray-900'
                  : 'bg-gray-100 text-gray-700 hover:bg-gray-200 dark:bg-gray-700/80 dark:text-gray-200 dark:hover:bg-gray-600'
              }`}
              title={item.description || item.name}
            >
              <span
                className="h-2 w-2 rounded-full"
                style={{ backgroundColor: getDomainSurface(item.id).marker }}
              />
              <span>{item.name}</span>
              <span className="opacity-75">({item.memory_count})</span>
            </button>
          )
        })}

        <span className="ml-auto text-xs text-gray-500 dark:text-gray-400">
          {t('memory.brainGraph.selectionHint')}
        </span>
      </div>
    </>
  )
})
