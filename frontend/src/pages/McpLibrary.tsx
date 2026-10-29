import { TARGETS, targetOf } from '../features/mcp/targets'
import { EquipmentProfiles, EquipmentProfilesButton } from '../features/equipment/EquipmentProfiles'
import { AddEditDialog } from '../features/mcp/components/AddEditDialog'
import { EquipDialog } from '../features/mcp/components/EquipDialog'
import { ImportDialog } from '../features/mcp/components/ImportDialog'
import { ConfigurationArbitrationDialog } from '../features/mcp/components/ConfigurationArbitrationDialog'
import { ClaudeDesktopConfigSelector } from '../features/mcp/components/ClaudeDesktopConfigSelector'
import { mcpApi } from '../api/mcp'
import { listen } from '@tauri-apps/api/event'

import { memo, useCallback, useEffect, useMemo, useRef, useState } from 'react'


import { useTranslation } from 'react-i18next'
import { ErrorRecovery } from '../components/ErrorRecovery'
import { HelpDocument } from '../components/HelpDocument'
import { Plug, Plus, Download, RefreshCw, Search, Trash2, Pencil, Loader2, Terminal, Globe, X, Check, Activity, ChevronRight } from 'lucide-react'
import { useMcpStore, useMcpFields } from '../store/mcpStore'
import type { McpDashboardStats, McpCatalogEntry, McpFailedCallPage, McpToolDiscovery, MemoryMcpTarget } from '../types/mcp'

/** 装备目标：id 与后端 MemoryMcpTarget 一致；remote 标记该目标支持 SSE/HTTP。
 * CLI 与 Desktop 是独立安装的目标，全部独立展示。 */


// ── 主页面 ────────────────────────────────────────────────────────────────────

type McpSection = 'services' | 'tools' | 'calls' | 'errors' | 'help'

/** 帮助说明各小节的 i18n key 前缀。 */
const HELP_TOPICS = ['keyStorage', 'keyRotation', 'observation', 'tools', 'equip', 'import', 'desktopConfig', 'coverage', 'errors'] as const

export const McpLibrary = memo(function McpLibrary({ active = true, section = 'services', onSectionChange }: {
  active?: boolean
  section?: McpSection
  onSectionChange?: (section: McpSection) => void
}) {
  const { t, i18n } = useTranslation()
  const [equipmentOpen, setEquipmentOpen] = useState(false)
  useEffect(() => { setEquipmentOpen(false) }, [section])
  const { mcpCatalog, mcpStatuses, mcpObservationStatuses, mcpObservationError, mcpStatusError: statusError, mcpStatusState, mcpCallStats, mcpCallRefreshState, mcpCallError, memoryMcp, memoryMcpRuntime, memoryMcpRuntimeError, loadMcpCatalog, checkMcpStatuses, loadMcpCallStats, loadMcpObservationStatuses, setMcpObservation, dshBridge, dshBridgeError, loadDshBridge, setDshBridge, setDshBridgeFeatures, refreshMcpCallObservations, deleteMcpEntry, checkMemoryMcp, checkMemoryMcpRuntime, installMemoryMcp, uninstallMemoryMcp } = useMcpFields('mcpCatalog', 'mcpStatuses', 'mcpObservationStatuses', 'mcpObservationError', 'mcpStatusError', 'mcpStatusState', 'mcpCallStats', 'mcpCallRefreshState', 'mcpCallError', 'memoryMcp', 'memoryMcpRuntime', 'memoryMcpRuntimeError', 'loadMcpCatalog', 'checkMcpStatuses', 'loadMcpCallStats', 'loadMcpObservationStatuses', 'setMcpObservation', 'dshBridge', 'dshBridgeError', 'loadDshBridge', 'setDshBridge', 'setDshBridgeFeatures', 'refreshMcpCallObservations', 'deleteMcpEntry', 'checkMemoryMcp', 'checkMemoryMcpRuntime', 'installMemoryMcp', 'uninstallMemoryMcp')

  const [dashboardStats, setDashboardStats] = useState<McpDashboardStats | null>(null)
  const [dashboardError, setDashboardError] = useState(false)
  const dashboardRequestId = useRef(0)
  const [booting, setBooting] = useState(true)
  const [loading, setLoading] = useState(false)
  const [notice, setNotice] = useState('')
  const [memoryMcpAction, setMemoryMcpAction] = useState<MemoryMcpTarget | null>(null)
  const [memoryMcpActionError, setMemoryMcpActionError] = useState('')
  const [query, setQuery] = useState('')
  const [toolService, setToolService] = useState('')
  const [callServer, setCallServer] = useState('')
  const [callSource, setCallSource] = useState('')
  const [callClock, setCallClock] = useState(() => Date.now())
  const callViewRef = useRef({ section, callServer, callSource })
  const [errorServer, setErrorServer] = useState('')
  const [errorOrigin, setErrorOrigin] = useState('')
  const [errorTool, setErrorTool] = useState('')
  const [errorWindow, setErrorWindow] = useState<{ start: string; end: string } | null>(null)
  const [callToolQuery, setCallToolQuery] = useState('')
  const [callToolSort, setCallToolSort] = useState<'calls' | 'failure' | 'name'>('calls')
  const [callToolPage, setCallToolPage] = useState(0)
  const [addOpen, setAddOpen] = useState(false)
  const [editing, setEditing] = useState<McpCatalogEntry | null>(null)
  const [equipping, setEquipping] = useState<McpCatalogEntry | null>(null)
  const [importOpen, setImportOpen] = useState(false)
  const [arbitrating, setArbitrating] = useState<{ name: string; agent?: string } | null>(null)
  const [loadError, setLoadError] = useState('')
  const [migrationNotices, setMigrationNotices] = useState<string[]>([])
  const [configBusy, setConfigBusy] = useState(false)
  const [busyObservation, setBusyObservation] = useState<string | null>(null)
  const [busyDshObservation, setBusyDshObservation] = useState(false)
  const [observationNotice, setObservationNotice] = useState<{ message: string; tone: 'success' | 'warning' | 'error' } | null>(null)
  const [showCoverage, setShowCoverage] = useState(false)
  const [busyAssignment, setBusyAssignment] = useState<string | null>(null)
  const [expandedObservations, setExpandedObservations] = useState<string[]>([])
  const [expandedAssignments, setExpandedAssignments] = useState<string[]>([])
  const failedScope = JSON.stringify([errorServer, errorOrigin, errorTool, errorWindow])
  const [failedPage, setFailedPage] = useState<{ scope: string; page: McpFailedCallPage; baselineTotal: number } | null>(null)
  const failedPageRef = useRef(failedPage)
  const failedCalls = failedPage?.scope === failedScope ? failedPage.page : null
  const [newFailedCalls, setNewFailedCalls] = useState(0)
  const [failedHasMore, setFailedHasMore] = useState(true)
  const failedProbeId = useRef(0)
  const [failedCallsError, setFailedCallsError] = useState('')
  const [failedCallsLoading, setFailedCallsLoading] = useState(false)
  const failedRequestId = useRef(0)
  const [toolDiscovery, setToolDiscovery] = useState<Record<string, { loading: boolean; error: string; result: McpToolDiscovery | null }>>({})
  const operationOpen = addOpen || editing !== null || equipping !== null || importOpen || arbitrating !== null

  useEffect(() => {
    callViewRef.current = { section, callServer, callSource }
  }, [section, callServer, callSource])

  const loadFailedCalls = useCallback(async (offset = 0) => {
    const requestId = ++failedRequestId.current
    ++failedProbeId.current
    setFailedCallsLoading(true)
    setFailedCallsError('')
    try {
      const previous = failedPageRef.current?.scope === failedScope ? failedPageRef.current.page : null
      const cursor = offset > 0 ? previous?.calls.at(-1) : undefined
      const page = await mcpApi.telemetryMcpFailedCalls({
        offset: cursor ? 0 : offset, limit: 25, server: errorServer || null, origin: errorOrigin || null,
        tool: errorTool || null, startAt: errorWindow?.start ?? null, endAt: errorWindow?.end ?? null,
        snapshotId: offset > 0 ? previous?.snapshot_id ?? null : null,
        beforeAt: cursor?.occurred_at ?? null, beforeId: cursor?.id ?? null,
      })
      if (requestId !== failedRequestId.current) return
      const next = {
        scope: failedScope,
        baselineTotal: offset > 0 && failedPageRef.current?.scope === failedScope ? failedPageRef.current.baselineTotal : page.total,
        page: offset > 0 && previous ? {
        ...page, calls: [...previous.calls, ...page.calls.filter(call => !previous.calls.some(old => old.id === call.id))],
      } : page }
      failedPageRef.current = next
      setFailedPage(next)
      setFailedHasMore(page.calls.length === 25)
      if (offset === 0) setNewFailedCalls(0)
    } catch (error) {
      if (requestId === failedRequestId.current) setFailedCallsError(String(error))
    } finally {
      if (requestId === failedRequestId.current) setFailedCallsLoading(false)
    }
  }, [errorServer, errorOrigin, errorTool, errorWindow, failedScope])

  const checkNewFailures = useCallback(async () => {
    const previous = failedPageRef.current
    if (!previous || previous.scope !== failedScope) return
    const requestId = ++failedProbeId.current
    try {
      const latest = await mcpApi.telemetryMcpFailedCalls({
        offset: 0, limit: 1, server: errorServer || null, origin: errorOrigin || null,
        tool: errorTool || null, startAt: errorWindow?.start ?? null, endAt: errorWindow?.end ?? null,
        snapshotId: null,
      })
      if (requestId === failedProbeId.current && failedPageRef.current === previous) {
        setNewFailedCalls(Math.max(0, latest.total - previous.baselineTotal))
      }
    } catch {
      // Keep the user's loaded snapshot and expanded details while the next poll retries.
    }
  }, [errorServer, errorOrigin, errorTool, errorWindow, failedScope])

  const loadDashboard = useCallback(async () => {
    const requestId = ++dashboardRequestId.current
    try {
      const stats = await mcpApi.telemetryMcpDashboard()
      if (requestId !== dashboardRequestId.current) return
      setDashboardStats(stats)
      setDashboardError(false)
    } catch {
      if (requestId === dashboardRequestId.current) setDashboardError(true)
    }
  }, [])

  const refreshCallStatistics = useCallback(async () => {
    await Promise.all([
      loadMcpCallStats({ server: callServer || undefined, source: callSource || undefined, recentLimit: 200 }),
      loadDashboard(),
    ])
    setCallClock(Date.now())
    if (section === 'errors') await loadFailedCalls()
  }, [loadMcpCallStats, callServer, callSource, section, loadFailedCalls, loadDashboard])

  const refreshAll = useCallback(async () => {
    setLoading(true)
    setLoadError('')
    try {
      await loadMcpCatalog()
      try {
        setMigrationNotices(await mcpApi.mcpCatalogMigrationReport())
      } catch {
        setMigrationNotices([])
      }
      // Status failures stay independent from catalog availability.
      await Promise.all([
        checkMcpStatuses().catch(() => {}),
        loadMcpObservationStatuses().catch(() => {}),
        loadDshBridge().catch(() => {}),
        ...(callViewRef.current.section === 'calls' ? [loadMcpCallStats({
          server: callViewRef.current.callServer || undefined, source: callViewRef.current.callSource || undefined, recentLimit: 200,
        }).catch(() => {})] : []),
        loadDashboard(),
        checkMemoryMcp().catch(() => {}),
        checkMemoryMcpRuntime().catch(() => {}),
      ])
      // Import the historical baseline in the background on first use. The
      // backend marker makes subsequent visits a cheap no-op.
      void refreshMcpCallObservations().then(result => {
        if (result.imported) void loadDashboard()
        if (result.imported && callViewRef.current.section === 'calls') {
          const { callServer: server, callSource: source } = callViewRef.current
          void loadMcpCallStats({ server: server || undefined, source: source || undefined, recentLimit: 200 }).catch(() => {})
        }
      }).catch(() => {})
    } catch (error) {
      setLoadError(String(error))
    } finally {
      setLoading(false)
      setBooting(false)
    }
  }, [loadMcpCatalog, checkMcpStatuses, loadMcpCallStats, loadMcpObservationStatuses, loadDshBridge, refreshMcpCallObservations, checkMemoryMcp, checkMemoryMcpRuntime, loadDashboard])

  const refreshAfterConfigChange = useCallback(async () => {
    useMcpStore.getState().invalidateMcpStatuses()
    await refreshAll()
  }, [refreshAll])

  // Re-entering the one management view reloads the shared catalog.
  useEffect(() => {
    if (active) void refreshAll()
  }, [active, refreshAll])

  useEffect(() => {
    if (!active || section === 'help' || section === 'tools') return
    let disposed = false
    let polling = false
    let eventTimer: ReturnType<typeof setTimeout> | undefined
    const refreshLive = async () => {
      if (disposed || polling || document.visibilityState !== 'visible') return
      polling = true
      setCallClock(Date.now())
      try {
        await Promise.all([
          loadDashboard(),
          ...(section === 'services' ? [loadMcpObservationStatuses().catch(() => []), loadDshBridge().catch(() => {})] : []),
          ...(section === 'calls' ? [loadMcpCallStats({
            server: callServer || undefined, source: callSource || undefined, recentLimit: 200,
          }).catch(() => {})] : []),
          ...(section === 'errors' ? [checkNewFailures()] : []),
        ])
      } finally { polling = false }
    }
    const timer = window.setInterval(() => { void refreshLive() }, 15000)
    const unlisten = listen('telemetry-updated', () => {
      if (disposed || eventTimer !== undefined) return
      eventTimer = setTimeout(() => { eventTimer = undefined; void refreshLive() }, 750)
    }).catch(() => () => {})
    return () => {
      disposed = true
      window.clearInterval(timer)
      if (eventTimer !== undefined) clearTimeout(eventTimer)
      void unlisten.then(stop => stop())
    }
  }, [active, section, callServer, callSource, loadDashboard, loadMcpObservationStatuses, loadDshBridge, loadMcpCallStats, checkNewFailures])

  const invalidateFailedRequests = useCallback(() => {
    ++failedRequestId.current
    ++failedProbeId.current
  }, [])

  useEffect(() => {
    invalidateFailedRequests()
    setNewFailedCalls(0)
    if (active && section === 'errors') void loadFailedCalls()
    return invalidateFailedRequests
  }, [active, section, loadFailedCalls, invalidateFailedRequests])

  useEffect(() => {
    if (active && section === 'calls') {
      setCallClock(Date.now())
      void loadMcpCallStats({ server: callServer || undefined, source: callSource || undefined, recentLimit: 200 }).catch(() => {})
    }
  }, [active, section, callServer, callSource, loadMcpCallStats])

  useEffect(() => {
    if (!notice) return
    const timer = window.setTimeout(() => setNotice(''), 6000)
    return () => window.clearTimeout(timer)
  }, [notice])

  const visible = useMemo(() => {
    const keyword = query.trim().toLowerCase()
    if (!keyword) return mcpCatalog
    return mcpCatalog.filter((entry) =>
      entry.name.toLowerCase().includes(keyword)
      || entry.description.toLowerCase().includes(keyword))
  }, [mcpCatalog, query])
  const selectedToolEntry = mcpCatalog.find(entry => entry.name === toolService) ?? mcpCatalog[0]
  const selectedToolDiscovery = selectedToolEntry ? toolDiscovery[selectedToolEntry.name] : undefined

  const remoteServices = mcpCatalog.filter(entry => entry.transport === 'sse' || entry.transport === 'http').length
  const confirmedCalls7d = (dashboardStats?.successful_calls_7d ?? 0) + (dashboardStats?.failed_calls_7d ?? 0)
  const failureRate7d = confirmedCalls7d > 0 ? (dashboardStats?.failed_calls_7d ?? 0) / confirmedCalls7d : null
  const healthScore = failureRate7d === null ? '—' : `${((1 - failureRate7d) * 100).toFixed(1)}%`
  const unhealthyTools = dashboardError ? [] : dashboardStats?.high_failure_tools ?? []
  const healthDateFormat = new Intl.DateTimeFormat(i18n.language, { year: 'numeric', month: '2-digit', day: '2-digit' })
  const healthDatesValid = !!dashboardStats && Number.isFinite(Date.parse(dashboardStats.health_start_at)) && Number.isFinite(Date.parse(dashboardStats.health_end_at))
  const healthWindow = dashboardStats && healthDatesValid
    ? `${healthDateFormat.format(new Date(dashboardStats.health_start_at))} – ${healthDateFormat.format(new Date(dashboardStats.health_end_at))}`
    : ''
  const healthWindowDetail = dashboardStats && healthDatesValid
    ? `${new Date(dashboardStats.health_start_at).toLocaleString(i18n.language)} – ${new Date(dashboardStats.health_end_at).toLocaleString(i18n.language)}`
    : undefined

  function showToolFailures(server: string, tool: string, window: { start: string; end: string } | null = null) {
    setErrorServer(server)
    setErrorOrigin('')
    setErrorTool(tool)
    setErrorWindow(window)
    onSectionChange?.('errors')
  }

  const toolGroups = useMemo(() => {
    const keyword = callToolQuery.trim().toLocaleLowerCase()
    const groups = (mcpCallStats?.tools ?? []).filter(tool =>
      !keyword || `${tool.server_name ?? ''} ${tool.name}`.toLocaleLowerCase().includes(keyword))
    return groups.sort((a, b) => {
      if (callToolSort === 'name') return `${a.server_name ?? ''}/${a.name}`.localeCompare(`${b.server_name ?? ''}/${b.name}`)
      if (callToolSort === 'failure') {
        const difference = b.failed / (b.success + b.failed || 1) - a.failed / (a.success + a.failed || 1)
        if (difference !== 0) return difference
      }
      return b.calls - a.calls || `${a.server_name ?? ''}/${a.name}`.localeCompare(`${b.server_name ?? ''}/${b.name}`)
    })
  }, [mcpCallStats, callToolQuery, callToolSort])
  const toolPages = Math.max(1, Math.ceil(toolGroups.length / 25))
  const toolPage = Math.min(callToolPage, toolPages - 1)
  const visibleToolGroups = toolGroups.slice(toolPage * 25, (toolPage + 1) * 25)

  const statusOf = useCallback((name: string, agent: string) =>
    mcpStatuses.find((s) => s.name === name && s.agent === agent), [mcpStatuses])
  const observationOf = useCallback((name: string, agent: string) =>
    mcpObservationStatuses.find((status) => status.name === name && status.agent === (agent === 'codex_desktop' ? 'codex_cli' : agent)), [mcpObservationStatuses])
  const dshBridgeInstalled = dshBridge?.installed ?? false
  const dshObservationEnabled = dshBridgeInstalled && (dshBridge?.features.observation ?? true)

  async function changeAssignment(entry: McpCatalogEntry, agent: string, assigned: boolean, repair = false) {
    setBusyAssignment(`${entry.name}:${agent}`)
    try {
      if (assigned) {
        const observed = observationOf(entry.name, agent)
        if (observed && !['off', 'unsupported'].includes(observed.state)) {
          await setMcpObservation(entry.name, agent, false)
        }
        if (repair) await useMcpStore.getState().equipMcp(entry.name, agent)
        else await useMcpStore.getState().unequipMcp(entry.name, agent)
      } else {
        await useMcpStore.getState().equipMcp(entry.name, agent)
      }
      setNotice(t(assigned && !repair ? 'mcpLibrary.oneClickUninstalled' : 'mcpLibrary.oneClickConfigured', {
        name: entry.name, agent: targetOf(agent)?.label ?? agent,
      }))
    } catch (error) {
      setObservationNotice({ tone: 'error', message: String(error) })
    } finally {
      setBusyAssignment(null)
    }
  }

  async function toggleObservation(name: string, agent: string, enabled: boolean) {
    setBusyObservation(`${name}:${agent === 'codex_desktop' ? 'codex_cli' : agent}`)
    setObservationNotice(null)
    try {
      const result = await setMcpObservation(name, agent, enabled)
      const healthy = enabled
        ? ['awaiting_connection', 'observing', 'fallback'].includes(result.state)
        : result.state === 'off'
      setObservationNotice({
        tone: !healthy ? 'error' : result.state === 'fallback' ? 'warning' : 'success',
        message: healthy
          ? t(enabled && result.state === 'fallback' ? 'mcpLibrary.observationFallbackNotice' : enabled ? 'mcpLibrary.observationEnabledNotice' : 'mcpLibrary.observationDisabledNotice', {
              agent: targetOf(agent)?.label ?? agent,
              state: t(`mcpLibrary.observationState.${result.state}`),
            })
          : t('mcpLibrary.observationIncomplete', { state: t(`mcpLibrary.observationState.${result.state}`) }),
      })
    } catch (error) {
      setObservationNotice({ tone: 'error', message: t('mcpLibrary.observationFailed', { error: String(error) }) })
      await loadMcpObservationStatuses().catch(() => {})
    } finally {
      setBusyObservation(null)
    }
  }

  async function toggleDshObservation(enabled: boolean) {
    setBusyDshObservation(true)
    setObservationNotice(null)
    try {
      if (!dshBridgeInstalled) {
        // 未安装时开启观测 = 一键接入整个桥接插件；关闭则是天然无操作。
        if (enabled) await setDshBridge(true)
      } else {
        const features = dshBridge?.features ?? { injection: true, observation: true, usage: true, collect: true, tools: true }
        await setDshBridgeFeatures({ ...features, observation: enabled })
      }
    } catch (error) {
      setObservationNotice({ tone: 'error', message: t('mcpLibrary.dshPluginFailed', { error: String(error) }) })
    } finally {
      setBusyDshObservation(false)
      await loadDshBridge().catch(() => {})
    }
  }

  async function toggleDshBridgeInstalled(enabled: boolean) {
    setBusyDshObservation(true)
    setObservationNotice(null)
    try {
      await setDshBridge(enabled)
    } catch (error) {
      setObservationNotice({ tone: 'error', message: t('mcpLibrary.dshPluginFailed', { error: String(error) }) })
    } finally {
      setBusyDshObservation(false)
      await loadDshBridge().catch(() => {})
    }
  }

  async function toggleDshBridgeFeature(feature: 'injection' | 'observation' | 'usage' | 'collect' | 'tools', enabled: boolean) {
    if (!dshBridgeInstalled) return
    setBusyDshObservation(true)
    setObservationNotice(null)
    try {
      const features = dshBridge?.features ?? { injection: true, observation: true, usage: true, collect: true, tools: true }
      await setDshBridgeFeatures({ ...features, [feature]: enabled })
    } catch (error) {
      setObservationNotice({ tone: 'error', message: t('mcpLibrary.dshPluginFailed', { error: String(error) }) })
    } finally {
      setBusyDshObservation(false)
      await loadDshBridge().catch(() => {})
    }
  }

  async function discoverTools(entry: McpCatalogEntry) {
    setToolDiscovery(current => ({ ...current, [entry.name]: { loading: true, error: '', result: current[entry.name]?.result ?? null } }))
    try {
      const result = await mcpApi.mcpDiscoverTools({ name: entry.name })
      setToolDiscovery(current => ({ ...current, [entry.name]: { loading: false, error: '', result } }))
    } catch (error) {
      setToolDiscovery(current => ({ ...current, [entry.name]: { loading: false, error: String(error), result: null } }))
    }
  }

  async function handleDelete(entry: McpCatalogEntry) {
    if (!window.confirm(t('mcpLibrary.deleteConfirm', { name: entry.name }))) return
    setLoading(true)
    try {
      await deleteMcpEntry(entry.name)
      setNotice(t('mcpLibrary.deleteDone', { name: entry.name }))
    } catch (error) {
      setNotice(t('mcpLibrary.deleteFailed', { error: String(error) }))
    } finally {
      setLoading(false)
    }
  }

  // 帮助说明是静态文档：不依赖目录加载状态，因此排在 booting / loadError 之前。
  const helpSections = useMemo(() => HELP_TOPICS.map(topic => ({
    id: topic,
    title: t(`mcpLibrary.help.${topic}.title`),
    body: t(`mcpLibrary.help.${topic}.body`),
  })), [t])

  if (section === 'help') {
    return (
      <HelpDocument
        domain={t('nav.mcpLibrary')}
        title={t('nav.mcpHelp')}
        summary={t('mcpLibrary.helpSubtitle')}
        sections={helpSections}
      />
    )
  }

  if (booting) {
    return (
      <div className="flex h-full items-center justify-center gap-2 text-sm text-gray-500 dark:text-gray-400">
        <Loader2 size={16} className="animate-spin" />{t('mcpLibrary.booting')}
      </div>
    )
  }

  // Keep the same keyed child under the same root in both views so a catalog
  // failure/recovery cannot discard the selector's draft, errors or saved state.
  const configSelector = (
    <ClaudeDesktopConfigSelector
      key="claude-desktop-config"
      disabled={loading || operationOpen}
      onBusyChange={setConfigBusy}
      onChanged={refreshAfterConfigChange}
    />
  )

  const statusCard = (label: string, value: string | number, hint?: string, tone?: 'warn', hintTitle?: string) => (
    <div className="min-w-0 py-2 sm:px-4 sm:first:pl-0">
      <div className="flex items-baseline gap-2">
        <span className={`text-lg font-semibold tabular-nums ${tone === 'warn' ? 'text-amber-700 dark:text-amber-300' : 'text-gray-900 dark:text-gray-100'}`}>{value}</span>
        <span className="text-xs font-medium text-gray-600 dark:text-gray-300">{label}</span>
      </div>
      {hint && <p className="mt-0.5 break-words text-[11px] leading-5 text-gray-500 dark:text-gray-400" title={hintTitle ?? hint}>{hint}</p>}
    </div>
  )

  if (equipmentOpen) return <EquipmentProfiles onBack={() => { setEquipmentOpen(false); void refreshAll() }} />

  return (
    <div className="flex h-full min-h-0 flex-col">
      {/* header */}
      <div className="fluent-page-header fluent-command-header flex flex-wrap items-center gap-3 border-b border-gray-200 dark:border-gray-800">
        <div className="mr-auto flex min-w-0 items-center gap-4">
          <div className="min-w-0">
            <h1 className="flex items-center gap-2 text-base font-semibold text-gray-900 dark:text-gray-100">
              <Plug size={18} className="text-violet-500" />{t('mcpLibrary.title')}
            </h1>
            <p className="truncate text-xs text-gray-500 dark:text-gray-400">{t('mcpLibrary.subtitle')}</p>
          </div>
        </div>
        <button
          onClick={() => { void refreshAll() }}
          disabled={loading || configBusy}
          className="inline-flex items-center gap-1 rounded-lg border border-gray-300 bg-white px-2.5 py-1.5 text-xs font-medium text-gray-700 transition hover:bg-gray-50 disabled:opacity-50 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-200 dark:hover:bg-gray-700"
        >
          <RefreshCw size={13} className={loading ? 'animate-spin' : ''} />{t('mcpLibrary.refreshStatus')}
        </button>
        {section === 'services' && <EquipmentProfilesButton onClick={() => setEquipmentOpen(true)} />}
        {section === 'services' && !loadError && <>
            <div className="relative">
              <Search size={14} className="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-gray-500" />
              <input
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder={t('mcpLibrary.search')}
                className="w-56 rounded-lg border border-gray-300 bg-white py-1.5 pl-8 pr-3 text-sm text-gray-800 outline-none transition focus:border-violet-400 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100"
              />
            </div>
            <button
              onClick={() => setImportOpen(true)}
              disabled={loading || configBusy}
              className="inline-flex items-center gap-1 rounded-lg border border-gray-300 bg-white px-2.5 py-1.5 text-xs font-medium text-gray-700 transition hover:bg-gray-50 disabled:opacity-50 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-200 dark:hover:bg-gray-700"
            >
              <Download size={13} />{t('mcpLibrary.importFromAgents')}
            </button>
            <button
              onClick={() => { setEditing(null); setAddOpen(true) }}
              disabled={loading || configBusy}
              className="inline-flex items-center gap-1 rounded-lg bg-violet-600 px-2.5 py-1.5 text-xs font-medium text-white transition hover:bg-violet-500 disabled:opacity-50"
            >
              <Plus size={13} />{t('mcpLibrary.add')}
            </button>
        </>}
      </div>

      {!loadError && <>
        <div className="grid grid-cols-1 divide-y divide-gray-200 border-b border-gray-200 px-5 py-2 sm:grid-cols-3 sm:divide-x sm:divide-y-0 dark:divide-gray-800 dark:border-gray-800" aria-label={t('mcpLibrary.statusCards')}>
          {statusCard(t('mcpLibrary.servicesCount'), mcpCatalog.length,
            t('mcpLibrary.serviceLocations', { local: mcpCatalog.length - remoteServices, remote: remoteServices }))}
          {statusCard(t('mcpLibrary.dashboardCalls'),
            dashboardError ? t('mcpLibrary.statusUnavailable') : dashboardStats?.total_calls ?? '—',
            dashboardError ? t('mcpLibrary.dashboardRefreshFailed') : t('mcpLibrary.dashboardCallsHint'))}
          {statusCard(t('mcpLibrary.healthScore'),
            dashboardError ? t('mcpLibrary.statusUnavailable') : healthScore,
            dashboardError ? t('mcpLibrary.dashboardRefreshFailed') : !dashboardStats ? undefined
              : failureRate7d === null ? t('mcpLibrary.healthEmpty', { range: healthWindow })
                : t('mcpLibrary.healthSummary', { range: healthWindow, rate: `${(failureRate7d * 100).toFixed(1)}%`, count: confirmedCalls7d }),
            unhealthyTools.length > 0 ? 'warn' : undefined, healthWindowDetail)}
        </div>
        {unhealthyTools.length > 0 && <details className="group shrink-0 border-b border-gray-200 px-5 text-xs dark:border-gray-800">
          <summary className="flex cursor-pointer list-none items-center gap-2 rounded py-2.5 text-amber-700 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-violet-500 dark:text-amber-300 [&::-webkit-details-marker]:hidden">
            <ChevronRight size={14} aria-hidden="true" className="shrink-0 transition-transform group-open:rotate-90 motion-reduce:transition-none" />
            <span className="min-w-0 flex-1 truncate font-medium" title={[t('mcpLibrary.unhealthyToolsTitle', { range: healthWindow, count: unhealthyTools.length }), healthWindowDetail].filter(Boolean).join(' · ')}>{t('mcpLibrary.unhealthyToolsTitle', { range: healthWindow, count: unhealthyTools.length })}</span>
            <span className="shrink-0 group-open:hidden">{t('mcpLibrary.expandDetails')}</span>
            <span className="hidden shrink-0 group-open:inline">{t('mcpLibrary.collapseDetails')}</span>
          </summary>
          <ul tabIndex={0} aria-label={t('mcpLibrary.unhealthyToolsTitle', { range: healthWindow, count: unhealthyTools.length })} className="mb-3 max-h-40 space-y-1.5 overflow-y-auto rounded focus-visible:outline focus-visible:outline-2 focus-visible:outline-violet-500">
            {unhealthyTools.map(tool => {
              const confirmed = tool.successful_calls + tool.failed_calls
              const rate = `${(tool.failed_calls / confirmed * 100).toFixed(1)}%`
              return <li key={JSON.stringify([tool.server_name, tool.tool_name])} className="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-0.5">
                <button type="button" disabled={!onSectionChange || !healthDatesValid} onClick={() => showToolFailures(tool.server_name, tool.tool_name, dashboardStats ? { start: dashboardStats.health_start_at, end: dashboardStats.health_end_at } : null)}
                  title={t('mcpLibrary.viewToolFailures')} className="min-w-0 rounded text-left text-violet-700 underline underline-offset-2 focus-visible:outline focus-visible:outline-2 focus-visible:outline-violet-500 disabled:text-gray-500 disabled:no-underline dark:text-violet-300">{tool.server_name} / {tool.tool_name}</button>
                <span className="text-amber-700 dark:text-amber-300">{t('mcpLibrary.unhealthyToolRate', { rate, failed: tool.failed_calls, count: confirmed })}</span>
              </li>
            })}
          </ul>
        </details>}
      </>}
      <div hidden={!loadError && section !== 'services'}>{configSelector}</div>

      {loadError && <div role="alert" className="flex min-h-0 flex-1 flex-col items-center justify-center gap-3 overflow-y-auto p-6 text-center">
        <p className="text-sm font-medium text-red-600 dark:text-red-400">{t('mcpLibrary.loadFailed')}</p>
        <p className="max-w-xl break-words text-xs text-gray-500 dark:text-gray-400">{loadError}</p>
        <ErrorRecovery onRetry={() => { void refreshAll() }} disabled={loading || configBusy} />
      </div>}
      {!loadError && section === 'services' && mcpObservationError && (
        <div role="alert" className="border-b border-red-200 bg-red-50 px-5 py-2 text-xs text-red-600 dark:border-red-800 dark:bg-red-500/10 dark:text-red-300">
          {t('mcpLibrary.observationLoadFailed', { error: mcpObservationError })}
        </div>
      )}
      {!loadError && section === 'services' && observationNotice && (
        <div role={observationNotice.tone === 'error' ? 'alert' : 'status'} className={`border-b px-5 py-2 text-xs ${observationNotice.tone === 'error' ? 'border-red-200 bg-red-50 text-red-600 dark:border-red-800 dark:bg-red-500/10 dark:text-red-300' : observationNotice.tone === 'warning' ? 'border-amber-200 bg-amber-50 text-amber-800 dark:border-amber-800 dark:bg-amber-500/10 dark:text-amber-300' : 'border-emerald-200 bg-emerald-50 text-emerald-700 dark:border-emerald-500/20 dark:bg-emerald-500/10 dark:text-emerald-300'}`}>
          {observationNotice.message}
        </div>
      )}
      {!loadError && statusError && (
        <div role="alert" className="border-b border-red-200 bg-red-50 px-5 py-3 text-xs text-red-600 dark:border-red-800 dark:bg-red-500/10 dark:text-red-300">
          {t('mcpLibrary.statusFailed', { error: statusError })}
          <ErrorRecovery onRetry={() => { void refreshAll() }} disabled={loading || configBusy} />
        </div>
      )}
      {!loadError && section === 'services' && migrationNotices.length > 0 && (
        <div role="status" className="border-b border-amber-200 bg-amber-50 px-5 py-3 text-xs leading-5 text-amber-800 dark:border-amber-800 dark:bg-amber-500/10 dark:text-amber-300">
          <p className="font-medium">{t('mcpLibrary.migrationNotice')}</p>
          {migrationNotices.map((message, index) => <p key={index} className="break-words">{message}</p>)}
        </div>
      )}
      {!loadError && notice && (
        <div className="flex items-center gap-2 border-b border-emerald-200 bg-emerald-50 px-5 py-2 text-xs text-emerald-700 dark:border-emerald-500/20 dark:bg-emerald-500/10 dark:text-emerald-300">
          <Check size={12} className="shrink-0" />
          <span className="min-w-0 flex-1 break-all">{notice}</span>
          <button onClick={() => setNotice('')} className="shrink-0 text-emerald-500 transition hover:text-emerald-700 dark:hover:text-emerald-300" aria-label="dismiss"><X size={12} /></button>
        </div>
      )}

      {!loadError && section === 'services' && <>
      <div className="min-h-0 flex-1 overflow-y-auto p-5">
        {/* DeepSeek Harness 统一桥接插件：注入/观测/用量/收集/原生工具 */}
        <article className="mb-3 rounded-xl border border-gray-200 bg-white p-4 transition hover:border-violet-300 dark:border-gray-700 dark:bg-gray-900 dark:hover:border-violet-500/40">
          <div className="flex flex-wrap items-start gap-3">
            <div className="rounded-lg bg-violet-500/10 p-2 text-violet-600 dark:text-violet-400">
              <Activity size={16} />
            </div>
            <div className="min-w-0 flex-1">
              <div className="flex items-center gap-2">
                <h3 className="min-w-0 truncate text-sm font-semibold text-gray-900 dark:text-gray-100">{t('mcpLibrary.dshPluginTitle')}</h3>
                <span className={`inline-flex shrink-0 items-center gap-1 rounded-full border px-2 py-0.5 text-[11px] font-medium ${dshBridgeInstalled ? 'border-emerald-500/50 bg-emerald-50 text-emerald-700 dark:border-emerald-500/30 dark:bg-emerald-500/10 dark:text-emerald-300' : 'border-gray-300 bg-gray-50 text-gray-500 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-400'}`}
                  title={dshBridge?.detail ?? ''}>
                  <span className={`h-1.5 w-1.5 rounded-full ${dshBridgeInstalled ? 'bg-emerald-500' : 'bg-gray-400'}`} />
                  {dshBridgeInstalled ? t('mcpLibrary.dshBridgeVersion', { version: dshBridge?.version ?? '?' }) : t('mcpLibrary.dshBridgeMissing')}
                </span>
              </div>
              <p className="mt-1 text-xs leading-5 text-gray-500 dark:text-gray-400">{t('mcpLibrary.dshPluginHint')}</p>
              {dshBridgeInstalled && (dshBridge?.legacy_memory_inject || dshBridge?.legacy_mcp_observe) && (
                <p className="mt-1 break-words text-xs text-amber-600 dark:text-amber-400">{t('mcpLibrary.dshBridgeLegacy')}</p>
              )}
              {dshBridgeError && <p role="alert" className="mt-1 break-words text-xs text-red-600 dark:text-red-400">{t('mcpLibrary.dshPluginFailed', { error: dshBridgeError })}</p>}
            </div>
            <button type="button" onClick={() => { void toggleDshBridgeInstalled(!dshBridgeInstalled) }} disabled={busyDshObservation || loading || configBusy}
              className="shrink-0 rounded-md border border-gray-300 px-2.5 py-1.5 text-xs font-medium text-gray-700 transition hover:bg-gray-50 disabled:opacity-50 dark:border-gray-600 dark:text-gray-200 dark:hover:bg-gray-800">
              {busyDshObservation ? <Loader2 size={12} className="animate-spin" /> : t(dshBridgeInstalled ? 'mcpLibrary.dshBridgeRemove' : 'mcpLibrary.dshBridgeInstall')}
            </button>
          </div>
          {dshBridgeInstalled && (
            <div className="mt-3 flex flex-wrap items-center gap-x-4 gap-y-2 border-t border-gray-100 pt-3 dark:border-gray-800">
              <span className="text-[11px] font-medium text-gray-500 dark:text-gray-400">{t('mcpLibrary.dshBridgeFeatures')}</span>
              {([['injection', 'dshBridgeFeatureInjection'], ['observation', 'dshBridgeFeatureObservation'], ['usage', 'dshBridgeFeatureUsage'], ['collect', 'dshBridgeFeatureCollect'], ['tools', 'dshBridgeFeatureTools']] as const).map(([feature, key]) => {
                const enabled = dshBridge?.features[feature] ?? true
                return (
                  <label key={feature} className="inline-flex cursor-pointer items-center gap-1.5 text-xs text-gray-700 dark:text-gray-300">
                    <input
                      type="checkbox"
                      checked={enabled}
                      disabled={busyDshObservation || loading || configBusy}
                      onChange={(event) => { void toggleDshBridgeFeature(feature, event.target.checked) }}
                      className="h-3.5 w-3.5 accent-violet-600"
                    />
                    {t(`mcpLibrary.${key}`)}
                  </label>
                )
              })}
            </div>
          )}
        </article>
        {/* 内置共享记忆 MCP：与其他服务相同的条目展示 */}
        <article className="mb-3 rounded-xl border border-gray-200 bg-white p-4 transition hover:border-violet-300 dark:border-gray-700 dark:bg-gray-900 dark:hover:border-violet-500/40">
          <div className="flex items-start gap-3">
            <div className="rounded-lg bg-indigo-500/10 p-2 text-indigo-600 dark:text-indigo-400">
              <Terminal size={16} />
            </div>
            <div className="min-w-0 flex-1">
              <div className="flex items-center gap-2">
                <h3 className="min-w-0 truncate font-mono text-sm font-semibold text-gray-900 dark:text-gray-100">Vibe-Assistant-memory</h3>
                <span className="shrink-0 rounded-full bg-indigo-500/10 px-1.5 py-0.5 text-[10px] font-medium text-indigo-600 dark:text-indigo-300">{t('mcpLibrary.builtinBadge')}</span>
                <span className="shrink-0 rounded-full bg-gray-100 px-1.5 py-0.5 text-[10px] font-medium text-gray-600 dark:bg-gray-800 dark:text-gray-300">stdio</span>
              </div>
              <p className="mt-0.5 truncate text-xs text-gray-500 dark:text-gray-400">{t('mcpLibrary.builtinMemoryMcpDesc')}</p>
            </div>
          </div>
          <div className="mt-3 flex flex-wrap gap-1.5">
            {TARGETS.map((target) => {
              const status = memoryMcp[target.id as keyof typeof memoryMcp]
              const runtime = memoryMcpRuntime[target.id as keyof typeof memoryMcpRuntime]
              const configured = status?.installed ?? false
              const lastActivity = runtime?.last_tool_call_at ?? runtime?.last_initialize_at
              const recentActivity = Boolean(lastActivity && Date.now() - Date.parse(lastActivity) <= 5 * 60 * 1000 && Date.now() >= Date.parse(lastActivity))
              const tone = !configured
                ? 'border-gray-300 bg-gray-50 text-gray-500 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-400'
                : recentActivity
                  ? 'border-sky-500/50 bg-sky-50 text-sky-700 dark:border-sky-500/30 dark:bg-sky-500/10 dark:text-sky-300'
                  : 'border-emerald-500/50 bg-emerald-50 text-emerald-700 dark:border-emerald-500/30 dark:bg-emerald-500/10 dark:text-emerald-300'
              const dot = !configured ? 'bg-gray-400' : recentActivity ? 'bg-sky-500' : 'bg-emerald-500'
              return (
                <span key={target.id} className={`inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-[11px] font-medium ${tone}`}>
                  <span className={`h-1.5 w-1.5 rounded-full ${dot}`} />
                  {target.label} · {t(configured ? 'mcpLibrary.stateInstalled' : 'mcpLibrary.stateMissing')}
                </span>
              )
            })}
          </div>
          <div className="mt-3 flex items-center gap-1.5">
            <button type="button" onClick={() => setExpandedObservations(current => current.includes('Vibe-Assistant-memory') ? current.filter(name => name !== 'Vibe-Assistant-memory') : [...current, 'Vibe-Assistant-memory'])}
              aria-expanded={expandedObservations.includes('Vibe-Assistant-memory')}
              className="inline-flex items-center gap-1 rounded-md border border-gray-300 px-2 py-1 text-[11px] font-medium text-gray-700 transition hover:bg-gray-50 dark:border-gray-600 dark:text-gray-200 dark:hover:bg-gray-800">
              {t(expandedObservations.includes('Vibe-Assistant-memory') ? 'mcpLibrary.observationCollapse' : 'mcpLibrary.manageAgents')}
            </button>
            <button type="button" onClick={async () => { await Promise.all([checkMemoryMcp(), checkMemoryMcpRuntime()]) }} disabled={loading}
              className="inline-flex items-center gap-1 rounded-md border border-gray-300 px-2 py-1 text-[11px] font-medium text-gray-700 transition hover:bg-gray-50 disabled:opacity-50 dark:border-gray-600 dark:text-gray-200 dark:hover:bg-gray-800">
              <RefreshCw size={11} className={loading ? 'animate-spin' : ''} />{t('common.refresh')}
            </button>
          </div>
          {expandedObservations.includes('Vibe-Assistant-memory') && <div className="mt-3 border-t border-gray-100 pt-2 dark:border-gray-800">
            <div className="space-y-2">
              {TARGETS.map((target) => {
                const status = memoryMcp[target.id as keyof typeof memoryMcp]
                const runtime = memoryMcpRuntime[target.id as keyof typeof memoryMcpRuntime]
                const configured = status?.installed ?? false
                const targetId = target.id as MemoryMcpTarget
                const working = memoryMcpAction === targetId
                const title = [
                  status?.detail,
                  runtime?.last_initialize_at ? t('mcpLibrary.memoryMcpLastInitialize', { time: new Date(runtime.last_initialize_at).toLocaleString() }) : t('mcpLibrary.memoryMcpNeverObserved'),
                  runtime?.last_tool_call_at ? t('mcpLibrary.memoryMcpLastToolCall', { time: new Date(runtime.last_tool_call_at).toLocaleString() }) : '',
                  runtime?.probe_state === 'not_probed' ? t('mcpLibrary.memoryMcpProbeNotRun') : '',
                ].filter(Boolean).join(' · ')
                return (
                  <div key={target.id} className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs" title={title}>
                    <span className="min-w-24 text-gray-700 dark:text-gray-300">{target.label}</span>
                    <span className={configured ? 'text-emerald-700 dark:text-emerald-300' : 'text-gray-500 dark:text-gray-400'}>
                      {t(configured ? 'mcpLibrary.stateInstalled' : 'mcpLibrary.memoryMcpNotConfigured')}
                    </span>
                    <button type="button" disabled={memoryMcpAction !== null || loading} onClick={async () => {
                      setMemoryMcpAction(targetId)
                      setMemoryMcpActionError('')
                      try {
                        if (configured) await uninstallMemoryMcp(targetId)
                        else await installMemoryMcp(targetId)
                        await Promise.all([checkMemoryMcp(), checkMemoryMcpRuntime()])
                        setNotice(t(configured ? 'mcpLibrary.memoryMcpRemovedToast' : 'mcpLibrary.memoryMcpInstalledToast', { label: target.label }))
                      } catch (error) { setMemoryMcpActionError(String(error)) }
                      finally { setMemoryMcpAction(null) }
                    }} className="ml-auto shrink-0 rounded-md border border-gray-300 px-2 py-1 text-[11px] font-medium text-gray-700 transition hover:bg-gray-50 disabled:opacity-50 dark:border-gray-600 dark:text-gray-200 dark:hover:bg-gray-800">{working ? <Loader2 size={12} className="animate-spin motion-reduce:animate-none" /> : t(configured ? 'mcpLibrary.memoryMcpRemove' : 'mcpLibrary.memoryMcpInstall')}</button>
                  </div>
                )
              })}
            </div>
            {memoryMcpActionError && <p role="alert" className="mt-2 text-xs text-red-600 dark:text-red-400">{t('mcpLibrary.memoryMcpActionFailed', { error: memoryMcpActionError })}</p>}
            {memoryMcpRuntimeError && <p className="mt-1 text-[11px] text-red-600 dark:text-red-400">{t('mcpLibrary.memoryMcpRuntimeError', { error: memoryMcpRuntimeError })}</p>}
          </div>}
        </article>
        {visible.length === 0 ? (
          <div className="mx-auto max-w-md py-16 text-center">
            <div className="mx-auto mb-3 flex h-14 w-14 items-center justify-center rounded-2xl bg-violet-100 dark:bg-violet-900/30">
              <Plug size={24} className="text-violet-600 dark:text-violet-400" />
            </div>
            <p className="text-sm font-medium text-gray-700 dark:text-gray-200">{t(mcpCatalog.length === 0 ? 'mcpLibrary.emptyTitle' : 'mcpLibrary.filteredEmptyTitle')}</p>
            <p className="mt-1 text-xs leading-5 text-gray-500 dark:text-gray-400">{t(mcpCatalog.length === 0 ? 'mcpLibrary.emptyHint' : 'mcpLibrary.filteredEmptyHint')}</p>
          </div>
        ) : (
          <div className="space-y-3">
            {visible.map((entry) => {
              const isRemote = entry.transport === 'sse' || entry.transport === 'http'
              return (
                <article key={entry.name} className="rounded-xl border border-gray-200 bg-white p-4 transition hover:border-violet-300 dark:border-gray-700 dark:bg-gray-900 dark:hover:border-violet-500/40">
                  <div className="flex items-start gap-3">
                    <div className={`rounded-lg p-2 ${isRemote ? 'bg-sky-500/10 text-sky-600 dark:text-sky-400' : 'bg-violet-500/10 text-violet-600 dark:text-violet-400'}`}>
                      {isRemote ? <Globe size={16} /> : <Terminal size={16} />}
                    </div>
                    <div className="min-w-0 flex-1">
                      <div className="flex items-center gap-2">
                        <h3 className="min-w-0 truncate font-mono text-sm font-semibold text-gray-900 dark:text-gray-100">{entry.name}</h3>
                        <span className="shrink-0 rounded-full bg-gray-100 px-1.5 py-0.5 text-[10px] font-medium text-gray-600 dark:bg-gray-800 dark:text-gray-300">
                          {entry.transport === 'stdio' ? 'stdio' : entry.transport.toUpperCase()}
                        </span>
                      </div>
                      <p className="mt-0.5 truncate text-xs text-gray-500 dark:text-gray-400">{entry.description || t('mcpLibrary.noDescription')}</p>
                    </div>
                  </div>
                  {/* per-agent chips */}
                  <div className="mt-3 flex flex-wrap gap-1.5">
                    {TARGETS.map((target) => {
                      const assigned = entry.assigned_agents.includes(target.id)
                      const status = mcpStatusState === 'ready' ? statusOf(entry.name, target.id) : undefined
                      if (!assigned && !status) return null
                      const state = status?.state
                      const tone = state === 'installed'
                        ? 'border-emerald-500/50 bg-emerald-50 text-emerald-700 dark:border-emerald-500/30 dark:bg-emerald-500/10 dark:text-emerald-300'
                        : state === 'differs'
                          ? 'border-amber-500/50 bg-amber-50 text-amber-700 dark:border-amber-500/30 dark:bg-amber-500/10 dark:text-amber-300'
                          : 'border-gray-300 bg-gray-50 text-gray-500 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-400'
                      const dot = state === 'installed' ? 'bg-emerald-500' : state === 'differs' ? 'bg-amber-500' : 'bg-gray-400'
                      return (
                        <span key={target.id} className={`inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-[11px] font-medium ${tone}`}>
                          <span className={`h-1.5 w-1.5 rounded-full ${dot}`} />
                          {target.label}{!assigned && state !== 'unavailable' && ` · ${t('mcpLibrary.discoveredConfig')}`} · {t(state === 'installed' ? 'mcpLibrary.stateInstalled' : state === 'missing' ? 'mcpLibrary.stateMissing' : state === 'differs' ? 'mcpLibrary.stateDiffers' : 'mcpLibrary.statusUnavailable')}
                        </span>
                      )
                    })}
                    {entry.assigned_agents.length === 0 && (
                      <span className="text-[10px] text-gray-500 dark:text-gray-400">{t('mcpLibrary.notEquipped')}</span>
                    )}
                  </div>
                  {mcpStatusState === 'ready' && mcpStatuses.some(status => status.name === entry.name && (status.state === 'differs' || status.error)) && (
                    <details className="mt-3 text-xs text-gray-600 dark:text-gray-300">
                      <summary className="cursor-pointer rounded py-1 font-medium focus-visible:outline focus-visible:outline-2 focus-visible:outline-violet-500">{t('mcpLibrary.configurationDetails')}</summary>
                      <p className="mt-1 leading-5">{t('mcpLibrary.configurationBaseline')}</p>
                      <ul className="mt-2 space-y-1">
                        {mcpStatuses.filter(status => status.name === entry.name && (status.state === 'differs' || status.error)).map(status => (
                          <li key={status.agent} className="break-words text-amber-800 dark:text-amber-300">
                            {targetOf(status.agent)?.label ?? status.agent} · {status.error ?? t('mcpLibrary.differingFields', { fields: status.differing_fields.map(field => t(`mcpLibrary.configFields.${field}`, { defaultValue: field })).join(' / ') })}
                          </li>
                        ))}
                      </ul>
                    </details>
                  )}
                  {entry.transport === 'stdio' && entry.assigned_agents.length > 0 && (
                    <div className="mt-3 rounded-lg border border-gray-100 px-3 py-2 dark:border-gray-800">
                      <div className="flex flex-wrap items-center gap-2 text-xs">
                        <Activity size={14} className="text-violet-600 dark:text-violet-400" />
                        <span className="font-medium text-gray-800 dark:text-gray-200">{t('mcpLibrary.observationCardTitle')}</span>
                        {TARGETS.filter(target => entry.assigned_agents.includes(target.id)).map(target => {
                          const observation = observationOf(entry.name, target.id)
                          const state = mcpObservationError ? undefined : observation?.state === 'observing' && observation.active_connections === 0 ? 'awaiting_connection' : observation?.state
                          const pluginObserved = target.id === 'dsh'
                          const pluginState = t(dshObservationEnabled ? 'mcpLibrary.dshPluginOn' : 'mcpLibrary.dshPluginOff')
                          return <span key={target.id} className={pluginObserved ? (dshObservationEnabled ? 'text-emerald-700 dark:text-emerald-300' : 'text-gray-500 dark:text-gray-400') : state === 'drifted' || state === 'recovery' || state === 'fallback' ? 'text-amber-700 dark:text-amber-300' : state === 'observing' ? 'text-emerald-700 dark:text-emerald-300' : 'text-gray-500 dark:text-gray-400'}>
                            {target.label} · {pluginObserved ? t('mcpLibrary.dshPluginInline', { state: pluginState }) : state ? t(`mcpLibrary.observationState.${state}`) : t('mcpLibrary.observationUnavailable')}
                            {!pluginObserved && state === 'drifted' && <button type="button" disabled={busyObservation !== null || loading || configBusy || !!mcpObservationError} onClick={() => { void toggleObservation(entry.name, target.id, true) }} className="ml-1 rounded border border-amber-400 px-1.5 text-amber-800 disabled:opacity-50 dark:text-amber-300">{t('mcpLibrary.observationRetry')}</button>}
                          </span>
                        })}
                        <button type="button" onClick={() => setExpandedObservations(current => current.includes(entry.name) ? current.filter(name => name !== entry.name) : [...current, entry.name])} aria-expanded={expandedObservations.includes(entry.name)} className="ml-auto text-violet-700 dark:text-violet-300">{t(expandedObservations.includes(entry.name) ? 'mcpLibrary.observationCollapse' : 'mcpLibrary.observationManage')}</button>
                      </div>
                      {expandedObservations.includes(entry.name) && <div className="mt-3 border-t border-gray-100 pt-2 dark:border-gray-800">
                        <div className="space-y-2">
                          {TARGETS.filter(target => entry.assigned_agents.includes(target.id)).map(target => {
                            const observation = mcpObservationError ? undefined : observationOf(entry.name, target.id)
                            const state = observation?.state === 'observing' && observation.active_connections === 0 ? 'awaiting_connection' : observation?.state
                            const enabled = state !== undefined && state !== 'off' && state !== 'unsupported'
                            const busy = busyObservation === `${entry.name}:${target.id === 'codex_desktop' ? 'codex_cli' : target.id}`
                            if (target.id === 'dsh') {
                              return (
                                <div key={target.id} className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs">
                                  <span className="min-w-24 text-gray-700 dark:text-gray-300">{target.label}</span>
                                  <span className={dshObservationEnabled ? 'text-emerald-700 dark:text-emerald-300' : 'text-gray-500 dark:text-gray-400'}>{t('mcpLibrary.dshPluginInline', { state: t(dshObservationEnabled ? 'mcpLibrary.dshPluginOn' : 'mcpLibrary.dshPluginOff') })}</span>
                                  <span className="text-gray-500 dark:text-gray-400">{t('mcpLibrary.dshPluginManaged')}</span>
                                  <button type="button" disabled={busyDshObservation || busyObservation !== null || loading || configBusy} onClick={() => { void toggleDshObservation(!dshObservationEnabled) }} className="rounded border border-violet-300 px-1.5 text-violet-700 disabled:opacity-50 dark:border-violet-500/40 dark:text-violet-300">{t(dshObservationEnabled ? 'mcpLibrary.dshPluginDisable' : 'mcpLibrary.dshPluginEnable')}</button>
                                  {busyDshObservation && <Loader2 size={12} className="animate-spin text-violet-500" />}
                                </div>
                              )
                            }
                            return (
                              <div key={target.id} className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs">
                                <span className="min-w-24 text-gray-700 dark:text-gray-300">{target.label}</span>
                                <label className="inline-flex cursor-pointer items-center gap-2 text-gray-700 dark:text-gray-300">
                                  <input
                                    type="checkbox"
                                    checked={enabled}
                                    disabled={busyObservation !== null || loading || configBusy || !!mcpObservationError || state === undefined || state === 'unsupported'}
                                    onChange={event => { void toggleObservation(entry.name, target.id, event.target.checked) }}
                                    className="h-4 w-4 cursor-pointer accent-violet-600 disabled:cursor-not-allowed"
                                    aria-label={t('mcpLibrary.observationToggleFor', { agent: target.label })}
                                  />
                                  {t('mcpLibrary.observationToggle')}
                                </label>
                                <span className={state === 'observing' ? 'text-emerald-700 dark:text-emerald-300' : state === 'drifted' || state === 'recovery' || state === 'fallback' ? 'text-amber-700 dark:text-amber-300' : 'text-gray-500 dark:text-gray-400'}>
                                  {state ? t(`mcpLibrary.observationState.${state}`) : t('mcpLibrary.observationUnavailable')}
                                </span>
                                {observation && observation.active_connections > 0 && <span className="text-gray-500 dark:text-gray-400">{t('mcpLibrary.observationConnections', { count: observation.active_connections })}</span>}
                                {busy && <Loader2 size={12} className="animate-spin text-violet-500" />}
                              </div>
                            )
                          })}
                        </div>
                      </div>}
                    </div>
                  )}
                  {/* actions */}
                  <div className="mt-3 flex items-center gap-1.5">
                    <button type="button" onClick={() => setExpandedAssignments(current => current.includes(entry.name) ? current.filter(name => name !== entry.name) : [...current, entry.name])}
                      aria-expanded={expandedAssignments.includes(entry.name)}
                      className="inline-flex items-center gap-1 rounded-md border border-gray-300 px-2 py-1 text-[11px] font-medium text-gray-700 transition hover:bg-gray-50 focus-visible:outline focus-visible:outline-2 focus-visible:outline-violet-500 dark:border-gray-600 dark:text-gray-200 dark:hover:bg-gray-800">
                      {t('mcpLibrary.manageAgents')}
                    </button>
                    <button type="button" onClick={() => setArbitrating({ name: entry.name })} disabled={loading || configBusy} className="rounded-md border border-gray-300 px-2 py-1 text-[11px] font-medium text-gray-700 hover:bg-gray-50 focus-visible:outline focus-visible:outline-2 focus-visible:outline-violet-500 disabled:opacity-50 dark:border-gray-600 dark:text-gray-200 dark:hover:bg-gray-800">{t('mcpLibrary.arbitration.open')}</button>
                    <button onClick={() => setEquipping(entry)} disabled={loading || configBusy} className="inline-flex items-center gap-1 rounded-md bg-violet-600 px-2 py-1 text-[11px] font-medium text-white transition hover:bg-violet-500 disabled:opacity-50">
                      <Plug size={11} />{t('mcpLibrary.equip')}
                    </button>
                    <button onClick={() => { setToolService(entry.name); onSectionChange?.('tools'); void discoverTools(entry) }} disabled={loading || configBusy} className="inline-flex items-center gap-1 rounded-md border border-gray-300 px-2 py-1 text-[11px] font-medium text-gray-600 transition hover:bg-gray-50 disabled:opacity-50 dark:border-gray-600 dark:text-gray-300 dark:hover:bg-gray-800">
                      <Activity size={11} />{t('mcpLibrary.discoverTools')}
                    </button>
                    <button onClick={() => { setEditing(entry); setAddOpen(true) }} disabled={loading || configBusy} className="inline-flex items-center gap-1 rounded-md border border-gray-300 px-2 py-1 text-[11px] font-medium text-gray-600 transition hover:bg-gray-50 disabled:opacity-50 dark:border-gray-600 dark:text-gray-300 dark:hover:bg-gray-800">
                      <Pencil size={11} />{t('mcpLibrary.edit')}
                    </button>
                    <button onClick={() => { void handleDelete(entry) }} disabled={loading || configBusy} className="ml-auto inline-flex items-center gap-1 rounded-md px-2 py-1 text-[11px] font-medium text-gray-500 transition hover:text-red-600 dark:hover:text-red-400 disabled:opacity-50">
                      <Trash2 size={11} />{t('mcpLibrary.delete')}
                    </button>
                  </div>
                  {expandedAssignments.includes(entry.name) && <div className="mt-3 divide-y divide-gray-100 rounded-lg border border-gray-200 dark:divide-gray-800 dark:border-gray-700">
                    {TARGETS.map(target => {
                      const assigned = entry.assigned_agents.includes(target.id)
                      const unsupported = isRemote && !target.remote
                      const state = mcpStatusState === 'ready' ? statusOf(entry.name, target.id)?.state : undefined
                      const needsRepair = assigned && (state === 'missing' || state === 'differs')
                      const busy = busyAssignment === `${entry.name}:${target.id}`
                      return <div key={target.id} className="flex flex-wrap items-center gap-2 px-3 py-2 text-xs">
                        <span className="min-w-28 font-medium text-gray-800 dark:text-gray-200">{target.label}</span>
                        <span className="text-gray-500 dark:text-gray-400">{unsupported ? t('mcpLibrary.remoteUnsupported') : state ? `${!assigned && state !== 'unavailable' ? `${t('mcpLibrary.discoveredConfig')} · ` : ''}${t(state === 'installed' ? 'mcpLibrary.stateInstalled' : state === 'differs' ? 'mcpLibrary.stateDiffers' : state === 'missing' ? 'mcpLibrary.stateMissing' : 'mcpLibrary.statusUnavailable')}` : assigned ? t('mcpLibrary.statusUnavailable') : t('mcpLibrary.notEquipped')}</span>
                        {needsRepair && <button type="button" onClick={() => { void changeAssignment(entry, target.id, true) }}
                          disabled={busyAssignment !== null || busyObservation !== null || loading || configBusy || !!mcpObservationError}
                          className="ml-auto rounded-md px-2.5 py-1.5 font-medium text-red-700 transition hover:bg-red-50 focus-visible:outline focus-visible:outline-2 focus-visible:outline-red-500 disabled:opacity-50 dark:text-red-300 dark:hover:bg-red-500/10">
                          {t('mcpLibrary.oneClickUninstall')}
                        </button>}
                        {!unsupported && <button type="button" onClick={() => { if (state === 'differs') setArbitrating({ name: entry.name, agent: target.id === 'codex_desktop' ? 'codex_cli' : target.id }); else void changeAssignment(entry, target.id, assigned, needsRepair) }}
                          disabled={busyAssignment !== null || busyObservation !== null || loading || configBusy || (assigned && !!mcpObservationError)}
                          className={`ml-auto rounded-md px-2.5 py-1.5 font-medium transition focus-visible:outline focus-visible:outline-2 focus-visible:outline-violet-500 disabled:opacity-50 ${assigned && !needsRepair ? 'text-red-700 hover:bg-red-50 dark:text-red-300 dark:hover:bg-red-500/10' : 'bg-violet-600 text-white hover:bg-violet-500'}`}>
                          {busy ? <Loader2 size={12} className="animate-spin" /> : t(state === 'differs' ? 'mcpLibrary.arbitration.open' : needsRepair ? 'mcpLibrary.oneClickRepair' : assigned ? 'mcpLibrary.oneClickUninstall' : 'mcpLibrary.oneClickConfigure')}
                        </button>}
                      </div>
                    })}
                  </div>}
                </article>
              )
            })}
          </div>
        )}
      </div>
      </>}

      {!loadError && section === 'tools' && <div className="min-h-0 flex-1 overflow-y-auto p-5">
        <div className="mx-auto max-w-5xl">
          <div className="mb-4 flex flex-wrap items-center gap-3">
            <div className="min-w-0 flex-1">
              <h2 className="text-sm font-semibold text-gray-900 dark:text-gray-100">{t('mcpLibrary.toolsTitle')}</h2>
            </div>
            <select aria-label={t('mcpLibrary.selectToolService')} value={selectedToolEntry?.name ?? ''}
              onChange={event => setToolService(event.target.value)}
              className="min-w-48 rounded-lg border border-gray-300 bg-white px-3 py-2 text-sm text-gray-800 focus-visible:outline focus-visible:outline-2 focus-visible:outline-violet-500 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100">
              {mcpCatalog.map(entry => <option key={entry.name} value={entry.name}>{entry.name}</option>)}
            </select>
            <button type="button" onClick={() => { if (selectedToolEntry) void discoverTools(selectedToolEntry) }}
              disabled={!selectedToolEntry || selectedToolDiscovery?.loading || selectedToolEntry.transport === 'sse' || selectedToolEntry.transport === 'http'}
              className="inline-flex items-center gap-2 rounded-lg bg-violet-600 px-3 py-2 text-xs font-medium text-white hover:bg-violet-500 focus-visible:outline focus-visible:outline-2 focus-visible:outline-violet-500 disabled:opacity-50">
              {selectedToolDiscovery?.loading ? <Loader2 size={14} className="animate-spin" /> : <RefreshCw size={14} />}{t('mcpLibrary.discoverTools')}
            </button>
          </div>
          {selectedToolEntry && (selectedToolEntry.transport === 'sse' || selectedToolEntry.transport === 'http') && <p className="mb-3 text-sm text-gray-600 dark:text-gray-300">{t('mcpLibrary.toolsRemoteUnsupported')}</p>}
          {selectedToolDiscovery?.loading && <p role="status" className="mb-3 text-sm text-gray-600 dark:text-gray-300">{t('mcpLibrary.toolsLoading')}</p>}
          {!selectedToolEntry && <p className="py-12 text-center text-sm text-gray-500 dark:text-gray-400">{t('mcpLibrary.emptyTitle')}</p>}
          {selectedToolEntry && selectedToolEntry.transport !== 'sse' && selectedToolEntry.transport !== 'http' && !selectedToolDiscovery && <p className="border-t border-gray-200 py-10 text-center text-sm text-gray-500 dark:border-gray-700 dark:text-gray-400">{t('mcpLibrary.toolsStartHint')}</p>}
          {selectedToolDiscovery?.error && <p role="alert" className="rounded-lg bg-red-50 px-4 py-3 text-sm text-red-700 dark:bg-red-500/10 dark:text-red-300">{t('mcpLibrary.toolsLoadFailed', { error: selectedToolDiscovery.error })}</p>}
          {selectedToolDiscovery?.result && <div className="border-t border-gray-200 dark:border-gray-700">
            <div className="flex flex-wrap items-center gap-3 py-3 text-xs text-gray-500 dark:text-gray-400">
              <span className="font-medium text-gray-800 dark:text-gray-200">{t(`mcpLibrary.toolsState.${selectedToolDiscovery.result.state}`)}</span>
              {['complete', 'empty', 'partial'].includes(selectedToolDiscovery.result.state) && <span>{t('mcpLibrary.toolCount', { count: selectedToolDiscovery.result.tools.length })}</span>}
              <span>{new Date(selectedToolDiscovery.result.fetched_at).toLocaleString()}</span>
            </div>
            {selectedToolDiscovery.result.reason && <p role={selectedToolDiscovery.result.state === 'error' ? 'alert' : undefined} className="mb-3 text-xs text-amber-700 dark:text-amber-300">{selectedToolDiscovery.result.reason}</p>}
            {selectedToolDiscovery.result.state === 'partial' && <p role="status" className="mb-3 text-xs text-amber-700 dark:text-amber-300">{t('mcpLibrary.toolsPartialHint')}</p>}
            {selectedToolDiscovery.result.state === 'empty' && <p className="py-8 text-center text-sm text-gray-500 dark:text-gray-400">{t('mcpLibrary.toolsEmpty')}</p>}
            <div className="divide-y divide-gray-100 dark:divide-gray-800">{selectedToolDiscovery.result.tools.map(tool => <div key={tool.name} className="grid gap-1 py-3 sm:grid-cols-[minmax(12rem,1fr)_2fr] sm:gap-6">
              <span className="break-all font-mono text-xs font-medium text-gray-900 dark:text-gray-100">{tool.name}</span>
              <p className="text-xs leading-5 text-gray-600 dark:text-gray-300">{tool.description || t('mcpLibrary.noDescription')}</p>
            </div>)}</div>
          </div>}
        </div>
      </div>}

      {!loadError && section === 'calls' && (
        <div className="min-h-0 flex-1 overflow-y-auto space-y-4 p-5">
          <section className="rounded-2xl border border-gray-200 bg-white p-4 dark:border-gray-700 dark:bg-gray-900">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <div><h2 className="text-sm font-semibold text-gray-900 dark:text-gray-100">{t('mcpLibrary.observedCallsTitle')}</h2></div>
              <button onClick={() => { void refreshCallStatistics().catch(() => {}) }} disabled={mcpCallRefreshState === 'loading'} className="inline-flex items-center gap-1 rounded-lg border border-gray-300 px-2.5 py-1.5 text-xs text-gray-600 disabled:opacity-50 dark:border-gray-600 dark:text-gray-300"><RefreshCw size={12} className={mcpCallRefreshState === 'loading' ? 'animate-spin' : ''} />{t('mcpLibrary.refreshObservations')}</button>
            </div>
            <div className="mt-4 flex flex-wrap gap-2">
              <select aria-label={t('mcpLibrary.callServerFilter')} value={callServer} onChange={event => setCallServer(event.target.value)}
                className="min-w-44 rounded-lg border border-gray-300 bg-white px-3 py-2 text-xs text-gray-800 focus-visible:outline focus-visible:outline-2 focus-visible:outline-violet-500 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100">
                <option value="">{t('mcpLibrary.allServices')}</option>
                <option value="Vibe-Assistant-memory">Vibe-Assistant-memory · {t('mcpLibrary.observationSourceMemoryMcp')}</option>
                <option value="agent-manager-memory">agent-manager-memory · {t('mcpLibrary.observationSourceMemoryMcp')}</option>
                {mcpCatalog.map(entry => <option key={entry.name} value={entry.name}>{entry.name}</option>)}
              </select>
              <select aria-label={t('mcpLibrary.callSourceFilter')} value={callSource} onChange={event => setCallSource(event.target.value)}
                className="min-w-44 rounded-lg border border-gray-300 bg-white px-3 py-2 text-xs text-gray-800 focus-visible:outline focus-visible:outline-2 focus-visible:outline-violet-500 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100">
                <option value="">{t('mcpLibrary.allSources')}</option>
                <option value="stdio_proxy">{t('mcpLibrary.observationSourceProxy')}</option>
                <option value="claude">{t('mcpLibrary.observationSourceTranscript')}</option>
                <option value="memory_mcp">{t('mcpLibrary.observationSourceMemoryMcp')}</option>
                <option value="dsh">{t('mcpLibrary.observationSourcePlugin')}</option>
              </select>
            </div>
            {mcpCallError && <p role="alert" className="mt-3 text-xs text-red-600 dark:text-red-400">{t('mcpLibrary.observedLoadFailed', { error: mcpCallError })}</p>}
            {!mcpCallError && <div className="mt-3 grid grid-cols-2 gap-2 sm:grid-cols-4">
              {[[t('mcpLibrary.observedCalls'), mcpCallStats?.total_calls ?? '—'], [t('mcpLibrary.observedFailed'), mcpCallStats?.failed_calls ?? '—'], [t('mcpLibrary.observedPending'), mcpCallStats?.pending_calls ?? '—'], [t('mcpLibrary.observedUnknown'), mcpCallStats?.unknown_calls ?? '—']].map(([label, value]) => <div key={label} className="rounded-xl bg-gray-50 p-3 dark:bg-gray-800"><p className="text-[11px] text-gray-500 dark:text-gray-400">{label}</p><p className="mt-1 text-lg font-semibold text-gray-900 dark:text-gray-100">{value}</p></div>)}
            </div>}
            {!mcpCallError && <p className="mt-2 text-xs leading-5 text-gray-600 dark:text-gray-300">{t('mcpLibrary.pendingEvidenceHint')}</p>}
            {mcpCallStats && !mcpCallError && mcpCallStats.recent.length > 0 && <div className="mt-3">
              <p className="mb-1 text-xs font-medium text-gray-600 dark:text-gray-300">{t('mcpLibrary.recentCalls')}</p>
              <div className="max-h-72 space-y-1.5 overflow-y-auto">
                {mcpCallStats.recent.map(call => (
                  <div key={`${call.source}-${call.id}`} className="flex flex-wrap items-center gap-x-2 gap-y-1 rounded-lg bg-gray-50 px-3 py-2 text-xs dark:bg-gray-800">
                    <span className="font-mono">{call.server_name} / {call.tool_name}</span>
                    <span className={call.status === 'success' ? 'text-emerald-600 dark:text-emerald-300' : call.status === 'failed' ? 'text-red-600 dark:text-red-400' : call.status === 'unknown' ? 'text-amber-800 dark:text-amber-300' : 'text-gray-500 dark:text-gray-400'}>
                      {t(call.status === 'success' ? 'mcpLibrary.observedSuccess' : call.status === 'failed' ? 'mcpLibrary.observedFailed' : call.status === 'unknown' ? 'mcpLibrary.observedUnknown' : 'mcpLibrary.observedPending')}
                    </span>
                    <span className="ml-auto text-gray-500 dark:text-gray-400">{t(call.origin === 'stdio_relay' ? 'mcpLibrary.observationSourceRelay' : call.origin === 'memory_mcp' ? 'mcpLibrary.observationSourceMemoryMcp' : call.origin === 'transcript' ? 'mcpLibrary.observationSourceTranscript' : call.origin === 'dsh_plugin' ? 'mcpLibrary.observationSourcePlugin' : 'mcpLibrary.observationSourceProxy')} · {new Date(call.occurred_at).toLocaleString()}</span>
                    {call.status === 'unknown' && <p className="basis-full break-words text-xs leading-5 text-amber-800 dark:text-amber-300">{t(`mcpLibrary.observationUnknownReason.${call.error_reason ?? 'unknown'}`, { defaultValue: t('mcpLibrary.observationUnknownReason.unknown') })}</p>}
                    {call.status === 'pending' && Number.isFinite(Date.parse(call.occurred_at)) && <p className="basis-full text-xs leading-5 text-gray-600 dark:text-gray-300">{t(callClock - Date.parse(call.occurred_at) < 60000 ? 'mcpLibrary.waitingLessThanMinute' : 'mcpLibrary.waitingMinutes', { count: Math.max(0, Math.floor((callClock - Date.parse(call.occurred_at)) / 60000)) })}</p>}
                  </div>
                ))}
              </div>
            </div>}
            {mcpCallStats && !mcpCallError && mcpCallStats.recent.length === 0 && <p className="py-8 text-center text-xs text-gray-500 dark:text-gray-400">{t('mcpLibrary.noCallsForFilter')}</p>}
            {mcpCallStats && !mcpCallError && mcpCallStats.tools.length > 0 && <div className="mt-4 border-t border-gray-100 pt-3 dark:border-gray-800">
              <h3 className="mb-2 text-xs font-medium text-gray-700 dark:text-gray-300">{t('mcpLibrary.callByTool')}</h3>
              <div className="mb-2 flex flex-wrap gap-2">
                <input value={callToolQuery} onChange={event => { setCallToolQuery(event.target.value); setCallToolPage(0) }}
                  aria-label={t('mcpLibrary.toolStatsSearch')} placeholder={t('mcpLibrary.toolStatsSearch')}
                  className="min-w-0 flex-1 rounded-lg border border-gray-300 bg-white px-3 py-2 text-xs text-gray-800 focus-visible:outline focus-visible:outline-2 focus-visible:outline-violet-500 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100" />
                <select value={callToolSort} onChange={event => { setCallToolSort(event.target.value as typeof callToolSort); setCallToolPage(0) }} aria-label={t('mcpLibrary.toolStatsSort')}
                  className="rounded-lg border border-gray-300 bg-white px-3 py-2 text-xs text-gray-800 focus-visible:outline focus-visible:outline-2 focus-visible:outline-violet-500 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100">
                  <option value="calls">{t('mcpLibrary.toolSortCalls')}</option>
                  <option value="failure">{t('mcpLibrary.toolSortFailure')}</option>
                  <option value="name">{t('mcpLibrary.toolSortName')}</option>
                </select>
              </div>
              {toolGroups.length === 0 && <p className="py-3 text-xs text-gray-500 dark:text-gray-400">{t('mcpLibrary.noMatchingTools')}</p>}
              <div className="divide-y divide-gray-100 text-xs dark:divide-gray-800">{visibleToolGroups.map(tool => <div key={JSON.stringify([tool.server_name, tool.name])} className="flex flex-wrap items-center gap-3 py-2">
                <span className="min-w-0 flex-1 break-all font-mono text-gray-800 dark:text-gray-200">{tool.server_name} / {tool.name}</span>
                <span className="tabular-nums text-gray-600 dark:text-gray-300">{t('mcpLibrary.callToolSummary', { total: tool.calls, failed: tool.failed, pending: tool.pending, unknown: tool.unknown ?? 0 })}</span>
                {tool.failed > 0 && onSectionChange && <button type="button" onClick={() => showToolFailures(tool.server_name ?? '', tool.name)} className="rounded px-2 py-1 text-violet-700 hover:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-violet-500 dark:text-violet-300">{t('mcpLibrary.viewToolFailures')}</button>}
              </div>)}</div>
              {toolGroups.length > 25 && <div className="mt-2 flex flex-wrap items-center justify-between gap-2 text-xs text-gray-600 dark:text-gray-300">
                <span>{t('mcpLibrary.toolStatsPage', { page: toolPage + 1, pages: toolPages, count: toolGroups.length })}</span>
                <div className="flex gap-2">
                  <button type="button" disabled={toolPage === 0} onClick={() => setCallToolPage(toolPage - 1)} className="rounded border border-gray-300 px-3 py-1.5 disabled:opacity-50 dark:border-gray-600">{t('mcpLibrary.previousPage')}</button>
                  <button type="button" disabled={toolPage + 1 >= toolPages} onClick={() => setCallToolPage(toolPage + 1)} className="rounded border border-gray-300 px-3 py-1.5 disabled:opacity-50 dark:border-gray-600">{t('mcpLibrary.nextPage')}</button>
                </div>
              </div>}
            </div>}
            {mcpCallStats && !mcpCallError && <div className="mt-3">
              <button type="button" onClick={() => setShowCoverage(current => !current)} aria-expanded={showCoverage} className="text-xs text-violet-700 hover:underline dark:text-violet-300">{t('mcpLibrary.coverageDetails')}</button>
              {showCoverage && <div className="mt-2 space-y-2 text-xs text-gray-500 dark:text-gray-400">
                <p>{t('mcpLibrary.observationSourceCounts', { native: mcpCallStats.native_calls, proxy: mcpCallStats.proxy_calls, transcript: mcpCallStats.transcript_calls, plugin: mcpCallStats.plugin_calls })}</p>
                <div className="flex flex-wrap gap-1.5">{mcpCallStats.coverage.map(item => <span key={item.source} title={item.state === 'unsupported' ? t('mcpLibrary.coverageReason.unsupported') : item.reason ?? undefined} className="rounded-full bg-violet-50 px-2 py-1 text-[10px] text-violet-700 dark:bg-violet-500/10 dark:text-violet-300">{item.source}: {t(`mcpLibrary.coverageState.${item.state}`, { defaultValue: item.state })}</span>)}</div>
              </div>}
            </div>}
          </section>
        </div>
      )}

      {!loadError && section === 'errors' && <div className="min-h-0 flex-1 overflow-y-auto p-5">
        <div className="mx-auto max-w-5xl">
          <div className="mb-4 flex flex-wrap items-start justify-between gap-3">
            <div>
              <h2 className="text-sm font-semibold text-gray-900 dark:text-gray-100">{t('mcpLibrary.errorLogTitle')}</h2>
            </div>
            <button type="button" onClick={() => { void loadFailedCalls() }} disabled={failedCallsLoading}
              className="inline-flex items-center gap-2 rounded-lg border border-gray-300 px-3 py-2 text-xs font-medium text-gray-700 hover:bg-gray-50 focus-visible:outline focus-visible:outline-2 focus-visible:outline-violet-500 disabled:opacity-50 dark:border-gray-600 dark:text-gray-200 dark:hover:bg-gray-800">
              <RefreshCw size={14} className={failedCallsLoading ? 'animate-spin' : ''} />{t('mcpLibrary.refreshObservations')}
            </button>
          </div>
          {newFailedCalls > 0 && <button type="button" disabled={failedCallsLoading} onClick={() => { void loadFailedCalls() }} className="mb-3 rounded-lg bg-violet-50 px-3 py-2 text-sm text-violet-800 focus-visible:outline focus-visible:outline-2 focus-visible:outline-violet-500 dark:bg-violet-500/10 dark:text-violet-200">{t('mcpLibrary.newFailures', { count: newFailedCalls })}</button>}
          {(errorTool || errorWindow) && <div className="mb-3 flex flex-wrap items-center gap-2 text-xs text-gray-600 dark:text-gray-300">
            {errorTool && <span className="break-all">{errorServer} / {errorTool}</span>}
            {errorWindow && <span>{new Date(errorWindow.start).toLocaleString(i18n.language)} – {new Date(errorWindow.end).toLocaleString(i18n.language)}</span>}
            <button type="button" onClick={() => { setErrorTool(''); setErrorWindow(null) }} className="rounded px-2 py-1 text-violet-700 hover:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-violet-500 dark:text-violet-300">{t('mcpLibrary.clearFailureScope')}</button>
          </div>}
          <div className="mb-4 flex flex-wrap gap-2">
            <select aria-label={t('mcpLibrary.callServerFilter')} value={errorServer} onChange={event => { setErrorServer(event.target.value); setErrorTool(''); setErrorWindow(null) }}
              className="min-w-44 rounded-lg border border-gray-300 bg-white px-3 py-2 text-xs text-gray-800 focus-visible:outline focus-visible:outline-2 focus-visible:outline-violet-500 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100">
              <option value="">{t('mcpLibrary.allServices')}</option>
              {errorServer && !mcpCatalog.some(entry => entry.name === errorServer) && <option value={errorServer}>{errorServer}</option>}
              {mcpCatalog.map(entry => <option key={entry.name} value={entry.name}>{entry.name}</option>)}
            </select>
            <select aria-label={t('mcpLibrary.callSourceFilter')} value={errorOrigin} onChange={event => setErrorOrigin(event.target.value)}
              className="min-w-44 rounded-lg border border-gray-300 bg-white px-3 py-2 text-xs text-gray-800 focus-visible:outline focus-visible:outline-2 focus-visible:outline-violet-500 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100">
              <option value="">{t('mcpLibrary.allSources')}</option>
              <option value="stdio_proxy">{t('mcpLibrary.observationSourceProxy')}</option>
              <option value="stdio_relay">{t('mcpLibrary.observationSourceRelay')}</option>
              <option value="transcript">{t('mcpLibrary.observationSourceTranscript')}</option>
              <option value="memory_mcp">{t('mcpLibrary.observationSourceMemoryMcp')}</option>
              <option value="dsh_plugin">{t('mcpLibrary.observationSourcePlugin')}</option>
            </select>
          </div>
          {failedCallsError && <p role="alert" className="rounded-lg bg-red-50 px-4 py-3 text-xs text-red-700 dark:bg-red-500/10 dark:text-red-300">{t('mcpLibrary.failedCallsLoadFailed', { error: failedCallsError })}</p>}
          {failedCalls && <>
            <div className="mb-4 flex flex-wrap items-center gap-2 border-y border-gray-200 py-3 text-xs dark:border-gray-700">
              <span className="font-semibold text-gray-900 dark:text-gray-100">{t('mcpLibrary.failedTotal', { count: failedCalls.total })}</span>
              {failedCalls.categories.map(item => <span key={item.category} className="rounded-md bg-red-50 px-2 py-1 text-red-700 dark:bg-red-500/10 dark:text-red-300">{t(`mcpLibrary.failureCategory.${item.category}`, { defaultValue: t('mcpLibrary.failureCategory.unknown_error') })} · {item.count}</span>)}
            </div>
            {failedCalls.total === 0 && <p className="py-12 text-center text-sm text-gray-500 dark:text-gray-400">{t('mcpLibrary.noFailedCalls')}</p>}
            <div className="divide-y divide-gray-200 dark:divide-gray-800">{failedCalls.calls.map(call => <details key={call.id} className="group py-3">
              <summary className="flex cursor-pointer list-none flex-wrap items-center gap-x-4 gap-y-1 rounded-md px-2 py-1 text-xs hover:bg-gray-50 focus-visible:outline focus-visible:outline-2 focus-visible:outline-violet-500 dark:hover:bg-gray-800">
                <span className="min-w-0 flex-1 break-all font-mono font-medium text-gray-900 dark:text-gray-100">{call.origin === 'stdio_relay' ? t('mcpLibrary.relaySystemError') : `${call.server_name} / ${call.tool_name}`}</span>
                <span className="font-medium text-red-700 dark:text-red-300">{t(`mcpLibrary.failureCategory.${call.error_category ?? 'unknown_error'}`, { defaultValue: t('mcpLibrary.failureCategory.unknown_error') })}</span>
                <span className="text-gray-500 dark:text-gray-400">{new Date(call.occurred_at).toLocaleString()}</span>
              </summary>
              <dl className="mt-2 grid gap-x-6 gap-y-2 rounded-lg bg-gray-50 px-4 py-3 text-xs dark:bg-gray-800 sm:grid-cols-2">
                <div><dt className="text-gray-500 dark:text-gray-400">{t('mcpLibrary.logSource')}</dt><dd className="mt-0.5 text-gray-800 dark:text-gray-200">{t(call.origin === 'stdio_relay' ? 'mcpLibrary.observationSourceRelay' : call.origin === 'memory_mcp' ? 'mcpLibrary.observationSourceMemoryMcp' : call.origin === 'transcript' ? 'mcpLibrary.observationSourceTranscript' : call.origin === 'dsh_plugin' ? 'mcpLibrary.observationSourcePlugin' : 'mcpLibrary.observationSourceProxy')} · {call.source}</dd></div>
                <div><dt className="text-gray-500 dark:text-gray-400">{t('mcpLibrary.logClient')}</dt><dd className="mt-0.5 break-all font-mono text-gray-800 dark:text-gray-200">{call.session_id}</dd></div>
                <div><dt className="text-gray-500 dark:text-gray-400">{t('mcpLibrary.logPhase')}</dt><dd className="mt-0.5 text-gray-800 dark:text-gray-200">{call.error_phase ? t(`mcpLibrary.failurePhase.${call.error_phase}`) : '—'}</dd></div>
                <div><dt className="text-gray-500 dark:text-gray-400">{t('mcpLibrary.logKind')}</dt><dd className="mt-0.5 text-gray-800 dark:text-gray-200">{call.failure_kind ? t(`mcpLibrary.failureKind.${call.failure_kind}`) : '—'}</dd></div>
                <div><dt className="text-gray-500 dark:text-gray-400">{t('mcpLibrary.logCode')}</dt><dd className="mt-0.5 font-mono text-gray-800 dark:text-gray-200">{call.error_code ?? '—'}</dd></div>
                <div><dt className="text-gray-500 dark:text-gray-400">{t('mcpLibrary.logResult')}</dt><dd className="mt-0.5 text-gray-800 dark:text-gray-200">{t(call.result_observed ? 'mcpLibrary.failureResultObserved' : 'mcpLibrary.failureResultMissing')}</dd></div>
                {call.error_reason && <div className="sm:col-span-2"><dt className="text-gray-500 dark:text-gray-400">{t('mcpLibrary.logReason')}</dt><dd className="mt-0.5 break-words text-gray-800 dark:text-gray-200">{call.error_reason}</dd></div>}
              </dl>
              <p className="mt-2 px-2 text-xs leading-5 text-gray-600 dark:text-gray-300">{t(`mcpLibrary.failureAdvice.${call.failure_kind ?? call.error_category ?? 'unknown'}`, { defaultValue: t('mcpLibrary.failureAdvice.unknown') })}</p>
            </details>)}</div>
            {failedHasMore && failedCalls.calls.length < failedCalls.total && <button type="button" onClick={() => { void loadFailedCalls(failedCalls.calls.length) }} disabled={failedCallsLoading}
              className="mt-4 rounded-lg border border-gray-300 px-3 py-2 text-xs font-medium text-gray-700 hover:bg-gray-50 disabled:opacity-50 dark:border-gray-600 dark:text-gray-200 dark:hover:bg-gray-800">{t('mcpLibrary.moreFailedCalls', { count: failedCalls.total - failedCalls.calls.length })}</button>}
          </>}
          {failedCallsLoading && !failedCalls && <p className="py-8 text-center text-xs text-gray-500 dark:text-gray-400">{t('mcpLibrary.loadingFailedCalls')}</p>}
        </div>
      </div>}

      {addOpen && (
        <AddEditDialog
          initial={editing}
          editing={editing !== null}
          onClose={() => { setAddOpen(false); setEditing(null) }}
          onSaved={(name) => setNotice(t('mcpLibrary.saveDone', { name }))}
        />
      )}
      {equipping && (
        <EquipDialog entry={equipping} onClose={() => setEquipping(null)} onNotice={setNotice} />
      )}
      {arbitrating && <ConfigurationArbitrationDialog name={arbitrating.name} initialAgent={arbitrating.agent} onClose={() => setArbitrating(null)} onApplied={async result => {
        await refreshAll()
        setNotice(t(result.errors.length > 0 ? 'mcpLibrary.arbitration.partialToast' : 'mcpLibrary.arbitration.done', { count: result.applied_agents.length, name: result.entry.name }))
      }} />}
      {importOpen && (
        <ImportDialog onClose={() => { setImportOpen(false) }} onNotice={setNotice} onCompare={name => { setImportOpen(false); setArbitrating({ name }) }} />
      )}
    </div>
  )
})
