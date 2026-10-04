import { useCallback, useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { invoke } from '@tauri-apps/api/core'
import { useAgentStore } from './store/agentStore'
import { LanguageSwitcher } from './components/LanguageSwitcher'
import { AgentList } from './components/AgentList'
import { AgentDetail } from './components/AgentDetail'
import { AgentForm } from './components/AgentForm'
import { TerminalPanel } from './components/TerminalPanel'
import { PortManager } from './pages/PortManager'
import { NetworkManager } from './pages/NetworkManager'
import { EnvManager } from './pages/EnvManager'
import { ProxyManager } from './pages/ProxyManager'
import { Settings } from './pages/Settings'
import { MemoryCenter } from './pages/MemoryCenter'
import { SkillLibrary } from './pages/SkillLibrary'
import { PublishedSkills } from './pages/PublishedSkills'
import { SkillMarketplace } from './pages/SkillMarketplace'
import { McpLibrary } from './pages/McpLibrary'
import { UsageAnalytics } from './pages/UsageAnalytics'
import { PendingMemories } from './pages/PendingMemories'
import { OrganizedConversations } from './pages/OrganizedConversations'
import { MemoryInjection } from './pages/MemoryInjection'
import { ErrorBoundary } from './components/ErrorBoundary'
import { ErrorRecovery, RecoveryNavigationContext, type RecoveryTarget } from './components/ErrorRecovery'
import { useDocumentVisible, useVisiblePolling } from './hooks/useVisiblePolling'
import { UpdateChecker } from './components/UpdateChecker'
import { Onboarding, needsOnboarding } from './components/Onboarding'
import { useTheme } from './theme'
import type { AgentState } from './types/agent'
import { useResizable } from './hooks/useResizable'
import { NativeWebviewPanel, type OpenTab } from './components/NativeWebviewPanel'
import {
  Plus, RefreshCw, Bot, X, Globe, Network,
  Maximize2, Minimize2, TerminalSquare, Sun, Moon,
  Shield, Eraser, Settings2, Brain, BookOpenText, Plug,
  ChevronDown, ChevronRight, BarChart3, Gauge, Variable,
} from 'lucide-react'
import logoUrl from '/logo.png'

type NavPage = 'agents' | 'ports' | 'network' | 'env-vars' | 'proxy' | 'settings' | 'memory' | 'skills' | 'published-skills' | 'skill-marketplace' | 'mcp-library' | 'usage' | 'pending-memories' | 'organized-conversations' | 'memory-injection'

const MEMORY_PAGES = ['memory', 'pending-memories', 'organized-conversations', 'memory-injection'] as const
// Skill 库两页同样「访问过即保持挂载」：重进保留筛选/勾选状态，也免去整树重挂载。
const SKILL_PAGES = ['skills', 'published-skills', 'skill-marketplace'] as const
const MCP_PAGES = ['mcp-library'] as const
const KEPT_PAGES: readonly string[] = [...MEMORY_PAGES, ...SKILL_PAGES, ...MCP_PAGES, 'usage']

export default function App() {
  const {
    agents, selectedId, logs, loading,
    fetchAgents, selectAgent, reorderAgents,
    startAgent, stopAgent, saveAgent, deleteAgent,
  } = useAgentStore()

  const { theme, toggle: toggleTheme } = useTheme()
  const { t } = useTranslation()

  const [page, setPage] = useState<NavPage>('agents')
  const documentVisible = useDocumentVisible()
  const openRecoveryTarget = useCallback((target: RecoveryTarget) => setPage(target), [])
  const [agentError, setAgentError] = useState<{ agent: AgentState; message: string; retry: () => void } | null>(null)
  // 记忆族页面访问过即保持挂载：返回时不再
  // 重新挂载触发全量数据重查，消除往返卡顿。未激活时仅 CSS 隐藏。
  const [keptPages, setKeptPages] = useState<Set<string>>(new Set())
  useEffect(() => {
    if (KEPT_PAGES.includes(page)) {
      setKeptPages((previous) => previous.has(page) ? previous : new Set(previous).add(page))
    }
  }, [page])
  const [showForm, setShowForm] = useState(false)
  const [showOnboarding, setShowOnboarding] = useState(needsOnboarding)
  const [editingAgent, setEditingAgent] = useState<AgentState | null>(null)
  const [openTabs, setOpenTabs] = useState<OpenTab[]>([])
  const [activeTabKey, setActiveTabKey] = useState<string | null>(null)
  const [terminalClearVersions, setTerminalClearVersions] = useState<Record<string, number>>({})
  const [panelFullscreen, setPanelFullscreen] = useState(false)
  // 从「已发布」页跳入 Skill 库时自动打开同步对话框
  const [skillsAutoSync, setSkillsAutoSync] = useState(false)
  // 侧边栏分组展开状态：默认展开记忆中心与 Skill 库，让子页面可发现
  const [expandedGroups, setExpandedGroups] = useState<Set<string>>(() => new Set(['memory', 'skills']))
  // 导航到子页面时自动展开所属分组
  useEffect(() => {
    const groupMap: Record<string, string> = {}
    for (const p of MEMORY_PAGES) groupMap[p] = 'memory'
    for (const p of SKILL_PAGES) groupMap[p] = 'skills'
    const group = groupMap[page]
    if (group) {
      setExpandedGroups(prev => prev.has(group) ? prev : new Set(prev).add(group))
    }
  }, [page])

  // 页面导航回调稳定化：配合 memo 化页面，App 的 5s agent 轮询重渲染不再波及隐藏页面。
  const openMemory = useCallback(() => setPage('memory'), [])
  const openUsage = useCallback(() => setPage('usage'), [])
  const openPendingMemories = useCallback(() => setPage('pending-memories'), [])
  const openOrganizedConversations = useCallback(() => setPage('organized-conversations'), [])
  const openMemoryInjection = useCallback(() => setPage('memory-injection'), [])
  const openSkills = useCallback(() => setPage('skills'), [])
  const openPublishedSkills = useCallback(() => setPage('published-skills'), [])
  const openSkillMarketplace = useCallback(() => setPage('skill-marketplace'), [])
  const clearSkillsAutoSync = useCallback(() => setSkillsAutoSync(false), [])
  const goSkillSync = useCallback(() => { setSkillsAutoSync(true); setPage('skills') }, [])

  const { width: sidebarWidth, height: panelHeight, onColMouseDown, onRowMouseDown } = useResizable({
    minW: 200, maxW: 480, defaultW: 288,
    minH: 120, maxH: 800, defaultH: 380,
  })

  const [appVersion, setAppVersion] = useState('1.0.0')
  useEffect(() => {
    invoke<string>('get_app_version').then(setAppVersion).catch(() => {})
  }, [])

  const selectedAgent = agents.find(a => a.config.id === selectedId) ?? null
  const agentLogs = selectedId ? (logs[selectedId] ?? []) : []
  const activeTab = openTabs.find(t => `${t.agentId}:${t.kind}` === activeTabKey)
    ?? openTabs.find(t => t.agentId === selectedId)
    ?? openTabs[0]
    ?? null
  const resolvedActiveTabKey = activeTab ? `${activeTab.agentId}:${activeTab.kind}` : null
  const showPanel = activeTab !== null
  const showSplit = showPanel && !panelFullscreen

  // Port/proxy pages also consume this list; hidden workspaces do not need logs.
  useVisiblePolling(fetchAgents, 5000, page === 'agents' || page === 'ports' || page === 'proxy')
  const refreshLogs = useCallback(async () => {
    if (selectedId) await useAgentStore.getState().fetchLogs(selectedId)
  }, [selectedId])
  useVisiblePolling(refreshLogs, 3000, page === 'agents' && !panelFullscreen && selectedId !== null)

  async function runAgentAction(id: string, action: 'start' | 'stop') {
    setAgentError(null)
    try {
      await (action === 'start' ? startAgent(id) : stopAgent(id))
    } catch (error) {
      const agent = agents.find(item => item.config.id === id)
      if (agent) setAgentError({ agent, message: String(error), retry: () => { void runAgentAction(id, action) } })
      await fetchAgents()
      await useAgentStore.getState().fetchLogs(id).catch(() => {})
    }
  }
  const handleStart = (id: string) => { void runAgentAction(id, 'start') }
  const handleStop = (id: string) => { void runAgentAction(id, 'stop') }

  function openAgentUI(agent: AgentState) {
    if (!agent.config.port) return
    const id = agent.config.id
    const uiTab: OpenTab = {
      agentId: id,
      label: agent.config.name,
      kind: 'ui',
      port: agent.config.port,
      token: agent.config.ui_token,
    }
    setOpenTabs(tabs => {
      const exists = tabs.some(tab => tab.agentId === id && tab.kind === 'ui')
      return exists
        ? tabs.map(tab => tab.agentId === id && tab.kind === 'ui' ? uiTab : tab)
        : [...tabs, uiTab]
    })
    selectAgent(id)
    setActiveTabKey(`${id}:ui`)
    setPage('agents')
  }

  async function openAgentTerminal(agent: AgentState) {
    setAgentError(null)
    const id = agent.config.id
    const existing = openTabs.find(t => t.agentId === id && t.kind === 'terminal')
    if (!existing) {
      // 与 Agent 进程共用同一 cwd 解析：worktree 隔离时返回（惰性创建的）独立工作树。
      let cwd = agent.config.working_dir
      try {
        cwd = await invoke<string>('resolve_agent_cwd', { id })
      } catch (e) {
        // Never silently lose isolation by falling back to the shared repository.
        setAgentError({ agent, message: String(e), retry: () => { void openAgentTerminal(agent) } })
        return
      }
      setOpenTabs(tabs => [
        ...tabs.filter(t => !(t.agentId === id && t.kind === 'terminal')),
        {
          agentId: id,
          label: agent.config.name,
          kind: 'terminal',
          command: agent.config.command,
          args: agent.config.args,
          cwd,
          env: agent.config.env,
        },
      ])
    }
    selectAgent(id)
    setActiveTabKey(`${id}:terminal`)
  }

  function closeTab(tab: OpenTab, e: React.MouseEvent) {
    e.stopPropagation()
    if (tab.kind === 'ui') {
      invoke('close_agent_ui_webview', { agentId: tab.agentId }).catch(() => {})
    }
    const key = `${tab.agentId}:${tab.kind}`
    setOpenTabs(tabs => tabs.filter(t => !(t.agentId === tab.agentId && t.kind === tab.kind)))
    if (activeTabKey === key) setActiveTabKey(null)
    setPanelFullscreen(false)
  }

  function clearActiveTerminal() {
    if (!activeTab || activeTab.kind !== 'terminal') return
    setTerminalClearVersions(versions => ({
      ...versions,
      [activeTab.agentId]: (versions[activeTab.agentId] ?? 0) + 1,
    }))
  }

  function togglePanelFullscreen() {
    if (!activeTab) return
    if (activeTab.kind === 'ui') {
      invoke('fullscreen_agent_ui_webview', {
        agentId: activeTab.agentId,
        title: activeTab.label,
      }).catch(error => console.error('Failed to fullscreen agent UI', error))
      return
    }
    setPanelFullscreen(fullscreen => !fullscreen)
  }

  function openNew() { setEditingAgent(null); setShowForm(true) }
  function openEdit(agent: AgentState) { setEditingAgent(agent); setShowForm(true) }

  const uiIsOpen = selectedAgent
    ? openTabs.some(t => t.agentId === selectedAgent.config.id && t.kind === 'ui')
    : false
  const termIsOpen = selectedAgent
    ? openTabs.some(t => t.agentId === selectedAgent.config.id && t.kind === 'terminal')
    : false

  return (
    <RecoveryNavigationContext.Provider value={openRecoveryTarget}>
    <div className="flex h-screen overflow-hidden bg-gray-100 text-gray-800 dark:bg-gray-950 dark:text-gray-100">

      {/* ── Sidebar ─────────────────────────────────────── */}
      <aside
        style={{ width: sidebarWidth }}
        className="flex shrink-0 flex-col border-r border-gray-200 bg-white dark:border-gray-800 dark:bg-gray-900"
      >
        {/* App title */}
        <div className="flex items-center justify-between border-b border-gray-200 px-4 py-3 dark:border-gray-800">
          <div className="flex items-center gap-2.5">
            <img src={logoUrl} alt={t('app.title')} className="h-7 w-7 rounded-lg" />
            <div className="flex flex-col leading-tight">
              <span className="text-sm font-semibold text-gray-900 dark:text-gray-100">{t('app.title')}</span>
            </div>
          </div>
          <div className="flex items-center gap-0.5">
            <LanguageSwitcher />
            <button
              onClick={toggleTheme}
              className="rounded-md p-1.5 text-gray-400 transition-colors hover:bg-gray-100 hover:text-gray-600 dark:hover:bg-gray-800 dark:hover:text-gray-300"
              title={theme === 'dark' ? t('app.switchToLight') : t('app.switchToDark')}
            >
              {theme === 'dark' ? <Sun className="h-4 w-4" /> : <Moon className="h-4 w-4" />}
            </button>
          </div>
        </div>

        {/* Nav */}
        <div className="flex min-h-0 shrink flex-col gap-0.5 overflow-y-auto p-2 border-b border-gray-200 dark:border-gray-800">
          {/* 智能体 */}
          <button
            onClick={() => setPage('agents')}
            className={`flex items-center gap-2.5 rounded-lg px-3 py-2 text-sm font-medium transition-colors text-left ${
              page === 'agents'
                ? 'bg-blue-50 text-blue-600 dark:bg-blue-600/20 dark:text-blue-400'
                : 'text-gray-500 hover:bg-gray-100 hover:text-gray-800 dark:text-gray-400 dark:hover:bg-gray-800 dark:hover:text-gray-200'
            }`}
          >
            <Bot className="h-4 w-4" />{t('nav.agents')}
          </button>

          {/* 记忆中心（分组） */}
          {(() => {
            const isActive = MEMORY_PAGES.includes(page as typeof MEMORY_PAGES[number])
            const isExpanded = expandedGroups.has('memory')
            return (
              <div>
                <button
                  onClick={() => {
                    setExpandedGroups(prev => {
                      const next = new Set(prev)
                      if (next.has('memory')) { next.delete('memory') } else { next.add('memory') }
                      return next
                    })
                    if (!isActive) setPage('memory')
                  }}
                  className={`flex w-full items-center gap-2.5 rounded-lg px-3 py-2 text-sm font-medium transition-colors text-left ${
                    isActive
                      ? 'bg-blue-50 text-blue-600 dark:bg-blue-600/20 dark:text-blue-400'
                      : 'text-gray-500 hover:bg-gray-100 hover:text-gray-800 dark:text-gray-400 dark:hover:bg-gray-800 dark:hover:text-gray-200'
                  }`}
                >
                  <Brain className="h-4 w-4" />{t('nav.memory')}
                  {isExpanded
                    ? <ChevronDown className="ml-auto h-3.5 w-3.5 opacity-50" />
                    : <ChevronRight className="ml-auto h-3.5 w-3.5 opacity-50" />}
                </button>
                {isExpanded && (
                  <div className="mt-0.5 flex flex-col gap-0.5">
                    {([
                      { id: 'memory' as NavPage, label: t('nav.memoryOverview'), onClick: openMemory },
                      { id: 'pending-memories' as NavPage, label: t('nav.memoryPending'), onClick: openPendingMemories },
                      { id: 'organized-conversations' as NavPage, label: t('nav.memoryOrganized'), onClick: openOrganizedConversations },
                      { id: 'memory-injection' as NavPage, label: t('nav.memoryInjection'), onClick: openMemoryInjection },
                    ]).map(sub => (
                      <button
                        key={sub.id}
                        onClick={sub.onClick}
                        className={`rounded-lg py-1.5 pl-9 pr-3 text-left text-[13px] transition-colors ${
                          page === sub.id
                            ? 'bg-blue-50 text-blue-600 font-medium dark:bg-blue-600/20 dark:text-blue-400'
                            : 'text-gray-500 hover:bg-gray-100 hover:text-gray-800 dark:text-gray-400 dark:hover:bg-gray-800 dark:hover:text-gray-200'
                        }`}
                      >
                        {sub.label}
                      </button>
                    ))}
                  </div>
                )}
              </div>
            )
          })()}

          {/* Skill 库（分组） */}
          {(() => {
            const isActive = SKILL_PAGES.includes(page as typeof SKILL_PAGES[number])
            const isExpanded = expandedGroups.has('skills')
            return (
              <div>
                <button
                  onClick={() => {
                    setExpandedGroups(prev => {
                      const next = new Set(prev)
                      if (next.has('skills')) { next.delete('skills') } else { next.add('skills') }
                      return next
                    })
                    if (!isActive) setPage('skills')
                  }}
                  className={`flex w-full items-center gap-2.5 rounded-lg px-3 py-2 text-sm font-medium transition-colors text-left ${
                    isActive
                      ? 'bg-blue-50 text-blue-600 dark:bg-blue-600/20 dark:text-blue-400'
                      : 'text-gray-500 hover:bg-gray-100 hover:text-gray-800 dark:text-gray-400 dark:hover:bg-gray-800 dark:hover:text-gray-200'
                  }`}
                >
                  <BookOpenText className="h-4 w-4" />{t('nav.skills')}
                  {isExpanded
                    ? <ChevronDown className="ml-auto h-3.5 w-3.5 opacity-50" />
                    : <ChevronRight className="ml-auto h-3.5 w-3.5 opacity-50" />}
                </button>
                {isExpanded && (
                  <div className="mt-0.5 flex flex-col gap-0.5">
                    {([
                      { id: 'skills' as NavPage, label: t('nav.skillsLocal'), onClick: openSkills },
                      { id: 'published-skills' as NavPage, label: t('nav.skillsPublished'), onClick: openPublishedSkills },
                      { id: 'skill-marketplace' as NavPage, label: t('nav.skillsMarketplace'), onClick: openSkillMarketplace },
                    ]).map(sub => (
                      <button
                        key={sub.id}
                        onClick={sub.onClick}
                        className={`rounded-lg py-1.5 pl-9 pr-3 text-left text-[13px] transition-colors ${
                          page === sub.id
                            ? 'bg-blue-50 text-blue-600 font-medium dark:bg-blue-600/20 dark:text-blue-400'
                            : 'text-gray-500 hover:bg-gray-100 hover:text-gray-800 dark:text-gray-400 dark:hover:bg-gray-800 dark:hover:text-gray-200'
                        }`}
                      >
                        {sub.label}
                      </button>
                    ))}
                  </div>
                )}
              </div>
            )
          })()}

          {/* MCP 服务 */}
          <button
            onClick={() => setPage('mcp-library')}
            className={`flex items-center gap-2.5 rounded-lg px-3 py-2 text-sm font-medium transition-colors text-left ${
              page === 'mcp-library'
                ? 'bg-blue-50 text-blue-600 dark:bg-blue-600/20 dark:text-blue-400'
                : 'text-gray-500 hover:bg-gray-100 hover:text-gray-800 dark:text-gray-400 dark:hover:bg-gray-800 dark:hover:text-gray-200'
            }`}
          >
            <Plug className="h-4 w-4" />{t('nav.mcpLibrary')}
          </button>

          <button
            type="button"
            onClick={openUsage}
            aria-current={page === 'usage' ? 'page' : undefined}
            className={`flex items-center gap-2.5 rounded-lg px-3 py-2 text-left text-sm font-medium transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 ${
              page === 'usage'
                ? 'bg-blue-50 text-blue-600 dark:bg-blue-600/20 dark:text-blue-400'
                : 'text-gray-500 hover:bg-gray-100 hover:text-gray-800 dark:text-gray-400 dark:hover:bg-gray-800 dark:hover:text-gray-200'
            }`}
          >
            <BarChart3 className="h-4 w-4" />{t('nav.usage')}
          </button>

          {/* 分隔线 */}
          <div className="my-1 border-t border-gray-200 dark:border-gray-700" />

          {/* 工具区 */}
          {([
            { id: 'ports' as NavPage,    icon: <Network className="h-4 w-4" />,   label: t('nav.ports') },
            { id: 'network' as NavPage,  icon: <Gauge className="h-4 w-4" />,     label: t('nav.network') },
            { id: 'env-vars' as NavPage, icon: <Variable className="h-4 w-4" />,  label: t('nav.envVars') },
            { id: 'proxy' as NavPage,    icon: <Shield className="h-4 w-4" />,    label: t('nav.proxy') },
            { id: 'settings' as NavPage, icon: <Settings2 className="h-4 w-4" />, label: t('nav.settings') },
          ]).map(nav => (
            <button
              key={nav.id}
              onClick={() => setPage(nav.id)}
              className={`flex items-center gap-2.5 rounded-lg px-3 py-2 text-sm font-medium transition-colors text-left ${
                page === nav.id
                  ? 'bg-blue-50 text-blue-600 dark:bg-blue-600/20 dark:text-blue-400'
                  : 'text-gray-500 hover:bg-gray-100 hover:text-gray-800 dark:text-gray-400 dark:hover:bg-gray-800 dark:hover:text-gray-200'
              }`}
            >
              {nav.icon}{nav.label}
            </button>
          ))}
        </div>

        {/* Agent list */}
        {page === 'agents' && <>
          <div className="flex items-center justify-between px-4 py-2">
            <p className="text-xs text-gray-400">{t('app.configured', { count: agents.length })}</p>
            <div className="flex gap-1">
              <button onClick={openNew} className="rounded p-1 text-gray-400 hover:bg-gray-100 hover:text-gray-600 dark:hover:bg-gray-800 dark:hover:text-gray-300" title={t('common.add')}>
                <Plus className="h-3.5 w-3.5" />
              </button>
              <button onClick={fetchAgents} disabled={loading} className="rounded p-1 text-gray-400 hover:bg-gray-100 hover:text-gray-600 dark:hover:bg-gray-800 dark:hover:text-gray-300 disabled:opacity-40">
                <RefreshCw className={`h-3.5 w-3.5 ${loading ? 'animate-spin' : ''}`} />
              </button>
            </div>
          </div>
          <div className="flex-1 overflow-y-auto">
            <AgentList
              agents={agents}
              selectedId={selectedId}
              onSelect={selectAgent}
              onStart={handleStart}
              onStop={handleStop}
              onDelete={deleteAgent}
              onConfigure={openEdit}
              onReorder={reorderAgents}
            />
          </div>
          <div className="border-t border-gray-200 p-3 dark:border-gray-800">
            <button onClick={openNew} className="flex w-full items-center justify-center gap-2 rounded-xl bg-blue-600 px-3 py-2 text-sm font-medium text-white transition-colors hover:bg-blue-500">
              <Plus className="h-4 w-4" /> {t('app.newAgent')}
            </button>
          </div>
        </>}

        {page !== 'agents' && <div className="flex-1" />}
       {/* Footer: version + update check */}
          <div className="border-t border-gray-200 px-4 py-3 space-y-2 dark:border-gray-800">
            <div className="flex items-center justify-center gap-1 text-xs font-bold text-gray-500 dark:text-gray-400">
              <span>v{appVersion.replace(/-beta.*$/i, ' beta')}</span>
              <span>·</span>
              <a
                href="https://www.zaferliu.me"
                target="_blank"
                rel="noreferrer"
                className="hover:text-gray-700 dark:hover:text-gray-200"
              >
                @ZaferLiu
              </a>
            </div>
            <div className="flex justify-center">
              <UpdateChecker sidebar autoCheck />
            </div>
          </div>
        </aside>

      {/* ── Col-resize handle ──────────────────────────── */}
      <div
        onMouseDown={onColMouseDown}
        className="drag-col"
      />

      {/* ── Main panel ─────────────────────────────────── */}
      <main className="flex flex-1 flex-col overflow-hidden bg-gray-50 dark:bg-gray-950">

        {keptPages.has('memory') && (
          <div className={`flex flex-1 flex-col overflow-hidden ${page === 'memory' ? '' : 'hidden'}`}>
            <ErrorBoundary>
              <MemoryCenter onOpenUsage={openUsage} onOpenPending={openPendingMemories} onOpenOrganized={openOrganizedConversations} onOpenInjection={openMemoryInjection} active={page === 'memory' && documentVisible} />
            </ErrorBoundary>
          </div>
        )}
        {keptPages.has('usage') && (
          <div className={`flex flex-1 flex-col overflow-hidden ${page === 'usage' ? '' : 'hidden'}`}>
            <ErrorBoundary>
              <UsageAnalytics active={page === 'usage' && documentVisible} />
            </ErrorBoundary>
          </div>
        )}
        {keptPages.has('pending-memories') && (
          <div className={`flex flex-1 flex-col overflow-hidden ${page === 'pending-memories' ? '' : 'hidden'}`}>
            <ErrorBoundary>
              <PendingMemories onBack={openMemory} active={page === 'pending-memories' && documentVisible} />
            </ErrorBoundary>
          </div>
        )}
        {keptPages.has('organized-conversations') && (
          <div className={`flex flex-1 flex-col overflow-hidden ${page === 'organized-conversations' ? '' : 'hidden'}`}>
            <ErrorBoundary>
              <OrganizedConversations onBack={openMemory} active={page === 'organized-conversations' && documentVisible} />
            </ErrorBoundary>
          </div>
        )}
        {keptPages.has('memory-injection') && (
          <div className={`flex flex-1 flex-col overflow-hidden ${page === 'memory-injection' ? '' : 'hidden'}`}>
            <ErrorBoundary>
              <MemoryInjection onBack={openMemory} active={page === 'memory-injection' && documentVisible} />
            </ErrorBoundary>
          </div>
        )}
        {keptPages.has('skills') && (
          <div className={`flex flex-1 flex-col overflow-hidden ${page === 'skills' ? '' : 'hidden'}`}>
            <ErrorBoundary>
              <SkillLibrary
                onOpenPublished={openPublishedSkills}
                autoOpenSync={skillsAutoSync}
                onSyncOpened={clearSkillsAutoSync}
              />
            </ErrorBoundary>
          </div>
        )}
        {keptPages.has('published-skills') && (
          <div className={`flex flex-1 flex-col overflow-hidden ${page === 'published-skills' ? '' : 'hidden'}`}>
            <ErrorBoundary>
              <PublishedSkills onBack={openSkills} onGoSync={goSkillSync} />
            </ErrorBoundary>
          </div>
        )}
        {keptPages.has('skill-marketplace') && (
          <div className={`flex flex-1 flex-col overflow-hidden ${page === 'skill-marketplace' ? '' : 'hidden'}`}>
            <ErrorBoundary>
              <SkillMarketplace />
            </ErrorBoundary>
          </div>
        )}
        {keptPages.has('mcp-library') && (
          <div className={`flex flex-1 flex-col overflow-hidden ${page === 'mcp-library' ? '' : 'hidden'}`}>
            <ErrorBoundary>
              <McpLibrary active={page === 'mcp-library' && documentVisible} />
            </ErrorBoundary>
          </div>
        )}
        {page === 'ports' && <PortManager agents={agents} />}
        {page === 'network' && (
          <ErrorBoundary>
            <NetworkManager />
          </ErrorBoundary>
        )}
        {page === 'env-vars' && (
          <ErrorBoundary>
            <EnvManager />
          </ErrorBoundary>
        )}
        {page === 'proxy' && <ProxyManager agents={agents} />}
        {page === 'settings' && !showOnboarding && (
          <ErrorBoundary>
            <Settings onOpenOnboarding={() => setShowOnboarding(true)} />
          </ErrorBoundary>
        )}

        {/* Keep the workspace mounted so terminals and iframe state survive navigation. */}
        <div className={`flex flex-1 flex-col overflow-hidden ${page === 'agents' ? '' : 'hidden'}`}>

            {agentError && (
              <div role="alert" className="shrink-0 border-b border-red-200 bg-red-50 px-5 py-3 text-red-700 dark:border-red-900 dark:bg-red-950/30 dark:text-red-300">
                <div className="flex items-start justify-between gap-3">
                  <p className="text-sm font-medium">{t('recovery.agentFailed', { name: agentError.agent.config.name })}</p>
                  <button type="button" onClick={() => setAgentError(null)} aria-label={t('common.close')} className="rounded p-1 focus-visible:ring-2 focus-visible:ring-blue-500"><X size={14} /></button>
                </div>
                <p className="mt-1 max-h-24 overflow-auto break-words text-xs">{agentError.message}</p>
                <ErrorRecovery error={agentError.message} onRetry={agentError.retry} onConfigure={() => openEdit(agentError.agent)} />
              </div>
            )}

            {/* ── Tab bar ── */}
            {openTabs.length > 0 && (
              <div className="flex items-center gap-0.5 border-b border-gray-200 bg-white px-2 pt-1.5 shrink-0 dark:border-gray-800 dark:bg-gray-900">
                {openTabs.map(tab => {
                  const isActive = selectedId === tab.agentId
                  return (
                    <button
                      key={`${tab.agentId}-${tab.kind}`}
                      onClick={() => { selectAgent(tab.agentId); setActiveTabKey(`${tab.agentId}:${tab.kind}`); }}
                      className={`group flex items-center gap-1.5 rounded-t-md border border-b-0 px-3 py-1.5 text-xs font-medium transition-colors ${
                        isActive && activeTab?.kind === tab.kind
                          ? tab.kind === 'terminal'
                            ? 'border-gray-200 bg-gray-50 text-green-600 dark:border-gray-700 dark:bg-gray-950 dark:text-green-400'
                            : 'border-gray-200 bg-gray-50 text-blue-600 dark:border-gray-700 dark:bg-gray-950 dark:text-blue-400'
                          : 'border-transparent text-gray-400 hover:bg-gray-100 hover:text-gray-600 dark:text-gray-500 dark:hover:bg-gray-800 dark:hover:text-gray-300'
                      }`}
                    >
                      {tab.kind === 'terminal'
                        ? <TerminalSquare className="h-3 w-3 shrink-0" />
                        : <Globe className="h-3 w-3 shrink-0" />
                      }
                      <span className="max-w-[100px] truncate">{tab.label}</span>
                      <span className="ml-0.5 text-[10px] text-gray-400 dark:text-gray-600">
                        {tab.kind === 'terminal' ? t('app.term') : `${tab.port}`}
                      </span>
                      <span
                        onClick={e => closeTab(tab, e)}
                        className="ml-1 rounded p-0.5 opacity-0 hover:bg-gray-200 hover:text-red-500 group-hover:opacity-100 dark:hover:bg-gray-700 dark:hover:text-red-400"
                      >
                        <X className="h-3 w-3" />
                      </span>
                    </button>
                  )
                })}
              </div>
            )}

            {/* ── Active terminal or embedded browser panel ── */}
            {showPanel && activeTab && (
              <div
                style={panelFullscreen ? undefined : { height: panelHeight }}
                className={`shrink-0 flex flex-col ${panelFullscreen ? 'flex-1' : ''}`}
              >
                {/* Panel toolbar */}
                <div className="flex items-center gap-2 border-b border-gray-200 bg-white px-3 py-1 shrink-0 dark:border-gray-800 dark:bg-gray-900">
                  {activeTab.kind === 'terminal'
                    ? <TerminalSquare className="h-3.5 w-3.5 shrink-0 text-green-500" />
                    : <Globe className="h-3.5 w-3.5 shrink-0 text-blue-500 dark:text-blue-400" />
                  }
                  <span className="flex-1 font-mono text-xs text-gray-500 dark:text-gray-400">
                    {activeTab.kind === 'terminal'
                      ? `${activeTab.command} ${(activeTab.args ?? []).join(' ')}`
                      : `http://127.0.0.1:${activeTab.port}`
                    }
                  </span>
                  {activeTab.kind === 'terminal' && (
                    <button
                      onClick={clearActiveTerminal}
                      className="rounded p-1 text-gray-400 hover:bg-gray-100 hover:text-gray-600 dark:hover:bg-gray-700 dark:hover:text-gray-300"
                      title={t('common.clear')}
                    >
                      <Eraser className="h-3.5 w-3.5" />
                    </button>
                  )}
                  <button
                    onClick={togglePanelFullscreen}
                    className="rounded p-1 text-gray-400 hover:bg-gray-100 hover:text-gray-600 dark:hover:bg-gray-700 dark:hover:text-gray-300"
                    title={activeTab.kind === 'terminal' && panelFullscreen ? t('common.restore') : t('common.fullscreen')}
                  >
                    {activeTab.kind === 'terminal' && panelFullscreen
                      ? <Minimize2 className="h-3.5 w-3.5" />
                      : <Maximize2 className="h-3.5 w-3.5" />
                    }
                  </button>
                  <button
                    onClick={e => closeTab(activeTab, e)}
                    className="rounded p-1 text-gray-400 hover:bg-gray-100 hover:text-red-500 dark:hover:bg-gray-700 dark:hover:text-red-400"
                    title={t('common.close')}
                  >
                    <X className="h-3.5 w-3.5" />
                  </button>
                </div>

                {/* Panel content */}
                <div className="flex-1 overflow-hidden">
                  {openTabs.map(tab => {
                    const tabKey = `${tab.agentId}:${tab.kind}`
                    const isActive = tabKey === resolvedActiveTabKey
                    return (
                      <div
                        key={tabKey}
                        className={`h-full w-full ${isActive ? '' : 'hidden'}`}
                      >
                        {tab.kind === 'terminal' ? (
                          <TerminalPanel
                            id={tab.agentId}
                            command={tab.command!}
                            args={tab.args ?? []}
                            cwd={tab.cwd ?? ''}
                            env={tab.env ?? {}}
                            active={isActive && page === 'agents'}
                            clearVersion={terminalClearVersions[tab.agentId] ?? 0}
                          />
                        ) : (
                          <NativeWebviewPanel
                            tab={tab}
                            active={isActive && page === 'agents' && !showForm && !showOnboarding}
                          />
                        )}
                      </div>
                    )
                  })}
                </div>
              </div>
            )}

            {/* ── Row-resize handle ── */}
            {showSplit && (
              <div
                onMouseDown={onRowMouseDown}
                className="drag-row"
              />
            )}

            {/* ── Agent detail ── */}
            {!panelFullscreen && (
              <div className="flex-1 overflow-hidden">
                {selectedAgent ? (
                  <AgentDetail
                    agent={selectedAgent}
                    logs={agentLogs}
                    onConfigure={() => openEdit(selectedAgent)}
                    onStart={handleStart}
                    onStop={handleStop}
                    onOpenUI={openAgentUI}
                    onOpenTerminal={openAgentTerminal}
                    uiIsOpen={uiIsOpen}
                    termIsOpen={termIsOpen}
                  />
                ) : (
                  <div className="flex h-full flex-col items-center justify-center gap-3">
                    <div className="flex h-16 w-16 items-center justify-center rounded-2xl bg-gray-100 dark:bg-gray-800">
                      <img src={logoUrl} alt="" className="h-10 w-10 opacity-40" />
                    </div>
                    <p className="text-sm text-gray-400 dark:text-gray-500">{t('app.selectAgentHint')}</p>
                    <button onClick={openNew} className="text-sm text-blue-500 hover:text-blue-400 dark:text-blue-400 dark:hover:text-blue-300">
                      {t('app.createFirstAgent')}
                    </button>
                  </div>
                )}
              </div>
            )}

        </div>
      </main>

      {showForm && (
        <AgentForm
          initial={editingAgent?.config}
          onSave={saveAgent}
          onClose={() => setShowForm(false)}
        />
      )}

      {showOnboarding && <Onboarding onFinish={() => setShowOnboarding(false)} />}
    </div>
    </RecoveryNavigationContext.Provider>
  )
}
