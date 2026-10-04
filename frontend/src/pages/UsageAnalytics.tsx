import { memo, useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { listen } from '@tauri-apps/api/event'
import {
  Activity, BarChart3, CalendarDays, ChevronDown, Database, FileClock,
  Calculator, Gauge, Grid3X3, RefreshCw, SlidersHorizontal, Sparkles, Zap,
} from 'lucide-react'
import { useMemoryStore } from '../store/memoryStore'
import { useVisiblePolling } from '../hooks/useVisiblePolling'
import { useTheme } from '../theme'
import { ErrorRecovery } from '../components/ErrorRecovery'
import type { TelemetryUsageAnalytics, TelemetryUsageBucket, TelemetryUsageCostGroup, TelemetryUsageHeatCell, TelemetryUsageRecord } from '../types/memory'
import { intlLocale, getLanguage } from '../i18n'

type RangeKey = 'today' | '7d' | '30d' | 'all'
type TabKey = 'trend' | 'records'

interface PricingRule {
  input: number
  output: number
  cache: number
}

const PRICING_STORAGE_KEY = 'usage-pricing-v1'

function pricingKey(group: Pick<TelemetryUsageCostGroup, 'source' | 'model'>) {
  return `${group.source}\u0000${group.model ?? ''}`
}

function loadPricingRules(): Record<string, PricingRule> {
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(PRICING_STORAGE_KEY) ?? '{}')
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) return {}
    const rules: Record<string, PricingRule> = {}
    for (const [key, raw] of Object.entries(parsed as Record<string, unknown>)) {
      if (!raw || typeof raw !== 'object' || Array.isArray(raw)) continue
      const value = raw as Record<string, unknown>
      const input = Number(value.input)
      const output = Number(value.output)
      const cache = Number(value.cache)
      if ([input, output, cache].every(rate => Number.isFinite(rate) && rate >= 0)) {
        rules[key] = { input, output, cache }
      }
    }
    return rules
  } catch {
    return {}
  }
}

function groupCost(group: TelemetryUsageCostGroup, rule?: PricingRule): number | null {
  if (!rule || (rule.input === 0 && rule.output === 0 && rule.cache === 0)) return null
  const uncachedInput = Math.max(0, group.input_tokens - group.cached_tokens)
  return (uncachedInput * rule.input + group.output_tokens * rule.output + group.cached_tokens * rule.cache) / 1_000_000
}

const RANGE_LABEL_KEYS: Record<RangeKey, string> = {
  today: 'memory.usage.rangeToday',
  '7d': 'memory.usage.range7d',
  '30d': 'memory.usage.range30d',
  all: 'memory.usage.rangeAll',
}

function amount(value: number) { return new Intl.NumberFormat(intlLocale()).format(value) }

function compactAmount(value: number) {
  const lang = getLanguage()
  if (lang === 'en') {
    if (value >= 1_000_000_000) return `${(value / 1_000_000_000).toFixed(2)}B`
    if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(value >= 10_000_000 ? 1 : 2)}M`
    return amount(value)
  }
  if (value >= 100_000_000) return `${(value / 100_000_000).toFixed(2)} 亿`
  if (value >= 10_000) return `${(value / 10_000).toFixed(value >= 1_000_000 ? 1 : 0)} 万`
  return amount(value)
}

function sourceLabel(source: string) {
  return ({ codex: 'Codex', claude: 'Claude Code', qoder: 'Qoder', workbuddy: 'WorkBuddy', minimax: 'MiniMax Code', kimi: 'Kimi', copilot: 'GitHub Copilot', gemini: 'Gemini CLI', opencode: 'OpenCode', openclaw: 'OpenClaw', pi: 'Pi', grokbuild: 'Grok Build' } as Record<string, string>)[source] ?? source
}

function parseTime(value: string) {
  return new Date(value.trim().replace(/(\.\d{3})\d+(?=(Z|[+-]\d{2}:\d{2})$)/, '$1'))
}

function displayTime(value: string) {
  const date = parseTime(value)
  return Number.isNaN(date.getTime()) ? value : new Intl.DateTimeFormat(intlLocale(), { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', hour12: false }).format(date)
}

function tokenTotal(event: TelemetryUsageRecord) { return event.input_tokens + event.output_tokens }

function boundsFor(range: RangeKey) {
  if (range === 'all') return { bucket: 'day' as const }
  const now = new Date()
  const tomorrow = new Date(now.getFullYear(), now.getMonth(), now.getDate() + 1)
  const start = new Date(now.getFullYear(), now.getMonth(), now.getDate() - (range === 'today' ? 0 : range === '7d' ? 6 : 29))
  return { startAt: start.toISOString(), endAt: tomorrow.toISOString(), bucket: range === 'today' ? 'hour' as const : 'day' as const }
}

function bucketLabel(bucket: TelemetryUsageBucket, range: RangeKey) {
  if (range === 'today') return bucket.label.slice(11, 16)
  return range === 'all' && bucket.label.length >= 10 ? bucket.label.slice(0, 7).replace('-', '/') : bucket.label.slice(5).replace('-', '/')
}

function UsageTrend({ buckets, range }: { buckets: TelemetryUsageBucket[]; range: RangeKey }) {
  const { t } = useTranslation()
  const [activeIndex, setActiveIndex] = useState<number | null>(null)
  const points = buckets
  const primaryMax = Math.max(1, ...points.flatMap(point => [point.input_tokens, point.cached_tokens]))
  const outputMax = Math.max(1, ...points.map(point => point.output_tokens))
  const primaryTicks = [primaryMax, Math.round(primaryMax * 0.75), Math.round(primaryMax * 0.5), Math.round(primaryMax * 0.25), 0]
  const outputTicks = [outputMax, Math.round(outputMax * 0.75), Math.round(outputMax * 0.5), Math.round(outputMax * 0.25), 0]
  const pointX = (index: number) => points.length <= 1 ? 50 : (index / (points.length - 1)) * 100
  const pointY = (value: number, scale: number = primaryMax) => 92 - (value / scale) * 82
  const coordinates = (field: 'input_tokens' | 'output_tokens' | 'cached_tokens') => points.map((point, index) => {
    return `${pointX(index)},${pointY(point[field], field === 'output_tokens' ? outputMax : primaryMax)}`
  }).join(' ')
  const fill = points.length ? `0,94 ${coordinates('input_tokens')} 100,94` : ''
  const labelIndexes = [...new Set([0, Math.round((points.length - 1) * .25), Math.round((points.length - 1) * .5), Math.round((points.length - 1) * .75), Math.max(0, points.length - 1)])]
  const activePoint = activeIndex === null ? null : points[activeIndex]

  function selectNearestPoint(clientX: number, target: SVGSVGElement) {
    const bounds = target.getBoundingClientRect()
    const ratio = Math.max(0, Math.min(1, (clientX - bounds.left) / Math.max(1, bounds.width)))
    setActiveIndex(Math.round(ratio * Math.max(0, points.length - 1)))
  }

  if (!points.length) return <div className="mt-5 flex h-64 items-center justify-center rounded-xl border border-dashed border-slate-200 bg-slate-50/70 text-sm text-slate-500 dark:border-slate-800 dark:bg-slate-950/40 dark:text-slate-400">{t('memory.usage.chartEmpty')}</div>

  return <div className="mt-5 grid grid-cols-[3.8rem_minmax(0,1fr)_3.8rem] gap-2" aria-label={t('memory.usage.chartAria')}>
    <div className="flex h-56 flex-col justify-between pb-7 pt-1 text-right font-mono text-[11px] tabular-nums text-slate-500 dark:text-slate-400" aria-label={t('memory.usage.yAxisAria')}>
      {primaryTicks.map((tick, index) => <span key={`${tick}-${index}`}>{compactAmount(tick)}</span>)}
    </div>
    <div className="min-w-0">
      <div className="relative h-56 overflow-hidden rounded-xl border border-slate-100 bg-slate-50/70 px-2 pt-2 dark:border-slate-800 dark:bg-slate-950/40">
        <div className="pointer-events-none absolute inset-x-2 top-2 bottom-7 grid grid-rows-4 border-b border-dashed border-slate-200/80 dark:border-slate-800">
          {[0, 1, 2, 3].map(row => <div key={row} className="border-t border-dashed border-slate-200/80 dark:border-slate-800" />)}
        </div>
        <svg viewBox="0 0 100 100" preserveAspectRatio="none" className="relative z-10 h-[calc(100%-1.25rem)] w-full cursor-crosshair overflow-visible outline-none focus:outline-none" role="group" tabIndex={0} aria-label={t('memory.usage.chartDescAria')} onMouseMove={event => selectNearestPoint(event.clientX, event.currentTarget)} onMouseLeave={() => setActiveIndex(null)} onFocus={() => setActiveIndex(current => current ?? 0)} onKeyDown={event => {
          if (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight') return
          event.preventDefault()
          setActiveIndex(current => Math.max(0, Math.min(points.length - 1, (current ?? 0) + (event.key === 'ArrowRight' ? 1 : -1))))
        }}>
          <defs><linearGradient id="usage-input-fill" x1="0" x2="0" y1="0" y2="1"><stop offset="0%" stopColor="#3b82f6" stopOpacity="0.24" /><stop offset="100%" stopColor="#3b82f6" stopOpacity="0" /></linearGradient></defs>
          <polygon points={fill} fill="url(#usage-input-fill)" />
          <polyline points={coordinates('input_tokens')} fill="none" stroke="#3b82f6" strokeWidth="1.5" vectorEffect="non-scaling-stroke" />
          <polyline points={coordinates('output_tokens')} fill="none" stroke="#8b5cf6" strokeWidth="1.5" vectorEffect="non-scaling-stroke" />
          <polyline points={coordinates('cached_tokens')} fill="none" stroke="#10b981" strokeWidth="1.25" vectorEffect="non-scaling-stroke" />
          {activePoint && activeIndex !== null && <>
            <line x1={pointX(activeIndex)} x2={pointX(activeIndex)} y1="4" y2="92" stroke="#94a3b8" strokeDasharray="2 3" strokeWidth="0.7" vectorEffect="non-scaling-stroke" />
          </>}
          <rect x="0" y="0" width="100" height="100" fill="transparent" />
        </svg>
        {activePoint && activeIndex !== null && <div className="pointer-events-none absolute inset-x-2 top-2 z-20 h-[calc(100%-1.25rem)]" aria-hidden="true">
          <TrendMarker x={pointX(activeIndex)} y={pointY(activePoint.input_tokens)} tone="bg-blue-500" />
          <TrendMarker x={pointX(activeIndex)} y={pointY(activePoint.output_tokens, outputMax)} tone="bg-violet-500" />
          <TrendMarker x={pointX(activeIndex)} y={pointY(activePoint.cached_tokens)} tone="bg-emerald-500" />
        </div>}
        {activePoint && activeIndex !== null && <div className={`pointer-events-none absolute top-3 z-20 w-48 rounded-lg border border-slate-200/90 bg-white/95 px-3 py-2.5 shadow-lg shadow-slate-900/10 backdrop-blur-sm dark:border-slate-700 dark:bg-slate-900/95 ${pointX(activeIndex) > 72 ? '-translate-x-full -ml-2' : 'ml-2'}`} style={{ left: `${pointX(activeIndex)}%` }} role="status" aria-live="polite">
          <p className="mb-2 font-mono text-xs font-semibold tabular-nums text-slate-700 dark:text-slate-200">{range === 'today' ? activePoint.label : activePoint.label.replaceAll('-', '/')}</p>
          <TooltipMetric label={t('memory.usage.tooltipInput')} value={activePoint.input_tokens} tone="bg-blue-500" />
          <TooltipMetric label={t('memory.usage.tooltipOutput')} value={activePoint.output_tokens} tone="bg-violet-500" />
          <TooltipMetric label={t('memory.usage.tooltipCache')} value={activePoint.cached_tokens} tone="bg-emerald-500" />
        </div>}
        <div className="absolute inset-x-3 bottom-1.5 flex justify-between font-mono text-[11px] tabular-nums text-slate-500 dark:text-slate-400" aria-label={t('memory.usage.xAxisAria')}>
          {labelIndexes.map(index => <span key={`${points[index].label}-${index}`}>{bucketLabel(points[index], range)}</span>)}
        </div>
      </div>
    </div>
    <div className="flex h-56 flex-col justify-between pb-7 pt-1 font-mono text-[11px] tabular-nums text-violet-500/85 dark:text-violet-300/85" aria-label={t('memory.usage.yAxis2Aria')}>
      {outputTicks.map((tick, index) => <span key={`${tick}-${index}`}>{compactAmount(tick)}</span>)}
    </div>
  </div>
}

function UsageHeatmap({ cells, loading }: { cells: TelemetryUsageHeatCell[]; loading: boolean }) {
  const { t } = useTranslation()
  const values = new Map(cells.map(cell => [`${cell.weekday}:${cell.hour}`, cell]))
  const maxTokens = Math.max(0, ...cells.map(cell => cell.input_tokens + cell.output_tokens))
  const dayOrder = [1, 2, 3, 4, 5, 6, 0]
  const dayLabel = (weekday: number) => new Intl.DateTimeFormat(intlLocale(), { weekday: 'short' })
    .format(new Date(2024, 0, 7 + weekday))

  return <section className="mt-7 rounded-2xl border border-slate-200 bg-white p-5 shadow-[0_1px_2px_rgba(15,23,42,0.03)] dark:border-slate-800 dark:bg-slate-900 sm:p-7" aria-labelledby="usage-heatmap-title">
    <div className="flex items-start gap-3">
      <span className="grid h-10 w-10 shrink-0 place-items-center rounded-xl bg-emerald-50 text-emerald-600 dark:bg-emerald-500/15 dark:text-emerald-300"><Grid3X3 size={20} /></span>
      <div>
        <h2 id="usage-heatmap-title" className="font-semibold text-slate-900 dark:text-slate-100">{t('memory.usage.heatmapTitle')}</h2>
        <p className="mt-1 text-sm text-slate-500 dark:text-slate-400">{t('memory.usage.heatmapDesc')}</p>
      </div>
    </div>
    {loading ? <div className="mt-5 h-44 animate-pulse rounded-xl bg-slate-100 dark:bg-slate-800" /> : maxTokens === 0 ? <div className="mt-4 rounded-lg border border-dashed border-slate-200 px-4 py-8 text-center text-sm text-slate-500 dark:border-slate-800 dark:text-slate-400">{t('memory.usage.heatmapEmpty')}</div> : (
      <div className="mt-5 overflow-x-auto pb-1">
        <div className="grid min-w-[690px] grid-cols-[3.5rem_repeat(24,minmax(1.25rem,1fr))] gap-1" role="img" aria-label={t('memory.usage.heatmapDesc')}>
          <span />
          {Array.from({ length: 24 }, (_, hour) => <span key={hour} className="text-center font-mono text-[9px] tabular-nums text-slate-400">{hour % 3 === 0 ? hour.toString().padStart(2, '0') : ''}</span>)}
          {dayOrder.flatMap(weekday => {
            const label = dayLabel(weekday)
            return [
              <span key={`label-${weekday}`} className="flex items-center text-xs text-slate-500 dark:text-slate-400">{label}</span>,
              ...Array.from({ length: 24 }, (_, hour) => {
                const cell = values.get(`${weekday}:${hour}`)
                const tokens = (cell?.input_tokens ?? 0) + (cell?.output_tokens ?? 0)
                const intensity = tokens === 0 ? 0.04 : 0.16 + 0.76 * Math.sqrt(tokens / maxTokens)
                const title = t('memory.usage.heatmapCell', { day: label, hour, tokens: amount(tokens), records: cell?.record_count ?? 0 })
                return <span key={`${weekday}-${hour}`} title={title} aria-label={title} className="aspect-square min-h-5 rounded-[4px] border border-emerald-700/10" style={{ backgroundColor: `rgba(64, 196, 99, ${intensity})` }} />
              }),
            ]
          })}
        </div>
      </div>
    )}
  </section>
}

type CalendarMode = 'daily' | 'weekly' | 'cumulative'

const CALENDAR_MODE_LABEL_KEYS: Record<CalendarMode, string> = {
  daily: 'memory.usage.calendarModeDaily',
  weekly: 'memory.usage.calendarModeWeekly',
  cumulative: 'memory.usage.calendarModeCumulative',
}

function calendarDateKey(date: Date) {
  return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, '0')}-${String(date.getDate()).padStart(2, '0')}`
}

interface CalendarDay {
  key: string
  date: Date
  input: number
  output: number
  tokens: number
  records: number
  future: boolean
}

interface CalendarWeek {
  days: CalendarDay[]
  tokens: number
  monthLabel: string
}

// GitHub 风格的滚动一年日历图：列 = 自然周（周一起），行 = 星期几，
// 颜色分级用非零值的四分位数，避免个别极端日把其余日子压成最浅色。
function UsageCalendar({ buckets, loading }: { buckets: TelemetryUsageBucket[]; loading: boolean }) {
  const { t } = useTranslation()
  const { theme } = useTheme()
  const [mode, setMode] = useState<CalendarMode>('daily')
  const palette = theme === 'dark'
    ? ['#161b22', '#0e4429', '#006d32', '#26a641', '#39d353']
    : ['#ebedf0', '#9be9a8', '#40c463', '#30a14e', '#216e39']

  const { weeks, totalTokens, activeDays, cumulativeByDay } = useMemo(() => {
    const byDay = new Map(buckets.map(bucket => [bucket.label, bucket]))
    const end = new Date()
    const start = new Date(end.getFullYear(), end.getMonth(), end.getDate() - 364)
    const startMonday = new Date(start.getFullYear(), start.getMonth(), start.getDate() - ((start.getDay() + 6) % 7))
    const monthFormat = new Intl.DateTimeFormat(intlLocale(), { month: 'short' })
    const built: CalendarWeek[] = []
    const cumulativeByDay = new Map<string, number>()
    let running = 0
    let totalTokens = 0
    let activeDays = 0
    let prevMonth = -1
    let lastLabelColumn = -3
    for (let cursor = startMonday; cursor <= end; cursor = new Date(cursor.getFullYear(), cursor.getMonth(), cursor.getDate() + 7)) {
      const days: CalendarDay[] = []
      let weekTokens = 0
      for (let offset = 0; offset < 7; offset++) {
        const date = new Date(cursor.getFullYear(), cursor.getMonth(), cursor.getDate() + offset)
        const future = date > end
        const bucket = byDay.get(calendarDateKey(date))
        const tokens = future || !bucket ? 0 : bucket.input_tokens + bucket.output_tokens
        if (!future) {
          running += tokens
          totalTokens += tokens
          if (tokens > 0) activeDays++
          weekTokens += tokens
          cumulativeByDay.set(calendarDateKey(date), running)
        }
        days.push({ key: calendarDateKey(date), date, input: bucket?.input_tokens ?? 0, output: bucket?.output_tokens ?? 0, tokens, records: bucket?.record_count ?? 0, future })
      }
      // 月份归到每周中点（周四），避免跨月周归期摇摆；相邻标签至少隔 3 列防重叠。
      const mid = new Date(cursor.getFullYear(), cursor.getMonth(), cursor.getDate() + 3)
      let monthLabel = ''
      if (mid.getMonth() !== prevMonth) {
        if (built.length - lastLabelColumn >= 3) {
          monthLabel = monthFormat.format(mid)
          lastLabelColumn = built.length
        }
        prevMonth = mid.getMonth()
      }
      built.push({ days, tokens: weekTokens, monthLabel })
    }
    return { weeks: built, totalTokens, activeDays, cumulativeByDay }
  }, [buckets])

  const thresholds = useMemo(() => {
    const values = mode === 'weekly'
      ? weeks.map(week => week.tokens).filter(value => value > 0)
      : mode === 'cumulative'
        ? [...cumulativeByDay.values()].filter(value => value > 0)
        : weeks.flatMap(week => week.days.map(day => day.tokens)).filter(value => value > 0)
    if (!values.length) return null
    values.sort((a, b) => a - b)
    const pick = (ratio: number) => values[Math.min(values.length - 1, Math.floor(ratio * values.length))]
    return [pick(0.25), pick(0.5), pick(0.75)] as const
  }, [mode, weeks, cumulativeByDay])

  const fullDate = useMemo(() => new Intl.DateTimeFormat(intlLocale(), { year: 'numeric', month: 'short', day: 'numeric' }), [])
  const shortDate = useMemo(() => new Intl.DateTimeFormat(intlLocale(), { month: 'short', day: 'numeric' }), [])
  const weekdayLabels = useMemo(() => {
    const format = new Intl.DateTimeFormat(intlLocale(), { weekday: 'short' })
    return [0, 2, 4].map(offset => format.format(new Date(2024, 0, 1 + offset)))
  }, [])

  function levelOf(value: number) {
    if (!thresholds || value <= 0) return 0
    if (value <= thresholds[0]) return 1
    if (value <= thresholds[1]) return 2
    if (value <= thresholds[2]) return 3
    return 4
  }

  function cellValue(day: CalendarDay, week: CalendarWeek) {
    if (mode === 'weekly') return week.tokens
    if (mode === 'cumulative') return cumulativeByDay.get(day.key) ?? 0
    return day.tokens
  }

  function cellTitle(day: CalendarDay, week: CalendarWeek) {
    if (mode === 'weekly') {
      const days = week.days.filter(item => !item.future)
      return t('memory.usage.calendarWeekCell', { start: shortDate.format(days[0].date), end: shortDate.format(days[days.length - 1].date), tokens: amount(week.tokens) })
    }
    if (mode === 'cumulative') {
      return t('memory.usage.calendarCumCell', { date: fullDate.format(day.date), tokens: amount(cumulativeByDay.get(day.key) ?? 0) })
    }
    return t('memory.usage.calendarCell', { date: fullDate.format(day.date), tokens: amount(day.tokens), input: amount(day.input), output: amount(day.output), records: day.records })
  }

  return <section className="mt-7 rounded-2xl border border-slate-200 bg-white p-5 shadow-[0_1px_2px_rgba(15,23,42,0.03)] dark:border-slate-800 dark:bg-slate-900 sm:p-7" aria-labelledby="usage-calendar-title">
    <div className="flex flex-wrap items-start justify-between gap-3">
      <div className="flex items-start gap-3">
        <span className="grid h-10 w-10 shrink-0 place-items-center rounded-xl bg-emerald-50 text-emerald-600 dark:bg-emerald-500/15 dark:text-emerald-300"><Activity size={20} /></span>
        <div>
          <h2 id="usage-calendar-title" className="font-semibold text-slate-900 dark:text-slate-100">{t('memory.usage.calendarTitle')}</h2>
          <p className="mt-1 text-sm text-slate-500 dark:text-slate-400">{t('memory.usage.calendarDesc', { mode: t(CALENDAR_MODE_LABEL_KEYS[mode]) })}</p>
        </div>
      </div>
      <div className="flex rounded-lg bg-slate-100 p-1 dark:bg-slate-800" role="tablist" aria-label={t('memory.usage.calendarModeLabel')}>
        {(Object.keys(CALENDAR_MODE_LABEL_KEYS) as CalendarMode[]).map(key => (
          <button key={key} type="button" role="tab" aria-selected={mode === key} onClick={() => setMode(key)} className={`rounded-md px-3 py-1.5 text-xs font-medium transition focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 ${mode === key ? 'bg-white text-emerald-700 shadow-sm dark:bg-slate-700 dark:text-emerald-300' : 'text-slate-500 hover:text-slate-800 dark:text-slate-400 dark:hover:text-slate-100'}`}>{t(CALENDAR_MODE_LABEL_KEYS[key])}</button>
        ))}
      </div>
    </div>
    {loading ? <div className="mt-5 h-40 animate-pulse rounded-xl bg-slate-100 dark:bg-slate-800" /> : totalTokens === 0 ? (
      <div className="mt-4 rounded-lg border border-dashed border-slate-200 px-4 py-10 text-center text-sm text-slate-500 dark:border-slate-800 dark:text-slate-400">{t('memory.usage.calendarEmpty')}</div>
    ) : (
      <>
        <div className="mt-5 overflow-x-auto pb-1">
          <div className="flex min-w-max gap-[3px]" role="img" aria-label={t('memory.usage.calendarAria')}>
            <div className="flex w-8 shrink-0 flex-col gap-[3px]">
              <span className="h-4" />
              {[0, 1, 2, 3, 4, 5, 6].map(row => (
                <span key={row} className="h-[11px] text-[10px] leading-[11px] text-slate-400 dark:text-slate-500">{row % 2 === 0 ? weekdayLabels[row / 2] : ''}</span>
              ))}
            </div>
            {weeks.map((week, index) => (
              <div key={index} className="flex w-[11px] shrink-0 flex-col gap-[3px]">
                <span className="h-4 whitespace-nowrap text-[10px] font-medium leading-4 text-slate-400 dark:text-slate-500">{week.monthLabel}</span>
                {week.days.map(day => {
                  if (day.future) return <span key={day.key} className="h-[11px]" />
                  const title = cellTitle(day, week)
                  return <span key={day.key} title={title} aria-label={title} className="h-[11px] w-[11px] rounded-[3px] transition-[background-color] duration-150 hover:ring-1 hover:ring-slate-500/70 dark:hover:ring-slate-300/70" style={{ backgroundColor: palette[levelOf(cellValue(day, week))] }} />
                })}
              </div>
            ))}
          </div>
        </div>
        <div className="mt-4 flex flex-wrap items-center justify-between gap-3 border-t border-slate-100 pt-4 dark:border-slate-800">
          <div className="flex items-center gap-1.5 text-xs text-slate-500 dark:text-slate-400">
            <span>{t('memory.usage.calendarLess')}</span>
            {palette.map(color => <span key={color} className="h-[11px] w-[11px] rounded-[3px]" style={{ backgroundColor: color }} />)}
            <span>{t('memory.usage.calendarMore')}</span>
          </div>
          <p className="font-mono text-xs tabular-nums text-slate-500 dark:text-slate-400">{t('memory.usage.calendarSummary', { tokens: compactAmount(totalTokens), days: activeDays })}</p>
        </div>
      </>
    )}
  </section>
}

function PricingEstimator({
  groups,
  rules,
  onRulesChange,
  expanded,
  onToggle,
}: {
  groups: TelemetryUsageCostGroup[]
  rules: Record<string, PricingRule>
  onRulesChange: (next: Record<string, PricingRule>) => void
  expanded: boolean
  onToggle: () => void
}) {
  const { t } = useTranslation()
  const stats = useMemo(() => {
    let cost = 0
    let pricedTokens = 0
    let totalTokens = 0
    for (const group of groups) {
      const tokens = group.input_tokens + group.output_tokens
      totalTokens += tokens
      const estimate = groupCost(group, rules[pricingKey(group)])
      if (estimate !== null) {
        cost += estimate
        pricedTokens += tokens
      }
    }
    return { cost, pricedTokens, totalTokens }
  }, [groups, rules])
  const coverage = stats.totalTokens > 0 ? Math.round((stats.pricedTokens / stats.totalTokens) * 100) : 0
  const configured = stats.pricedTokens > 0
  const containsEstimated = groups.some(group => group.estimated_tokens > 0 && groupCost(group, rules[pricingKey(group)]) !== null)

  const update = (group: TelemetryUsageCostGroup, field: keyof PricingRule, raw: string) => {
    const key = pricingKey(group)
    const current = rules[key] ?? { input: 0, output: 0, cache: 0 }
    const value = raw.trim() === '' ? 0 : Math.max(0, Number(raw) || 0)
    onRulesChange({ ...rules, [key]: { ...current, [field]: value } })
  }

  return <section className="mt-7 rounded-2xl border border-slate-200 bg-white p-5 dark:border-slate-800 dark:bg-slate-900 sm:p-7" aria-labelledby="usage-cost-title">
    <div className="flex flex-wrap items-start justify-between gap-4">
      <div className="flex min-w-0 items-start gap-3">
        <span className="grid h-10 w-10 shrink-0 place-items-center rounded-xl bg-emerald-50 text-emerald-600 dark:bg-emerald-500/15 dark:text-emerald-300"><Calculator size={20} /></span>
        <div>
          <h2 id="usage-cost-title" className="font-semibold text-slate-900 dark:text-slate-100">{t('memory.usage.costTitle')}</h2>
          <p className="mt-1 max-w-2xl text-sm text-slate-500 dark:text-slate-400">{t('memory.usage.costDesc')}</p>
        </div>
      </div>
      <button type="button" onClick={onToggle} className="inline-flex h-9 items-center gap-2 rounded-lg border border-slate-200 px-3 text-xs font-medium text-slate-600 transition hover:border-slate-300 hover:bg-slate-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 dark:border-slate-700 dark:text-slate-300 dark:hover:bg-slate-800">
        <SlidersHorizontal size={14} />{t(expanded ? 'memory.usage.costHide' : 'memory.usage.costConfigure')}
      </button>
    </div>
    <div className="mt-5 flex flex-wrap items-end justify-between gap-4 border-t border-slate-100 pt-5 dark:border-slate-800">
      <div>
        <p className="text-xs font-medium text-slate-500 dark:text-slate-400">{t('memory.usage.costEstimated')}</p>
        <p className="mt-1 font-mono text-3xl font-semibold tracking-[-0.03em] tabular-nums text-slate-900 dark:text-slate-100">{configured ? new Intl.NumberFormat(intlLocale(), { style: 'currency', currency: 'USD', minimumFractionDigits: stats.cost < 1 ? 4 : 2, maximumFractionDigits: stats.cost < 1 ? 4 : 2 }).format(stats.cost) : '—'}</p>
      </div>
      <div className="max-w-lg text-right text-xs text-slate-500 dark:text-slate-400">
        <p>{configured ? t('memory.usage.costCoverage', { pct: coverage }) : t('memory.usage.costCoverageNone')}</p>
        {containsEstimated && <p className="mt-1 text-amber-600 dark:text-amber-300">{t('memory.usage.costEstimatedWarning')}</p>}
      </div>
    </div>
    {expanded && <div className="mt-5 border-t border-slate-100 pt-5 dark:border-slate-800">
      <h3 className="text-sm font-semibold text-slate-800 dark:text-slate-100">{t('memory.usage.costRulesTitle')}</h3>
      <p className="mt-1 text-xs leading-5 text-slate-500 dark:text-slate-400">{t('memory.usage.costRulesHint')}</p>
      <div className="mt-4 space-y-2">
        {groups.map(group => {
          const key = pricingKey(group)
          const rule = rules[key] ?? { input: 0, output: 0, cache: 0 }
          const estimate = groupCost(group, rule)
          return <div key={key} className="grid gap-3 rounded-xl bg-slate-50 px-3 py-3 dark:bg-slate-950/50 lg:grid-cols-[minmax(10rem,1fr)_repeat(3,minmax(6rem,0.65fr))_7rem] lg:items-end">
            <div className="min-w-0"><p className="truncate text-sm font-medium text-slate-800 dark:text-slate-100">{group.model || t('memory.usage.costUnknownModel')}</p><p className="mt-0.5 text-xs text-slate-500">{sourceLabel(group.source)} · {compactAmount(group.input_tokens + group.output_tokens)}</p></div>
            {(['input', 'output', 'cache'] as const).map(field => <label key={field} className="text-xs text-slate-500 dark:text-slate-400"><span className="mb-1 block">{t(`memory.usage.cost${field === 'input' ? 'Input' : field === 'output' ? 'Output' : 'Cache'}Rate`)}</span><input type="number" min="0" step="0.01" inputMode="decimal" value={rule[field] || ''} onChange={event => update(group, field, event.target.value)} className="h-9 w-full rounded-lg border border-slate-200 bg-white px-2.5 font-mono text-sm tabular-nums text-slate-800 outline-none focus:border-emerald-500 focus:ring-2 focus:ring-emerald-500/15 dark:border-slate-700 dark:bg-slate-900 dark:text-slate-100" /></label>)}
            <div className="text-right"><p className="text-[11px] text-slate-400">{t('memory.usage.costEstimated')}</p><p className="mt-1 font-mono text-sm font-semibold tabular-nums">{estimate === null ? '—' : new Intl.NumberFormat(intlLocale(), { style: 'currency', currency: 'USD', minimumFractionDigits: estimate < 1 ? 4 : 2, maximumFractionDigits: estimate < 1 ? 4 : 2 }).format(estimate)}</p></div>
          </div>
        })}
      </div>
    </div>}
  </section>
}

export const UsageAnalytics = memo(function UsageAnalytics({ active = true }: { active?: boolean }) {
  const { t } = useTranslation()
  const { loadUsageAnalytics, checkTelemetry } = useMemoryStore()
  const [tab, setTab] = useState<TabKey>('trend')
  const [range, setRange] = useState<RangeKey>('today')
  const [source, setSource] = useState('all')
  const [analytics, setAnalytics] = useState<TelemetryUsageAnalytics | null>(null)
  const [loading, setLoading] = useState(true)
  const [refreshing, setRefreshing] = useState(false)
  const [loadError, setLoadError] = useState('')
  const [pricingRules, setPricingRules] = useState<Record<string, PricingRule>>(loadPricingRules)
  const [pricingExpanded, setPricingExpanded] = useState(false)
  const [calendarBuckets, setCalendarBuckets] = useState<TelemetryUsageBucket[] | null>(null)
  const requestVersion = useRef(0)
  const calendarVersion = useRef(0)

  useEffect(() => {
    try { localStorage.setItem(PRICING_STORAGE_KEY, JSON.stringify(pricingRules)) } catch { /* Local storage can be disabled. */ }
  }, [pricingRules])

  // One fetch path for polling, backend events and manual refresh. Old filter
  // responses cannot overwrite the current view, including after hiding it.
  const reloadAnalytics = useCallback(async () => {
    const version = ++requestVersion.current
    try {
      const result = await loadUsageAnalytics({ ...boundsFor(range), source })
      if (version === requestVersion.current) {
        setAnalytics(result)
        setLoadError('')
      }
    } catch (error) {
      if (version === requestVersion.current) setLoadError(String(error))
    } finally {
      if (version === requestVersion.current) setLoading(false)
    }
  }, [range, source, loadUsageAnalytics])

  // The activity calendar covers a rolling year regardless of the range
  // selector, so it fetches its own day buckets keyed only by the source.
  const reloadCalendar = useCallback(async () => {
    const version = ++calendarVersion.current
    const now = new Date()
    const start = new Date(now.getFullYear(), now.getMonth(), now.getDate() - 364)
    const tomorrow = new Date(now.getFullYear(), now.getMonth(), now.getDate() + 1)
    try {
      const result = await loadUsageAnalytics({ startAt: start.toISOString(), endAt: tomorrow.toISOString(), source, bucket: 'day' })
      if (version === calendarVersion.current) setCalendarBuckets(result.buckets)
    } catch {
      if (version === calendarVersion.current) setCalendarBuckets([])
    }
  }, [source, loadUsageAnalytics])

  useEffect(() => {
    if (active) {
      setLoading(true)
      void checkTelemetry({ limit: 20 })
    }
    return () => { requestVersion.current += 1 }
  }, [range, source, active, checkTelemetry])

  useEffect(() => {
    if (!active) return
    void reloadCalendar()
    return () => { calendarVersion.current += 1 }
  }, [active, reloadCalendar])

  useVisiblePolling(reloadAnalytics, 30_000, active)
  useEffect(() => {
    if (!active) return
    let alive = true
    let unlisten: (() => void) | undefined
    void listen('telemetry-updated', () => { if (alive) { void reloadAnalytics(); void reloadCalendar() } })
      .then(stop => { if (alive) unlisten = stop; else stop() })
      .catch(() => { /* Polling remains the fallback outside Tauri. */ })
    return () => { alive = false; unlisten?.() }
  }, [reloadAnalytics, reloadCalendar, active])

  const totals = { input: analytics?.input_tokens ?? 0, output: analytics?.output_tokens ?? 0, cache: analytics?.cached_tokens ?? 0 }
  const currentTotal = totals.input + totals.output
  const estimatedTokens = analytics?.estimated_tokens ?? 0
  const estimatedShare = currentTotal > 0 ? (estimatedTokens / currentTotal) * 100 : 0
  const cacheRatio = totals.input > 0 ? (totals.cache / totals.input) * 100 : 0

  async function refresh() {
    if (refreshing) return
    setRefreshing(true)
    try {
      await checkTelemetry({ limit: 20, refreshUsage: true })
      setLoading(true)
      void reloadCalendar()
      await reloadAnalytics()
    } catch (error) {
      setLoadError(String(error))
    } finally { setRefreshing(false); setLoading(false) }
  }

  return <main className="h-full overflow-y-auto bg-[#fbfcfe] text-slate-900 dark:bg-slate-950 dark:text-slate-100">
    <div className="mx-auto max-w-[1500px] px-5 py-6 lg:px-9 lg:py-8">
      <header className="flex flex-wrap items-start justify-between gap-4">
        <div>
          <h1 className="text-2xl font-bold tracking-[-0.025em]">{t('nav.usage')}</h1>
          <p className="mt-1 text-sm text-slate-500 dark:text-slate-400">{t('memory.usage.desc')}</p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <label className="sr-only" htmlFor="usage-source">{t('memory.usage.sourceLabel')}</label><div className="relative"><select id="usage-source" value={source} onChange={event => setSource(event.target.value)} className="h-10 appearance-none rounded-lg border border-slate-200 bg-white py-0 pl-3 pr-9 text-sm font-medium text-slate-700 outline-none transition hover:border-slate-300 focus:border-blue-500 focus:ring-2 focus:ring-blue-500/20 dark:border-slate-700 dark:bg-slate-900 dark:text-slate-200"><option value="all">{t('memory.usage.allSources')}</option>{(analytics?.sources ?? []).map(item => <option key={item} value={item}>{sourceLabel(item)}</option>)}</select><ChevronDown size={16} className="pointer-events-none absolute right-3 top-3 text-slate-400" /></div>
          <div className="relative"><select value={range} onChange={event => setRange(event.target.value as RangeKey)} className="h-10 appearance-none rounded-lg border border-slate-200 bg-white py-0 pl-9 pr-9 text-sm font-medium text-slate-700 outline-none transition hover:border-slate-300 focus:border-blue-500 focus:ring-2 focus:ring-blue-500/20 dark:border-slate-700 dark:bg-slate-900 dark:text-slate-200"><option value="today">{t('memory.usage.rangeToday')}</option><option value="7d">{t('memory.usage.range7d')}</option><option value="30d">{t('memory.usage.range30d')}</option><option value="all">{t('memory.usage.rangeAll')}</option></select><CalendarDays size={16} className="pointer-events-none absolute left-3 top-3 text-slate-400" /><ChevronDown size={16} className="pointer-events-none absolute right-3 top-3 text-slate-400" /></div>
          <button type="button" onClick={() => { void refresh() }} disabled={refreshing} className="inline-flex h-10 items-center gap-2 rounded-lg border border-slate-200 bg-white px-3 text-sm font-medium text-slate-700 transition hover:border-slate-300 hover:bg-slate-50 disabled:cursor-wait disabled:opacity-60 focus:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 dark:border-slate-700 dark:bg-slate-900 dark:text-slate-200 dark:hover:bg-slate-800"><RefreshCw size={16} className={refreshing ? 'animate-spin motion-reduce:animate-none' : ''} />{t('memory.usage.rescan')}</button>
        </div>
      </header>
      {loadError && <div role="alert" className="mt-4 rounded-lg bg-amber-500/10 px-3 py-2 text-xs text-amber-800 dark:text-amber-200">
        <p className="break-words">{loadError}</p>
        <ErrorRecovery onRetry={() => { void refresh() }} disabled={refreshing} />
      </div>}

      <section className="mt-7 rounded-2xl border border-slate-200 bg-white p-5 shadow-[0_1px_2px_rgba(15,23,42,0.03)] dark:border-slate-800 dark:bg-slate-900 sm:p-7" aria-label={t('memory.usage.overviewLabel')}>
        <div className="flex flex-wrap items-start justify-between gap-5"><div className="flex items-center gap-4"><span className="grid h-12 w-12 place-items-center rounded-2xl bg-blue-50 text-blue-500 dark:bg-blue-500/15 dark:text-blue-300"><Zap size={25} /></span><div><p className="text-sm font-medium text-slate-500 dark:text-slate-400">{t('memory.usage.totalLabel')}</p><p className="mt-0.5 font-mono text-3xl font-semibold tracking-[-0.04em] tabular-nums sm:text-4xl">{loading ? '—' : amount(currentTotal)}</p><p className="mt-1 text-xs text-slate-400">{loading ? t('memory.usage.totalLoading') : `${t('memory.usage.totalSubtext', { value: compactAmount(currentTotal) })}${estimatedTokens > 0 ? ` · ${t('memory.usage.totalSubtextEstimated', { pct: estimatedShare >= 1 ? estimatedShare.toFixed(0) : '<1' })}` : ''}`}</p></div></div><div className="grid min-w-[14rem] grid-cols-2 divide-x divide-slate-100 rounded-xl border border-slate-100 dark:divide-slate-800 dark:border-slate-800"><div className="px-4 py-3"><p className="text-xs font-medium text-slate-500">{t('memory.usage.ledgerLabel')}</p><p className="mt-1 font-mono text-xl font-semibold tabular-nums">{loading ? '—' : amount(analytics?.record_count ?? 0)}</p></div><div className="px-4 py-3"><p className="text-xs font-medium text-slate-500">{t('memory.usage.detailLabel')}</p><p className="mt-1 text-sm font-semibold text-slate-600 dark:text-slate-300">{loading ? '—' : analytics?.truncated_records ? t('memory.usage.detailLatest') : t('memory.usage.detailAll')}</p></div></div></div>
        <div className="mt-6 grid gap-3 sm:grid-cols-2 xl:grid-cols-4"><Metric icon={<BarChart3 size={16} />} label={t('memory.usage.inputLabel')} value={amount(totals.input)} tone="blue" /><Metric icon={<Sparkles size={16} />} label={t('memory.usage.outputLabel')} value={amount(totals.output)} tone="violet" /><Metric icon={<Database size={16} />} label={t('memory.usage.cacheLabel')} value={amount(totals.cache)} tone="emerald" /><div className="rounded-xl border border-slate-100 px-4 py-3 dark:border-slate-800"><div className="flex items-center justify-between text-sm text-slate-500"><span>{t('memory.usage.cacheHitLabel')}</span>{analytics && analytics.cache_capable === false ? <span className="font-semibold text-slate-400 dark:text-slate-500">—</span> : <span className="font-semibold text-emerald-600 dark:text-emerald-400">{Math.min(100, cacheRatio).toFixed(1)}%</span>}</div>{analytics && analytics.cache_capable === false ? <p className="mt-3 text-xs leading-5 text-slate-400 dark:text-slate-500">{t('memory.usage.cacheUnavailable')}</p> : <div className="mt-3 h-1.5 overflow-hidden rounded-full bg-slate-100 dark:bg-slate-800"><div className="h-full rounded-full bg-emerald-500 transition-[width] duration-200 motion-reduce:transition-none" style={{ width: `${Math.min(100, cacheRatio)}%` }} /></div>}</div></div>
      </section>

      <UsageCalendar buckets={calendarBuckets ?? []} loading={calendarBuckets === null} />

      <section className="mt-7 rounded-2xl border border-slate-200 bg-white p-5 shadow-[0_1px_2px_rgba(15,23,42,0.03)] dark:border-slate-800 dark:bg-slate-900 sm:p-7"><div className="flex flex-wrap items-center justify-between gap-3"><div className="flex rounded-lg bg-slate-100 p-1 dark:bg-slate-800" role="tablist" aria-label={t('memory.usage.tablistLabel')}><Tab active={tab === 'trend'} icon={<Gauge size={15} />} onClick={() => setTab('trend')}>{t('memory.usage.tabTrend')}</Tab><Tab active={tab === 'records'} icon={<FileClock size={15} />} onClick={() => setTab('records')}>{t('memory.usage.tabRecords')}</Tab></div><p className="text-sm text-slate-500 dark:text-slate-400">{t(RANGE_LABEL_KEYS[range])} · {source === 'all' ? t('memory.usage.allSources') : sourceLabel(source)}</p></div>
        {tab === 'trend' ? <><div className="mt-6 flex items-center justify-between"><h2 className="text-lg font-semibold tracking-[-0.015em]">{t('memory.usage.trendTitle')}</h2><div className="hidden items-center gap-3 text-xs text-slate-500 sm:flex"><Legend color="bg-blue-500" label={t('memory.usage.legendInput')} /><Legend color="bg-violet-500" label={t('memory.usage.legendOutput')} /><Legend color="bg-emerald-500" label={t('memory.usage.legendCache')} /></div></div>{loading ? <div className="mt-5 h-64 animate-pulse rounded-xl bg-slate-100 dark:bg-slate-800" /> : <UsageTrend buckets={analytics?.buckets ?? []} range={range} />}</> : <UsageRecords events={analytics?.records ?? []} truncated={Boolean(analytics?.truncated_records)} />}
      </section>

      <UsageHeatmap cells={analytics?.heatmap ?? []} loading={loading} />

      <PricingEstimator
        groups={analytics?.cost_groups ?? []}
        rules={pricingRules}
        onRulesChange={setPricingRules}
        expanded={pricingExpanded}
        onToggle={() => setPricingExpanded(value => !value)}
      />
    </div>
  </main>
})

function Metric({ icon, label, value, tone }: { icon: React.ReactNode; label: string; value: string; tone: 'blue' | 'violet' | 'emerald' }) { const tones = { blue: 'text-blue-500', violet: 'text-violet-500', emerald: 'text-emerald-500' }; return <div className="rounded-xl border border-slate-100 px-4 py-3 dark:border-slate-800"><div className="flex items-center gap-2 text-sm text-slate-500"><span className={tones[tone]}>{icon}</span>{label}</div><p className="mt-2 font-mono text-xl font-semibold tracking-[-0.02em] tabular-nums">{value}</p></div> }
function Tab({ active, icon, children, onClick }: { active: boolean; icon: React.ReactNode; children: React.ReactNode; onClick: () => void }) { return <button type="button" role="tab" aria-selected={active} onClick={onClick} className={`inline-flex items-center gap-2 rounded-md px-3 py-2 text-sm font-medium transition focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 ${active ? 'bg-white text-emerald-700 shadow-sm dark:bg-slate-700 dark:text-emerald-300' : 'text-slate-500 hover:text-slate-800 dark:text-slate-400 dark:hover:text-slate-100'}`}>{icon}{children}</button> }
function Legend({ color, label }: { color: string; label: string }) { return <span className="inline-flex items-center gap-1.5"><i className={`h-2 w-2 rounded-full ${color}`} />{label}</span> }
function TooltipMetric({ label, value, tone }: { label: string; value: number; tone: string }) { return <p className="flex items-center justify-between gap-3 font-mono text-xs leading-6 tabular-nums text-slate-600 dark:text-slate-300"><span className="inline-flex items-center gap-1.5"><i className={`h-2 w-2 rounded-full ${tone}`} />{label}</span><span>{amount(value)}</span></p> }
function TrendMarker({ x, y, tone }: { x: number; y: number; tone: string }) { return <i className={`absolute h-2.5 w-2.5 -translate-x-1/2 -translate-y-1/2 rounded-full border-2 border-white shadow-sm dark:border-slate-900 ${tone}`} style={{ left: `${x}%`, top: `${y}%` }} /> }

function UsageRecords({ events, truncated }: { events: TelemetryUsageRecord[]; truncated: boolean }) {
  const { t } = useTranslation()
  if (!events.length) return <div className="flex min-h-64 flex-col items-center justify-center text-center"><span className="grid h-11 w-11 place-items-center rounded-xl bg-slate-100 text-slate-400 dark:bg-slate-800"><FileClock size={20} /></span><h2 className="mt-3 font-medium">{t('memory.usage.recordsEmptyTitle')}</h2><p className="mt-1 max-w-md text-sm text-slate-500">{t('memory.usage.recordsEmptyDesc')}</p></div>
  return <><div className="mt-4 flex items-center justify-between text-xs text-slate-500 dark:text-slate-400"><span>{t('memory.usage.tableInfoOrder')}</span>{truncated && <span>{t('memory.usage.tableInfoTruncated')}</span>}</div><div className="mt-2 overflow-x-auto rounded-xl border border-slate-100 dark:border-slate-800"><table className="min-w-[850px] w-full text-left text-sm"><thead className="border-b border-slate-100 bg-slate-50/80 text-xs font-medium text-slate-500 dark:border-slate-800 dark:bg-slate-950/40"><tr><th className="px-5 py-3.5">{t('memory.usage.thTime')}</th><th className="px-5 py-3.5">{t('memory.usage.thSource')}</th><th className="px-5 py-3.5">{t('memory.usage.thModel')}</th><th className="px-5 py-3.5 text-right">{t('memory.usage.thInput')}</th><th className="px-5 py-3.5 text-right">{t('memory.usage.thOutput')}</th><th className="px-5 py-3.5 text-right">{t('memory.usage.thCache')}</th><th className="px-5 py-3.5 text-right">{t('memory.usage.thTotal')}</th><th className="px-5 py-3.5">{t('memory.usage.thOrigin')}</th></tr></thead><tbody className="divide-y divide-slate-100 dark:divide-slate-800">{events.map(event => <tr key={event.record_id} className="transition hover:bg-slate-50/70 dark:hover:bg-slate-800/40"><td className="whitespace-nowrap px-5 py-4 font-mono text-xs text-slate-500">{displayTime(event.occurred_at)}</td><td className="px-5 py-4 font-medium">{sourceLabel(event.source)}</td><td className="px-5 py-4 text-slate-600 dark:text-slate-300"><span className="rounded bg-slate-100 px-2 py-1 text-xs dark:bg-slate-800">{event.model ?? (event.record_kind === 'session_total' ? t('memory.usage.modelSessionTotal') : t('memory.usage.modelUnset'))}</span></td><td className="px-5 py-4 text-right font-mono tabular-nums">{amount(event.input_tokens)}</td><td className="px-5 py-4 text-right font-mono tabular-nums">{amount(event.output_tokens)}</td><td className="px-5 py-4 text-right font-mono tabular-nums text-emerald-600 dark:text-emerald-400">{amount(event.cached_tokens)}</td><td className="px-5 py-4 text-right font-mono font-semibold tabular-nums">{amount(tokenTotal(event))}</td><td className="px-5 py-4"><span className={event.origin.startsWith('native') || event.origin.startsWith('adapter') ? 'text-emerald-600 dark:text-emerald-400' : 'text-amber-600 dark:text-amber-400'}>{event.origin.startsWith('native') ? t('memory.usage.originLog') : event.origin.startsWith('adapter') ? t('memory.usage.originReported') : t('memory.usage.originEstimated')}</span></td></tr>)}</tbody></table></div></>
}
