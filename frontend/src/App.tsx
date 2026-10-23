import { useCallback, useEffect, useId, useRef, useState, type ReactNode } from 'react'
import { useTranslation } from 'react-i18next'
import { invoke } from '@tauri-apps/api/core'
import { LanguageSwitcher } from './components/LanguageSwitcher'
import { PortManager } from './pages/PortManager'
import { NetworkManager } from './pages/NetworkManager'
import { EnvManager } from './pages/EnvManager'
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
import { MemoryCapture } from './pages/MemoryCapture'
import { MemoryHelp } from './pages/MemoryHelp'
import { ErrorBoundary } from './components/ErrorBoundary'
import { RecoveryNavigationContext, type RecoveryTarget } from './components/ErrorRecovery'
import { useDocumentVisible } from './hooks/useVisiblePolling'
import { UpdateChecker } from './components/UpdateChecker'
import { Onboarding, needsOnboarding } from './components/Onboarding'
import { useTheme } from './theme'
import { NavigationSearch, type NavigationDestination } from './components/NavigationSearch'
import { usePageMotion } from './hooks/usePageMotion'
import { setLanguage } from './i18n'
import { useResizable } from './hooks/useResizable'
import { Sun, Moon, Settings2, Brain, BookOpenText, Plug, Wrench, ChevronRight, BarChart3, Search, PanelLeftClose, PanelLeftOpen, Home } from 'lucide-react'
import logoUrl from '/logo.png'
import { CloseBehaviorDialog } from './features/desktop/CloseBehaviorDialog'

type NavPage = 'ports' | 'network' | 'env-vars' | 'settings' | 'settings-general' | 'settings-llm' | 'settings-memory' | 'settings-cloud' | 'memory' | 'skills' | 'published-skills' | 'skill-marketplace' | 'mcp-library' | 'mcp-tools' | 'mcp-calls' | 'mcp-errors' | 'mcp-help' | 'usage' | 'pending-memories' | 'organized-conversations' | 'memory-capture' | 'memory-injection' | 'memory-help'

const MEMORY_PAGES = ['memory', 'memory-capture', 'memory-injection', 'memory-help', 'pending-memories', 'organized-conversations'] as const
// Skill 库两页同样「访问过即保持挂载」：重进保留筛选/勾选状态，也免去整树重挂载。
const SKILL_PAGES = ['skills', 'published-skills', 'skill-marketplace'] as const
const MCP_PAGES = ['mcp-library', 'mcp-tools', 'mcp-calls', 'mcp-errors', 'mcp-help'] as const
const TOOL_PAGES = ['ports', 'network', 'env-vars'] as const
const SETTINGS_PAGES = ['settings-general', 'settings-llm', 'settings-memory', 'settings-cloud'] as const
const KEPT_PAGES: readonly string[] = [...MEMORY_PAGES, ...SKILL_PAGES, 'usage']

export default function App() {
  const { theme, toggle: toggleTheme } = useTheme()
  const { t } = useTranslation()

  const [page, setPage] = useState<NavPage>('memory')
  const [navigationOpen, setNavigationOpen] = useState(false)
  const [compactNavigation, setCompactNavigation] = useState(() => localStorage.getItem('ui.navCompact') === 'true')
  const pageHost = usePageMotion(page)
  useEffect(() => { localStorage.setItem('ui.navCompact', String(compactNavigation)) }, [compactNavigation])
  useEffect(() => {
    const shortcut = (event: KeyboardEvent) => {
      if (event.defaultPrevented) return
      const modal = document.querySelector('dialog[open], [role="dialog"][aria-modal="true"]')
      if (modal && !modal.classList.contains('app-command-dialog')) return
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'k') { event.preventDefault(); setNavigationOpen(value => !value) }
    }
    window.addEventListener('keydown', shortcut)
    return () => window.removeEventListener('keydown', shortcut)
  }, [])
  const documentVisible = useDocumentVisible()
  const openRecoveryTarget = useCallback((target: RecoveryTarget) => setPage(target), [])
  // 记忆族页面访问过即保持挂载：返回时不再
  // 重新挂载触发全量数据重查，消除往返卡顿。未激活时仅 CSS 隐藏。
  const [keptPages, setKeptPages] = useState<Set<string>>(new Set())
  useEffect(() => {
    const keepKey = MCP_PAGES.includes(page as typeof MCP_PAGES[number]) ? 'mcp-library' : page
    if (KEPT_PAGES.includes(keepKey) || keepKey === 'mcp-library') {
      setKeptPages((previous) => previous.has(keepKey) ? previous : new Set(previous).add(keepKey))
    }
  }, [page])
  const [showOnboarding, setShowOnboarding] = useState(needsOnboarding)
  // 从「已发布」页跳入 Skill 库时自动打开同步对话框
  const [skillsAutoSync, setSkillsAutoSync] = useState(false)
  // Preserve the remaining module navigation preferences.
  const [expandedGroups, setExpandedGroups] = useState<Set<string>>(() => {
    try { const saved = JSON.parse(localStorage.getItem('ui.navGroups') ?? 'null'); if (Array.isArray(saved)) return new Set(saved.filter((value): value is string => typeof value === 'string' && ['memory', 'skills', 'mcp', 'toolbox', 'settings'].includes(value))) } catch { /* Start with collapsed navigation groups. */ }
    return new Set()
  })
  useEffect(() => { localStorage.setItem('ui.navGroups', JSON.stringify([...expandedGroups])) }, [expandedGroups])
  const previousNavigationPage = useRef(page)
  // Expand a family after navigation while allowing manual collapse.
  useEffect(() => {
    if (previousNavigationPage.current === page) return
    previousNavigationPage.current = page
    const groupMap: Record<string, string> = {}
    for (const p of MEMORY_PAGES) groupMap[p] = 'memory'
    for (const p of SKILL_PAGES) groupMap[p] = 'skills'
    for (const p of MCP_PAGES) groupMap[p] = 'mcp'
    for (const p of TOOL_PAGES) groupMap[p] = 'toolbox'
    for (const p of SETTINGS_PAGES) groupMap[p] = 'settings'
    const group = groupMap[page]
    if (group) {
      setExpandedGroups(prev => prev.has(group) ? prev : new Set(prev).add(group))
    }
  }, [page])

  // Stable callbacks preserve mounted memory and skill page state.
  const openMemory = useCallback(() => setPage('memory'), [])
  const openUsage = useCallback(() => setPage('usage'), [])
  const openPendingMemories = useCallback(() => setPage('pending-memories'), [])
  const openOrganizedConversations = useCallback(() => setPage('organized-conversations'), [])
  const openMemoryInjection = useCallback(() => setPage('memory-injection'), [])
  const openMemoryCapture = useCallback(() => setPage('memory-capture'), [])
  const openMemoryHelp = useCallback(() => setPage('memory-help'), [])
  const openSkills = useCallback(() => setPage('skills'), [])
  const openPublishedSkills = useCallback(() => setPage('published-skills'), [])
  const openSkillMarketplace = useCallback(() => setPage('skill-marketplace'), [])
  const openSettingsGeneral = useCallback(() => setPage('settings-general'), [])
  const openSettingsLlm = useCallback(() => setPage('settings-llm'), [])
  const openSettingsMemory = useCallback(() => setPage('settings-memory'), [])
  const openSettingsCloud = useCallback(() => setPage('settings-cloud'), [])

  /** 折叠分组：切换展开；若当前不在该分组内，顺手跳到它的入口页。 */
  const toggleNavGroup = (group: string, pages: readonly string[], entry: NavPage) => {
    setExpandedGroups(prev => {
      const next = new Set(prev)
      if (next.has(group)) { next.delete(group) } else { next.add(group) }
      return next
    })
    if (!pages.includes(page)) setPage(entry)
  }
  const clearSkillsAutoSync = useCallback(() => setSkillsAutoSync(false), [])
  const goSkillSync = useCallback(() => { setSkillsAutoSync(true); setPage('skills') }, [])

  const { width: sidebarWidth, onColMouseDown } = useResizable({
    minW: 216, maxW: 360, defaultW: 240,
    minH: 120, maxH: 800, defaultH: 380,
  })

  // Legacy launch/control flags are no longer consumed; old pages fall back to Memory.
  useEffect(() => {
    let cancelled = false
    invoke<{ page?: string; lang?: string }>('get_startup_options').then(options => {
      if (cancelled) return
      if (options.lang === 'zh' || options.lang === 'en') setLanguage(options.lang)
      const pages: readonly string[] = [...MEMORY_PAGES, ...SKILL_PAGES, ...MCP_PAGES, ...TOOL_PAGES, ...SETTINGS_PAGES, 'usage', 'settings']
      if (options.page && pages.includes(options.page)) setPage(options.page as NavPage)
    }).catch(() => {})
    return () => { cancelled = true }
  }, [])

  // Legacy 'settings' page redirects to the first settings sub-page.
  useEffect(() => { if (page === 'settings') setPage('settings-general') }, [page])

  const navigationFamilies: Array<{ group: string; icon: ReactNode; pages: Array<[NavPage, string]> }> = [
    { group: t('nav.memory'), icon: <Brain size={17} />, pages: [['memory', 'nav.memoryOverview'], ['memory-capture', 'nav.memoryCapture'], ['memory-injection', 'nav.memoryInjection'], ['pending-memories', 'nav.memoryPending'], ['organized-conversations', 'nav.memoryOrganized'], ['memory-help', 'nav.memoryHelp']] },
    { group: t('nav.skills'), icon: <BookOpenText size={17} />, pages: [['skills', 'nav.skillsLocal'], ['published-skills', 'nav.skillsPublished'], ['skill-marketplace', 'nav.skillsMarketplace']] },
    { group: t('nav.mcpLibrary'), icon: <Plug size={17} />, pages: [['mcp-library', 'nav.mcpServices'], ['mcp-tools', 'nav.mcpTools'], ['mcp-calls', 'nav.mcpCalls'], ['mcp-errors', 'nav.mcpErrors'], ['mcp-help', 'nav.mcpHelp']] },
    { group: t('nav.usage'), icon: <BarChart3 size={17} />, pages: [['usage', 'nav.usage']] },
    { group: t('nav.toolbox'), icon: <Wrench size={17} />, pages: [['ports', 'nav.ports'], ['network', 'nav.network'], ['env-vars', 'nav.envVars']] },
    { group: t('nav.settings'), icon: <Settings2 size={17} />, pages: [['settings-general', 'nav.settingsGeneral'], ['settings-llm', 'nav.settingsLlm'], ['settings-memory', 'nav.settingsMemory'], ['settings-cloud', 'nav.settingsCloud']] },
  ]
  const destinations: NavigationDestination[] = navigationFamilies.flatMap(family => family.pages.map(([id, label]) => ({ id, label: t(label), group: family.group, icon: family.icon })))
  const currentDestination = destinations.find(destination => destination.id === page)!

  return (
    <RecoveryNavigationContext.Provider value={openRecoveryTarget}>
    <div className="fluent-app flex h-screen overflow-hidden">
      <a className="fluent-skip-link" href="#workspace-main">{t('shell.skipToContent')}</a>

      {/* ── Sidebar ─────────────────────────────────────── */}
      <aside
        style={{ width: compactNavigation ? 64 : sidebarWidth }}
        data-compact={compactNavigation}
        className="app-sidebar flex shrink-0 flex-col"
      >
        <div className="app-brand">
          <img src={logoUrl} alt={t('app.title')} className="app-brand-logo" />
          <div className="app-brand-copy"><p className="app-brand-name">Vibe Assistant</p><p className="app-brand-caption">{t('shell.workspace')}</p></div>
          <button type="button" onClick={() => setCompactNavigation(value => !value)} className="app-icon-button" title={t(compactNavigation ? 'shell.expandNavigation' : 'shell.collapseNavigation')} aria-label={t(compactNavigation ? 'shell.expandNavigation' : 'shell.collapseNavigation')} aria-expanded={!compactNavigation}>
            {compactNavigation ? <PanelLeftOpen size={17} /> : <PanelLeftClose size={17} />}
          </button>
        </div>
        <button type="button" className="app-search-button" onClick={() => setNavigationOpen(true)} aria-label={t('shell.searchPages')} title={t('shell.searchPages')}><Search size={15} /><span>{t('shell.searchPages')}</span><kbd>Ctrl K</kbd></button>
        <nav className="app-navigation" aria-label={t('shell.primaryNavigation')}>
          <SidebarNavGroup
            icon={<Brain className="h-4 w-4" />}
            label={t('nav.memory')}
            active={MEMORY_PAGES.includes(page as typeof MEMORY_PAGES[number])}
            expanded={expandedGroups.has('memory')}
            onToggle={() => toggleNavGroup('memory', MEMORY_PAGES, 'memory')}
          >
            {([
              { id: 'memory' as NavPage, label: t('nav.memoryOverview'), onClick: openMemory },
              { id: 'memory-capture' as NavPage, label: t('nav.memoryCapture'), onClick: openMemoryCapture },
              { id: 'memory-injection' as NavPage, label: t('nav.memoryInjection'), onClick: openMemoryInjection },
              { id: 'pending-memories' as NavPage, label: t('nav.memoryPending'), onClick: openPendingMemories },
              { id: 'organized-conversations' as NavPage, label: t('nav.memoryOrganized'), onClick: openOrganizedConversations },
              { id: 'memory-help' as NavPage, label: t('nav.memoryHelp'), onClick: openMemoryHelp },
            ]).map(sub => (
              <SidebarNavItem
                key={sub.id}
                label={sub.label}
                active={page === sub.id}
                onClick={sub.onClick}
                indent
              />
            ))}
          </SidebarNavGroup>

          <SidebarNavGroup
            icon={<BookOpenText className="h-4 w-4" />}
            label={t('nav.skills')}
            active={SKILL_PAGES.includes(page as typeof SKILL_PAGES[number])}
            expanded={expandedGroups.has('skills')}
            onToggle={() => toggleNavGroup('skills', SKILL_PAGES, 'skills')}
          >
            {([
              { id: 'skills' as NavPage, label: t('nav.skillsLocal'), onClick: openSkills },
              { id: 'published-skills' as NavPage, label: t('nav.skillsPublished'), onClick: openPublishedSkills },
              { id: 'skill-marketplace' as NavPage, label: t('nav.skillsMarketplace'), onClick: openSkillMarketplace },
            ]).map(sub => (
              <SidebarNavItem
                key={sub.id}
                label={sub.label}
                active={page === sub.id}
                onClick={sub.onClick}
                indent
              />
            ))}
          </SidebarNavGroup>

          <SidebarNavGroup
            icon={<Plug className="h-4 w-4" />}
            label={t('nav.mcpLibrary')}
            active={MCP_PAGES.includes(page as typeof MCP_PAGES[number])}
            expanded={expandedGroups.has('mcp')}
            onToggle={() => toggleNavGroup('mcp', MCP_PAGES, 'mcp-library')}
          >
            {([
              { id: 'mcp-library' as NavPage, label: t('nav.mcpServices') },
              { id: 'mcp-tools' as NavPage, label: t('nav.mcpTools') },
              { id: 'mcp-calls' as NavPage, label: t('nav.mcpCalls') },
              { id: 'mcp-errors' as NavPage, label: t('nav.mcpErrors') },
              { id: 'mcp-help' as NavPage, label: t('nav.mcpHelp') },
            ]).map(sub => <SidebarNavItem key={sub.id} label={sub.label} active={page === sub.id} onClick={() => setPage(sub.id)} indent />)}
          </SidebarNavGroup>

          <SidebarNavItem
            icon={<BarChart3 className="h-4 w-4" />}
            label={t('nav.usage')}
            active={page === 'usage'}
            onClick={openUsage}
          />


          {/* 分隔线：以上为主功能，以下为工具 */}
          <div className="my-1 border-t border-gray-200 dark:border-gray-700" />

          <SidebarNavGroup
            icon={<Wrench className="h-4 w-4" />}
            label={t('nav.toolbox')}
            active={TOOL_PAGES.includes(page as typeof TOOL_PAGES[number])}
            expanded={expandedGroups.has('toolbox')}
            onToggle={() => toggleNavGroup('toolbox', TOOL_PAGES, 'ports')}
          >
            {([
              { id: 'ports' as NavPage, label: t('nav.ports') },
              { id: 'network' as NavPage, label: t('nav.network') },
              { id: 'env-vars' as NavPage, label: t('nav.envVars') },
            ]).map(sub => <SidebarNavItem key={sub.id} label={sub.label} active={page === sub.id} onClick={() => setPage(sub.id)} indent />)}
          </SidebarNavGroup>
          <SidebarNavGroup
            icon={<Settings2 className="h-4 w-4" />}
            label={t('nav.settings')}
            active={SETTINGS_PAGES.includes(page as typeof SETTINGS_PAGES[number]) || page === 'settings'}
            expanded={expandedGroups.has('settings')}
            onToggle={() => toggleNavGroup('settings', SETTINGS_PAGES, 'settings-general')}
          >
            {([
              { id: 'settings-general' as NavPage, label: t('nav.settingsGeneral'), onClick: openSettingsGeneral },
              { id: 'settings-llm' as NavPage, label: t('nav.settingsLlm'), onClick: openSettingsLlm },
              { id: 'settings-memory' as NavPage, label: t('nav.settingsMemory'), onClick: openSettingsMemory },
              { id: 'settings-cloud' as NavPage, label: t('nav.settingsCloud'), onClick: openSettingsCloud },
            ]).map(sub => (
              <SidebarNavItem key={sub.id} label={sub.label} active={page === sub.id} onClick={sub.onClick} indent />
            ))}
          </SidebarNavGroup>
        </nav>

        <div className="app-sidebar-footer">
          <div className="app-sidebar-footer-meta"><span>v1.1.0</span><a href="https://www.zaferliu.me" target="_blank" rel="noreferrer">@ZaferLiu</a>
            <div className="app-sidebar-footer-controls"><LanguageSwitcher /><button type="button" onClick={toggleTheme} className="app-icon-button" title={theme === 'dark' ? t('app.switchToLight') : t('app.switchToDark')} aria-label={theme === 'dark' ? t('app.switchToLight') : t('app.switchToDark')}>{theme === 'dark' ? <Sun size={16} /> : <Moon size={16} />}</button></div>
          </div>
          <div className="app-sidebar-footer-update"><UpdateChecker sidebar autoCheck /></div>
        </div>
        </aside>

      {/* ── Col-resize handle ──────────────────────────── */}
      <div
        onMouseDown={onColMouseDown}
        className={`drag-col ${compactNavigation ? 'hidden' : ''}`}
      />

      {/* ── Main panel ─────────────────────────────────── */}
      <main id="workspace-main" tabIndex={-1} className="app-content">
        <div className="app-contextbar"><Home size={14} aria-hidden="true" /><span>{currentDestination.group}</span>{currentDestination.group !== currentDestination.label && <><ChevronRight size={12} aria-hidden="true" /><span className="app-contextbar-current">{currentDestination.label}</span></>}<button type="button" onClick={() => setNavigationOpen(true)} className="app-contextbar-search" aria-label={t('shell.searchPages')}><Search size={13} /><span>Ctrl K</span></button></div>
        <div className="app-page-host" data-page={page} ref={pageHost}>

        {keptPages.has('memory') && (
          <div className={`flex flex-1 flex-col overflow-hidden ${page === 'memory' ? '' : 'hidden'}`}>
            <ErrorBoundary>
              <MemoryCenter onOpenUsage={openUsage} onOpenPending={openPendingMemories} onOpenOrganized={openOrganizedConversations} onOpenCapture={openMemoryCapture} onOpenInjection={openMemoryInjection} active={page === 'memory' && documentVisible} />
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
              <MemoryInjection onBack={openMemory} onOpenCapture={openMemoryCapture} active={page === 'memory-injection' && documentVisible} />
            </ErrorBoundary>
          </div>
        )}
        {keptPages.has('memory-capture') && (
          <div className={`flex flex-1 flex-col overflow-hidden ${page === 'memory-capture' ? '' : 'hidden'}`}>
            <ErrorBoundary>
              <MemoryCapture onBack={openMemory} onOpenInjection={openMemoryInjection} active={page === 'memory-capture' && documentVisible} />
            </ErrorBoundary>
          </div>
        )}
        {keptPages.has('memory-help') && (
          <div className={`flex flex-1 flex-col overflow-hidden ${page === 'memory-help' ? '' : 'hidden'}`}>
            <ErrorBoundary>
              <MemoryHelp onBack={openMemory} />
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
          <div className={`flex flex-1 flex-col overflow-hidden ${MCP_PAGES.includes(page as typeof MCP_PAGES[number]) ? '' : 'hidden'}`}>
            <ErrorBoundary>
              <McpLibrary
                active={MCP_PAGES.includes(page as typeof MCP_PAGES[number]) && documentVisible}
                section={page === 'mcp-tools' ? 'tools' : page === 'mcp-calls' ? 'calls' : page === 'mcp-errors' ? 'errors' : page === 'mcp-help' ? 'help' : 'services'}
                onSectionChange={section => setPage({ services: 'mcp-library', tools: 'mcp-tools', calls: 'mcp-calls', errors: 'mcp-errors', help: 'mcp-help' }[section] as NavPage)}
              />
            </ErrorBoundary>
          </div>
        )}
        {page === 'ports' && <PortManager />}
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
        {(SETTINGS_PAGES.includes(page as typeof SETTINGS_PAGES[number]) || page === 'settings') && !showOnboarding && (
          <ErrorBoundary>
            <Settings
              section={page === 'settings' ? 'general' : page === 'settings-llm' ? 'llm' : page === 'settings-memory' ? 'memory' : page === 'settings-cloud' ? 'cloud' : 'general'}
              onOpenOnboarding={() => setShowOnboarding(true)}
            />
          </ErrorBoundary>
        )}

        </div>
      </main>

      <NavigationSearch destinations={destinations} onNavigate={id => setPage(id as NavPage)} open={navigationOpen} onClose={() => setNavigationOpen(false)} />
      {showOnboarding && <Onboarding onFinish={() => setShowOnboarding(false)} onNavigate={setPage} />}
      <CloseBehaviorDialog />
    </div>
    </RecoveryNavigationContext.Provider>
  )
}

// ---------- 侧边栏零件 ----------

/**
 * 侧边栏导航项。选中态、悬浮态、缩进态只有这一份定义——此前每个导航按钮都
 * 各自复制了一整串三元表达式，新增一项要同步改五处。
 */
function SidebarNavItem({
  icon,
  label,
  active,
  onClick,
  indent = false,
  trailing,
  expanded,
  controls,
}: {
  icon?: ReactNode
  label: string
  active: boolean
  onClick: () => void
  /** 分组里的子项：字号更细、缩进更深。 */
  indent?: boolean
  /** 右侧附加内容，如展开箭头。 */
  trailing?: ReactNode
  expanded?: boolean
  controls?: string
}) {
  return <button type="button" onClick={onClick} title={label} aria-current={active && expanded === undefined ? 'page' : undefined} aria-expanded={expanded} aria-controls={controls} data-active={active && expanded === undefined} data-current-group={active && expanded !== undefined} data-indent={indent} className="app-nav-item">
    {active && expanded === undefined && <span className="app-nav-indicator" aria-hidden="true" />}
    {icon}<span className="app-nav-label truncate">{label}</span>
    {trailing && <span className="app-nav-trailing">{trailing}</span>}
  </button>
}

/** 可折叠的导航分组：展开箭头与子项的缩进竖线由这里统一给出。 */
function SidebarNavGroup({
  icon,
  label,
  active,
  expanded,
  onToggle,
  children,
}: {
  icon: ReactNode
  label: string
  active: boolean
  expanded: boolean
  onToggle: () => void
  children: ReactNode
}) {
  const groupId = useId()
  return <div className="app-nav-group">
    <SidebarNavItem icon={icon} label={label} active={active} expanded={expanded} controls={groupId} onClick={onToggle} trailing={<ChevronRight className={`h-3 w-3 transition-transform motion-reduce:transition-none ${expanded ? 'rotate-90' : ''}`} />} />
    <div id={groupId} className="app-nav-disclosure" data-expanded={expanded} aria-hidden={!expanded} inert={!expanded}><div>{children}</div></div>
  </div>
}
