import { memo, useEffect, useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import {
  BookOpenText, Check, ChevronRight, FileCode2, FolderSync, RefreshCw,
  Search, Sparkles, Copy, CheckCheck, Hash, Globe, Rocket,
  ShieldCheck, Layers, RotateCcw, ArrowRightLeft,
  Loader2, Trash2, X,
} from 'lucide-react'
import { useMemoryStore } from '../store/memoryStore'
import type { SkillDocument, SkillDriftFile, SkillItem, SkillSyncPreview } from '../types/memory'

type SourceFilter = 'all' | 'published' | 'codex' | 'claude' | 'qoder' | 'workbuddy' | 'minimax' | 'kimi' | 'copilot' | 'zcode'

export const SOURCES: { id: Exclude<SourceFilter, 'all'>; label: string; color: string }[] = [
  { id: 'codex',    label: 'Codex',       color: 'bg-emerald-500' },
  { id: 'claude',   label: 'Claude Code', color: 'bg-amber-500' },
  { id: 'qoder',    label: 'Qoder',       color: 'bg-sky-500' },
  { id: 'workbuddy', label: 'WorkBuddy',  color: 'bg-rose-500' },
  { id: 'minimax',  label: 'MiniMax Code', color: 'bg-indigo-500' },
  { id: 'kimi',     label: 'Kimi',        color: 'bg-cyan-500' },
  { id: 'copilot',  label: 'GitHub Copilot', color: 'bg-slate-500' },
  { id: 'zcode',    label: 'ZCode',       color: 'bg-fuchsia-500' },
]

const sourceColor = (source: string) => SOURCES.find(s => s.id === source)?.color ?? 'bg-gray-400'

// ── 技能性质（origin）：区分厂商内置、插件市场安装、本地自建与市场导入 ──

type SkillOrigin = 'builtin' | 'plugin' | 'local' | 'marketplace'
type OriginFilter = 'all' | SkillOrigin

/** 列表排序权重：本地/市场（用户自己的）在前，插件与内置在后。 */
const ORIGIN_ORDER: Record<SkillOrigin, number> = { local: 0, marketplace: 1, plugin: 2, builtin: 3 }

const originOf = (skill: SkillItem): SkillOrigin =>
  skill.origin === 'builtin' || skill.origin === 'plugin' || skill.origin === 'marketplace' ? skill.origin : 'local'

/** 非本地性质的行内徽标样式；本地是默认语境不加徽标，保持行紧凑。 */
const ORIGIN_BADGE: Record<Exclude<SkillOrigin, 'local'>, string> = {
  marketplace: 'bg-emerald-500/10 text-emerald-600 dark:bg-emerald-500/15 dark:text-emerald-300',
  plugin: 'bg-sky-500/10 text-sky-600 dark:bg-sky-500/15 dark:text-sky-300',
  builtin: 'bg-gray-200/70 text-gray-500 dark:bg-gray-700/60 dark:text-gray-400',
}

const ORIGIN_LABEL_KEY: Record<SkillOrigin, string> = {
  local: 'skills.originLocal',
  marketplace: 'skills.originMarketplace',
  plugin: 'skills.originPlugin',
  builtin: 'skills.originBuiltin',
}

export const SkillLibrary = memo(function SkillLibrary({ onOpenPublished, autoOpenSync, onSyncOpened }: {
  onOpenPublished?: () => void
  autoOpenSync?: boolean
  onSyncOpened?: () => void
}) {
  const { t } = useTranslation()
 const { skills, skillCacheReady, loadSkills, scanSkills, readSkill, applySkillSync, setSkillStatus, setSkillStatusBulk, setSkillAssignment, setSkillAssignmentBulk, publishSkill, rollbackSkillLatest, deleteSkill } = useMemoryStore()
  const [filter, setFilter] = useState<SourceFilter>('all')
  const [originFilter, setOriginFilter] = useState<OriginFilter>('all')
  const [query, setQuery] = useState('')
  const [selected, setSelected] = useState<SkillDocument | null>(null)
  const [loading, setLoading] = useState(false)
  const [notice, setNotice] = useState<string | null>(null)
  const [copied, setCopied] = useState(false)
  const [syncOpen, setSyncOpen] = useState(false)
  // 批量多选：key 为 `${source}:${name}`
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set())
  const [bulkTarget, setBulkTarget] = useState('workbuddy')
  // 发布对话框
  const [publishOpen, setPublishOpen] = useState(false)
  const [publishTargets, setPublishTargets] = useState<Set<string>>(new Set())
  // Shared catalog cache survives page navigation; only its first creation
  // deserves a blocking loading state.
  const [booting, setBooting] = useState(() => !skillCacheReady)

  useEffect(() => {
    let mounted = true
    void loadSkills().catch(() => setNotice(t('skills.readFailed'))).finally(() => {
      if (mounted) setBooting(false)
    })
    return () => { mounted = false }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [loadSkills])

  // Auto-dismiss transient notices so they never linger as stale chrome.
  useEffect(() => {
    if (!notice) return
    const timer = setTimeout(() => setNotice(null), 5000)
    return () => clearTimeout(timer)
  }, [notice])

  // 「已发布」页可带参跳入：进入后直接打开同步对话框。
  useEffect(() => {
    if (autoOpenSync) {
      setSyncOpen(true)
      onSyncOpened?.()
    }
  }, [autoOpenSync, onSyncOpened])

  // ── Derived data ──

 const visibleSkills = useMemo(() => skills.filter((skill) => {
   const filterMatches = filter === 'all'
     ? true
     : filter === 'published'
       ? skill.status === 'published'
       : skill.source === filter
   const originMatches = originFilter === 'all' || originOf(skill) === originFilter
   const q = query.trim().toLowerCase()
   return filterMatches && originMatches && (!q || `${skill.name} ${skill.description} ${skill.source}`.toLowerCase().includes(q))
 }).sort((a, b) => {
   // 分组排序：本地/市场在前，插件与内置在后；同组内按名称排序。
   const byOrigin = ORIGIN_ORDER[originOf(a)] - ORIGIN_ORDER[originOf(b)]
   return byOrigin !== 0 ? byOrigin : a.name.localeCompare(b.name)
 }), [skills, filter, originFilter, query])

 // 筛选胶囊的「分面计数」：只应用其他维度（性质 + 搜索），排除胶囊自身维度，
 // 这样切换性质下拉时「全部 / 已发布 / 各 Agent」的数字会同步收窄。
 const facetedSkills = useMemo(() => skills.filter((skill) => {
   const originMatches = originFilter === 'all' || originOf(skill) === originFilter
   const q = query.trim().toLowerCase()
   return originMatches && (!q || `${skill.name} ${skill.description} ${skill.source}`.toLowerCase().includes(q))
 }), [skills, originFilter, query])

  const countsBySource = useMemo(() => {
    const map: Record<string, number> = {}
    for (const s of facetedSkills) map[s.source] = (map[s.source] || 0) + 1
    return map
  }, [facetedSkills])

  // 当前筛选结果是否全部被勾选（用于「全选」复选框的受控状态）。
  const allVisibleChecked = visibleSkills.length > 0 && visibleSkills.every((s) => selectedIds.has(`${s.source}:${s.name}`))

  // 已发布数量，用于头部「已发布」入口徽标。
  const publishedCount = useMemo(() => skills.filter((s) => s.status === 'published').length, [skills])
  // 已发布胶囊的分面计数（随性质/搜索收窄）。
  const facetedPublishedCount = useMemo(() => facetedSkills.filter((s) => s.status === 'published').length, [facetedSkills])

  // ── Actions ──

  async function selectSkill(skill: SkillItem) {
    setLoading(true)
    try {
      setSelected(await readSkill(skill.source, skill.name))
    } catch (error) {
      setNotice(t('skills.readSkillFailed', { error: String(error) }))
    } finally {
      setLoading(false)
    }
  }

  async function importSkills() {
    setLoading(true)
    try {
      const imported = await scanSkills()
      setNotice(t('skills.scanDone', { count: imported.length }))
      if (selected && !imported.some((skill) => skill.source === selected.item.source && skill.name === selected.item.name)) setSelected(null)
    } catch (error) {
      setNotice(t('skills.scanFailed', { error: String(error) }))
    } finally {
      setLoading(false)
    }
  }

  // ── Bulk selection helpers ──

  const keyToRef = (key: string) => {
    const idx = key.indexOf(':')
    return { source: key.slice(0, idx), name: key.slice(idx + 1) }
  }

  function toggleSelect(key: string) {
    setSelectedIds((prev) => {
      const next = new Set(prev)
      if (next.has(key)) next.delete(key)
      else next.add(key)
      return next
    })
  }

  function toggleSelectAll() {
    const keys = visibleSkills.map((s) => `${s.source}:${s.name}`)
    setSelectedIds((prev) => {
      const allChecked = keys.length > 0 && keys.every((k) => prev.has(k))
      const next = new Set(prev)
      if (allChecked) keys.forEach((k) => next.delete(k))
      else keys.forEach((k) => next.add(k))
      return next
    })
  }

  async function refreshSelectedFrom(updated: SkillItem[]) {
    if (!selected) return
    const match = updated.find((u) => u.source === selected.item.source && u.name === selected.item.name)
    if (match) setSelected(await readSkill(match.source, match.name))
  }

  async function bulkSetStatus(status: 'draft' | 'published') {
    const items = [...selectedIds].map(keyToRef)
    setLoading(true)
    try {
      const updated = await setSkillStatusBulk(items, status)
      setNotice(status === 'published' ? t('skills.bulkPublished', { count: updated.length }) : t('skills.bulkDrafted', { count: updated.length }))
      setSelectedIds(new Set())
      await refreshSelectedFrom(updated)
    } catch (error) {
      setNotice(t('skills.bulkUpdateFailed', { error: String(error) }))
    } finally {
      setLoading(false)
    }
  }

  async function bulkAssign(targetAgent: string, equipped: boolean) {
    const items = [...selectedIds].map(keyToRef)
    setLoading(true)
    try {
      const updated = await setSkillAssignmentBulk(items, targetAgent, equipped)
      const label = SOURCES.find((s) => s.id === targetAgent)?.label ?? targetAgent
      setNotice(equipped ? t('skills.bulkEquipDone', { count: updated.length, label }) : t('skills.bulkUnequipDone', { count: updated.length, label }))
      setSelectedIds(new Set())
      await refreshSelectedFrom(updated)
    } catch (error) {
      setNotice(t('skills.bulkAssignFailed', { error: String(error) }))
    } finally {
      setLoading(false)
    }
  }

  function openPublishDialog() {
    if (!selected) return
    setPublishTargets(new Set(selected.item.assigned_agents))
    setPublishOpen(true)
  }

  async function confirmPublish() {
    if (!selected) return
    setLoading(true)
    try {
      await publishSkill(selected.item.source, selected.item.name, [...publishTargets])
      await reloadSelected(selected.item.source, selected.item.name)
      // 发布即推送：直接同步到所有已装备的 Agent（新增 + 覆盖更新），不再要求
      // 用户手动走「预览差异 → 确认同步」。
      const synced = await syncToAgents([...publishTargets].filter(a => a !== selected.item.source))
      setNotice(synced || t('skills.publishNoSync'))
      setPublishOpen(false)
    } catch (error) {
      setNotice(t('skills.publishFailed', { error: String(error) }))
    } finally {
      setLoading(false)
    }
  }

  /** 把共享库当前已发布内容直接同步到一组 Agent，返回汇总文案。 */
  async function syncToAgents(agents: string[]) {
    if (agents.length === 0) return ''
    const parts: string[] = []
    for (const agent of agents) {
      try {
        const result = await applySkillSync(agent, true)
        parts.push(t('skills.syncAgentSummary', { agent, created: result.create.length + result.missing.length, updated: result.update.length + result.conflict.length }))
      } catch (error) {
        parts.push(t('skills.syncAgentFailed', { agent, error: String(error) }))
      }
    }
    return t('skills.syncDoneSummary', { summary: parts.join('；') })
  }

  async function copyHash(hash: string) {
    await navigator.clipboard.writeText(hash)
    setCopied(true)
    setTimeout(() => setCopied(false), 2000)
  }

  async function reloadSelected(source: string, name: string) {
    setSelected(await readSkill(source, name))
  }

  async function updateSkillStatus(status: 'draft' | 'published') {
    if (!selected) return
    setLoading(true)
    try {
      await setSkillStatus(selected.item.source, selected.item.name, status)
      await reloadSelected(selected.item.source, selected.item.name)
      setNotice(status === 'published' ? t('skills.statusPublishedDone') : t('skills.statusDraftDone'))
    } catch (error) {
      setNotice(t('skills.statusUpdateFailed', { error: String(error) }))
    } finally {
      setLoading(false)
    }
  }

  async function rollbackLatest() {
    if (!selected) return
    setLoading(true)
    try {
      await rollbackSkillLatest(selected.item.source, selected.item.name)
      await reloadSelected(selected.item.source, selected.item.name)
      setNotice(t('skills.rollbackDone'))
    } catch (error) {
      setNotice(t('skills.rollbackFailed', { error: String(error) }))
    } finally {
    setLoading(false)
   }
 }

  // 删除当前查看的 Skill（含二次确认）。整个共享库目录与历史快照都会移除。
  async function deleteSelected() {
    if (!selected || !window.confirm(
      t('skills.deleteConfirm', { source: selected.item.source, name: selected.item.name })
    )) return
    const { source, name } = selected.item
    setLoading(true)
    try {
      await deleteSkill(source, name)
      setSelected(null)
      setNotice(t('skills.deleteDone', { name }))
    } catch (error) {
      setNotice(t('skills.deleteFailed', { error: String(error) }))
    } finally {
      setLoading(false)
    }
  }

  async function quickToggleAssign(skill: SkillItem, agentId: string) {
    const equipped = skill.assigned_agents.includes(agentId)
    try {
      const updated = await setSkillAssignment(skill.source, skill.name, agentId, !equipped)
      if (selected?.item.source === updated.source && selected.item.name === updated.name) {
        setSelected({ ...selected, item: updated })
      }
    } catch (error) {
      setNotice(t('skills.equipFailed', { error: String(error) }))
    }
  }

 // ── Render ──

  if (booting) {
    return <div className="flex h-full min-h-0 items-center justify-center bg-gray-50 px-6 dark:bg-gray-950" role="status" aria-live="polite">
      <div className="w-full max-w-md rounded-2xl border border-gray-200 bg-white p-6 shadow-sm dark:border-gray-800 dark:bg-gray-900">
        <div className="flex items-center gap-3 text-gray-800 dark:text-gray-100"><span className="rounded-xl bg-violet-500/10 p-2.5 text-violet-600 dark:text-violet-400"><Loader2 className="animate-spin motion-reduce:animate-none" size={21} /></span><div><p className="text-sm font-semibold">{t('skills.booting')}</p><p className="mt-0.5 text-xs text-gray-500 dark:text-gray-400">{t('skills.bootingHint')}</p></div></div>
        <div className="mt-5 grid grid-cols-2 gap-2.5" aria-hidden="true"><div className="h-16 animate-pulse rounded-lg bg-gray-100 dark:bg-gray-800" /><div className="h-16 animate-pulse rounded-lg bg-gray-100 dark:bg-gray-800" /><div className="col-span-2 h-10 animate-pulse rounded-lg bg-gray-100 dark:bg-gray-800" /></div>
      </div>
    </div>
  }

  return (
    <div className="flex h-full min-h-0 flex-col bg-gray-50 dark:bg-gray-950">
      {/* ── Header: title + primary actions only ── */}
      <header className="flex items-center gap-4 border-b border-gray-200 bg-white px-6 py-4 dark:border-gray-800 dark:bg-gray-900">
        <div className="flex min-w-0 items-center gap-3">
          <div className="rounded-xl bg-violet-500/10 p-2.5 text-violet-600 dark:text-violet-400">
            <BookOpenText size={22} />
          </div>
          <div className="min-w-0">
            <h1 className="text-base font-semibold text-gray-900 dark:text-gray-100">{t('skills.title')}</h1>
            <p className="mt-0.5 hidden text-xs text-gray-500 dark:text-gray-400 sm:block">{t('skills.subtitle')}</p>
          </div>
        </div>

        <div className="ml-auto flex items-center gap-2">
          {onOpenPublished && (
            <button
              onClick={onOpenPublished}
              className="inline-flex items-center gap-1.5 rounded-lg border border-gray-300 px-3 py-2 text-xs font-medium text-gray-700 transition hover:bg-gray-50 dark:border-gray-600 dark:text-gray-200 dark:hover:bg-gray-800"
            >
              <ShieldCheck size={14} />
              {t('skills.publishedTitle')}
              <span className="rounded-full bg-emerald-500/10 px-1.5 py-0.5 font-mono text-[10px] font-semibold text-emerald-700 dark:text-emerald-300">{publishedCount}</span>
            </button>
          )}
          <button
            onClick={() => setSyncOpen(true)}
            className="inline-flex items-center gap-1.5 rounded-lg border border-gray-300 px-3 py-2 text-xs font-medium text-gray-700 transition hover:bg-gray-50 dark:border-gray-600 dark:text-gray-200 dark:hover:bg-gray-800"
          >
            <Rocket size={14} />
            {t('skills.syncTitle')}
          </button>
          <button
            onClick={importSkills}
            disabled={loading}
            className="inline-flex items-center gap-2 rounded-lg bg-violet-600 px-4 py-2 text-xs font-medium text-white transition-all hover:bg-violet-500 active:scale-[0.97] disabled:opacity-50"
          >
            <RefreshCw size={14} className={loading ? 'animate-spin' : ''} />
            {t('skills.scan')}
          </button>
        </div>
      </header>

      {/* ── Toolbar: search + source filters in one dedicated row ── */}
      <div className="flex flex-wrap items-center gap-3 border-b border-gray-200 bg-white px-6 py-3 dark:border-gray-800 dark:bg-gray-900">
        <label className="relative block w-full sm:w-64">
          <Search size={14} className="pointer-events-none absolute left-2.5 top-2.5 text-gray-400" />
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={t('skills.search')}
            className="w-full rounded-lg border border-gray-300 bg-white py-1.5 pl-8 pr-3 text-xs text-gray-800 outline-none transition focus:border-violet-400 focus:ring-2 focus:ring-violet-400/20 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100 dark:focus:border-violet-500"
          />
        </label>
        <select
          value={originFilter}
          onChange={(e) => setOriginFilter(e.target.value as OriginFilter)}
          aria-label={t('skills.originFilterAria')}
          className="rounded-lg border border-gray-300 bg-white px-2 py-1.5 text-xs text-gray-700 outline-none transition focus:border-violet-400 focus:ring-2 focus:ring-violet-400/20 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-200 dark:focus:border-violet-500"
        >
          <option value="all">{t('skills.originAll')}</option>
          <option value="local">{t('skills.originLocal')}</option>
          <option value="marketplace">{t('skills.originMarketplace')}</option>
          <option value="plugin">{t('skills.originPlugin')}</option>
          <option value="builtin">{t('skills.originBuiltin')}</option>
        </select>
        <div className="flex flex-wrap gap-1.5">
         <FilterPill active={filter === 'all'} onClick={() => setFilter('all')}>
           {t('skills.all')} <span className="ml-1 font-mono opacity-70">{facetedSkills.length}</span>
         </FilterPill>
         <FilterPill active={filter === 'published'} onClick={() => setFilter('published')}>
           <ShieldCheck size={11} className="mr-0.5" />
           {t('skills.publishedFilter')} <span className="ml-1 font-mono opacity-70">{facetedPublishedCount}</span>
         </FilterPill>
         {SOURCES.map(s => (
            <FilterPill key={s.id} active={filter === s.id} onClick={() => setFilter(s.id)}>
              <span className={`mr-1 inline-block h-1.5 w-1.5 rounded-full ${s.color}`} />
              {s.label}
              <span className="ml-1 font-mono opacity-70">{countsBySource[s.id] ?? 0}</span>
            </FilterPill>
          ))}
        </div>
      </div>

      {/* ── Main: list + detail ── */}
      <div className="grid min-h-0 flex-1 grid-cols-1 md:grid-cols-[300px_minmax(0,1fr)]">
        {/* ── Left: skill list ── */}
        <aside className="flex min-h-0 flex-col border-r border-gray-200 bg-white dark:border-gray-800 dark:bg-gray-900 max-md:max-h-72">
          {/* 常态列表头：全选 + 总数，保持轻量 */}
          <div className="flex items-center justify-between border-b border-gray-200 px-3 py-2 dark:border-gray-800">
            <label className="flex cursor-pointer items-center gap-1.5 text-xs text-gray-600 dark:text-gray-300">
              <input type="checkbox" checked={allVisibleChecked} onChange={toggleSelectAll} className="h-3.5 w-3.5 accent-violet-600" />
              {t('skills.selectAll')}
            </label>
            <span className="text-xs text-gray-400 dark:text-gray-500">{t('skills.totalCount', { count: visibleSkills.length })}</span>
          </div>

          <div className="min-h-0 flex-1 overflow-y-auto p-2">
            {visibleSkills.map((skill) => {
              const key = `${skill.source}:${skill.name}`
              const isSelected = selected?.item.source === skill.source && selected.item.name === skill.name
              const isChecked = selectedIds.has(key)
              return (
               <div
                 key={key}
                  className={`group mb-0.5 w-full rounded-lg transition-all ${
                    isSelected
                      ? 'border-l-[3px] border-l-violet-500 bg-violet-50 dark:border-l-violet-400 dark:bg-violet-500/10'
                      : 'border-l-[3px] border-l-transparent hover:bg-gray-100 dark:hover:bg-gray-800'
                  }`}
                >
                <div className="flex items-center">
                  <input
                   type="checkbox"
                   checked={isChecked}
                   onChange={() => toggleSelect(key)}
                   onClick={(e) => e.stopPropagation()}
                   className="ml-2.5 h-3.5 w-3.5 shrink-0 cursor-pointer accent-violet-600"
                   aria-label={t('skills.selectSkillAria', { name: skill.name })}
                 />
                 <button
                   onClick={() => selectSkill(skill)}
                   className={`min-w-0 flex-1 px-2.5 py-2.5 text-left ${isSelected ? 'text-violet-900 dark:text-violet-100' : 'text-gray-800 dark:text-gray-100'}`}
                 >
                   <div className="flex items-center gap-2">
                     <span className={`inline-block h-2 w-2 shrink-0 rounded-full ${sourceColor(skill.source)}`} />
                     <FileCode2 size={14} className={isSelected ? 'text-violet-600 dark:text-violet-400' : 'text-gray-400'} />
                     <span className="min-w-0 flex-1 truncate text-sm font-medium">{skill.name}</span>
                     {originOf(skill) !== 'local' && (
                       <span className={`shrink-0 rounded px-1 py-px text-[10px] font-medium ${ORIGIN_BADGE[originOf(skill) as Exclude<SkillOrigin, 'local'>]}`}>
                         {t(ORIGIN_LABEL_KEY[originOf(skill)])}
                       </span>
                     )}
                     <ChevronRight size={14} className="shrink-0 text-gray-400 opacity-0 transition group-hover:opacity-100" />
                   </div>
                   <p className="mt-1 truncate pl-8 text-xs text-gray-500 dark:text-gray-400">
                     {skill.description || skill.source}
                   </p>
                 </button>
                 </div>
                 {filter === 'published' && (
                   <div className="flex flex-wrap gap-1 px-2.5 pb-2">
                     {SOURCES.map(agent => {
                       const equipped = skill.assigned_agents.includes(agent.id)
                       return (
                         <button
                           key={agent.id}
                           onClick={() => { void quickToggleAssign(skill, agent.id) }}
                           disabled={loading}
                           title={`${equipped ? t('skills.unassignTitle') : t('skills.assignTitle')} ${agent.label}`}
                           className={`inline-flex items-center gap-1 rounded-full border px-1.5 py-0.5 text-[10px] font-medium transition ${
                             equipped
                               ? 'border-violet-500/60 bg-violet-50 text-violet-700 dark:border-violet-500/40 dark:bg-violet-500/15 dark:text-violet-200'
                               : 'border-gray-200 text-gray-400 hover:border-gray-300 hover:text-gray-600 dark:border-gray-700 dark:hover:border-gray-600'
                           }`}
                         >
                           <span className={`h-1.5 w-1.5 rounded-full ${agent.color}`} />
                           {agent.label}
                         </button>
                       )
                     })}
                   </div>
                 )}
               </div>
              )
            })}
            {!visibleSkills.length && (
              <div className="px-4 py-12 text-center">
                <div className="mx-auto mb-3 flex h-14 w-14 items-center justify-center rounded-2xl bg-amber-100 dark:bg-amber-900/30">
                  <Sparkles size={24} className="text-amber-600 dark:text-amber-400" />
                </div>
                <p className="text-sm font-medium text-gray-700 dark:text-gray-200">{t('skills.emptyTitle')}</p>
                <p className="mt-1 text-xs leading-5 text-gray-500 dark:text-gray-400">{t('skills.emptyHint')}</p>
              </div>
            )}
          </div>

          {/* ── 批量操作栏：仅在勾选后出现，吸附在列表底部 ── */}
          {selectedIds.size > 0 && (
            <div className="space-y-2 border-t border-violet-200 bg-violet-50 px-3 py-2.5 dark:border-violet-500/20 dark:bg-violet-500/10">
              <div className="flex items-center gap-2">
                <span className="text-xs font-semibold text-violet-700 dark:text-violet-300">{t('skills.bulkSelected', { count: selectedIds.size })}</span>
                <button disabled={loading} onClick={() => { void bulkSetStatus('published') }} className="inline-flex items-center gap-1 rounded-md bg-violet-600 px-2 py-1 text-xs font-medium text-white transition hover:bg-violet-500 disabled:opacity-50"><Check size={12} />{t('skills.bulkPublish')}</button>
                <button disabled={loading} onClick={() => { void bulkSetStatus('draft') }} className="inline-flex items-center gap-1 rounded-md border border-gray-300 bg-white px-2 py-1 text-xs font-medium text-gray-700 transition hover:bg-gray-50 disabled:opacity-50 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-200 dark:hover:bg-gray-700">{t('skills.bulkToDraft')}</button>
                <button onClick={() => setSelectedIds(new Set())} className="ml-auto inline-flex items-center gap-1 rounded-md px-1.5 py-1 text-xs text-gray-400 transition hover:text-gray-600 dark:hover:text-gray-300" aria-label={t('skills.clearSelectionAria')}><X size={12} />{t('skills.clearSelection')}</button>
              </div>
              <div className="flex items-center gap-2">
                <ShieldCheck size={12} className="shrink-0 text-gray-400" />
                <select value={bulkTarget} onChange={(e) => setBulkTarget(e.target.value)} className="min-w-0 flex-1 rounded-md border border-gray-300 bg-white px-1.5 py-1 text-xs text-gray-700 outline-none focus:border-violet-400 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-200">
                  {SOURCES.map(s => <option key={s.id} value={s.id}>{s.label}</option>)}
                </select>
                <button disabled={loading} onClick={() => { void bulkAssign(bulkTarget, true) }} className="inline-flex items-center gap-1 rounded-md bg-violet-600 px-2 py-1 text-xs font-medium text-white transition hover:bg-violet-500 disabled:opacity-50">{t('skills.bulkEquip')}</button>
                <button disabled={loading} onClick={() => { void bulkAssign(bulkTarget, false) }} className="inline-flex items-center gap-1 rounded-md border border-gray-300 bg-white px-2 py-1 text-xs font-medium text-gray-700 transition hover:bg-gray-50 disabled:opacity-50 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-200 dark:hover:bg-gray-700">{t('skills.bulkUnequip')}</button>
              </div>
            </div>
          )}
        </aside>

        {/* ── Right: skill detail ── */}
        <section className="min-h-0 min-w-0 overflow-y-auto max-md:border-t max-md:border-gray-200 max-md:dark:border-gray-800">
          {selected ? (
            <div className="mx-auto max-w-4xl space-y-4 p-5">
              {/* metadata + actions, one card instead of two */}
              <div className="rounded-2xl border border-gray-200 bg-white p-5 dark:border-gray-700 dark:bg-gray-900">
                <div className="flex items-start gap-3">
                  <div className="rounded-lg bg-violet-500/10 p-2 text-violet-600 dark:text-violet-400">
                    <FileCode2 size={18} />
                  </div>
                  <div className="min-w-0 flex-1">
                    <h2 className="text-lg font-semibold text-gray-900 dark:text-gray-100">{selected.item.name}</h2>
                    <p className="mt-0.5 text-sm text-gray-500 dark:text-gray-400">
                      {selected.item.description || t('skills.noDescription')}
                    </p>
                    <div className="mt-3 flex flex-wrap items-center gap-2 text-xs">
                      <span className="inline-flex items-center gap-1 rounded-full bg-gray-100 px-2 py-0.5 font-medium text-gray-600 dark:bg-gray-800 dark:text-gray-300">
                        {t(ORIGIN_LABEL_KEY[originOf(selected.item)])}
                      </span>
                      <span className="inline-flex items-center gap-1 rounded-full bg-gray-100 px-2 py-0.5 font-medium text-gray-600 dark:bg-gray-800 dark:text-gray-300">
                        <Globe size={11} />
                        {selected.item.source}
                      </span>
                      <button
                        onClick={() => copyHash(selected.item.hash)}
                        className="inline-flex items-center gap-1 rounded-full bg-gray-100 px-2 py-0.5 font-mono text-gray-500 transition hover:bg-gray-200 dark:bg-gray-800 dark:text-gray-400 dark:hover:bg-gray-700"
                      >
                        <Hash size={11} />
                        {selected.item.hash.slice(0, 16)}…
                        {copied ? <CheckCheck size={12} className="text-emerald-500" /> : <Copy size={12} />}
                      </button>
                      <span className={`rounded-full px-2 py-0.5 font-medium ${selected.item.status === 'published' ? 'bg-emerald-500/10 text-emerald-700 dark:text-emerald-300' : 'bg-amber-500/10 text-amber-800 dark:text-amber-300'}`}>
                        {selected.item.status === 'published' ? t('skills.detailStatusPublished') : t('skills.detailStatusDraft')} · v{selected.item.version}
                      </span>
                    </div>
                  </div>
                </div>

                {/* 装备状态：只读展示，编辑统一走发布对话框 */}
                <div className="mt-4 flex flex-wrap items-center gap-2 rounded-xl bg-gray-50 px-3 py-2.5 dark:bg-gray-800/60">
                  <ShieldCheck size={13} className="shrink-0 text-violet-600 dark:text-violet-400" />
                  <span className="text-xs font-medium text-gray-600 dark:text-gray-400">{t('skills.equippedLabel')}</span>
                  {selected.item.assigned_agents.length > 0 ? (
                    selected.item.assigned_agents.map((id) => {
                      const agent = SOURCES.find((s) => s.id === id)
                      return (
                        <span key={id} className="inline-flex items-center gap-1.5 rounded-full border border-violet-500/60 bg-violet-50 px-2.5 py-0.5 text-xs font-medium text-violet-700 dark:bg-violet-500/15 dark:text-violet-200">
                          <span className={`h-1.5 w-1.5 rounded-full ${agent?.color ?? 'bg-gray-400'}`} />
                          {agent?.label ?? id}
                        </span>
                      )
                    })
                  ) : (
                    <span className="text-xs text-gray-400 dark:text-gray-500">{t('skills.notEquippedAny')}</span>
                  )}
                  <button onClick={openPublishDialog} className="ml-auto text-xs font-medium text-violet-600 transition hover:text-violet-500 dark:text-violet-400">
                    {t('skills.adjustArrow')}
                  </button>
                </div>

                <div className="mt-4 flex flex-wrap items-center gap-2 border-t border-gray-100 pt-4 dark:border-gray-800">
                  <button disabled={loading} onClick={openPublishDialog} className="inline-flex items-center gap-1.5 rounded-md bg-violet-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-violet-500 disabled:opacity-50"><Check size={13} />{selected.item.status === 'published' ? t('skills.adjustAssign') : t('skills.publishAssign')}</button>
                  <button disabled={loading || selected.item.status === 'draft'} onClick={() => { void updateSkillStatus('draft') }} className="inline-flex items-center gap-1.5 rounded-md border border-gray-300 px-2.5 py-1.5 text-xs font-medium text-gray-700 hover:bg-gray-50 disabled:opacity-50 dark:border-gray-600 dark:text-gray-200 dark:hover:bg-gray-800">{t('skills.toDraft')}</button>
                  <button disabled={loading} onClick={() => { void rollbackLatest() }} className="inline-flex items-center gap-1.5 rounded-md border border-gray-300 px-2.5 py-1.5 text-xs font-medium text-gray-500 hover:bg-gray-50 disabled:opacity-50 dark:border-gray-600 dark:text-gray-400 dark:hover:bg-gray-800"><RotateCcw size={13} />{t('skills.rollbackVersion')}</button>
                  <button disabled={loading} onClick={() => { void deleteSelected() }} className="inline-flex items-center gap-1.5 rounded-md border border-red-200 px-2.5 py-1.5 text-xs font-medium text-red-600 hover:bg-red-50 disabled:opacity-50 dark:border-red-900/60 dark:text-red-400 dark:hover:bg-red-900/20"><Trash2 size={13} />{t('skills.delete')}</button>
                </div>

                <p className="mt-3 text-xs leading-5 text-gray-400 dark:text-gray-500">
                  {t('skills.detailFlowHint')}
                </p>
              </div>

              {/* content card */}
              <div className="rounded-2xl border border-gray-200 bg-white dark:border-gray-700 dark:bg-gray-900">
                <div className="flex items-center gap-2 border-b border-gray-100 px-5 py-3 dark:border-gray-800">
                  <Layers size={14} className="text-gray-400" />
                  <span className="text-xs font-semibold uppercase tracking-wide text-gray-500">SKILL.md</span>
                </div>
                <pre className="overflow-x-auto whitespace-pre-wrap p-5 text-xs leading-6 text-gray-700 dark:text-gray-200 font-mono">
                  {selected.content}
                </pre>
              </div>

              {/* bundled files card */}
              {selected.item.files && selected.item.files.length > 0 && (
                <div className="rounded-2xl border border-gray-200 bg-white dark:border-gray-700 dark:bg-gray-900">
                  <div className="flex items-center gap-2 border-b border-gray-100 px-5 py-3 dark:border-gray-800">
                    <FolderSync size={14} className="text-gray-400" />
                    <span className="text-xs font-semibold uppercase tracking-wide text-gray-500">
                      {t('skills.bundledFiles', { count: selected.item.files.length })}
                    </span>
                    <span className="ml-auto text-[10px] text-gray-400 dark:text-gray-500">{t('skills.bundledFilesHint')}</span>
                  </div>
                  <div className="flex flex-wrap gap-1.5 p-5">
                    {selected.item.files.map((file) => (
                      <span key={file} className="inline-flex items-center rounded-md bg-gray-100 px-2 py-0.5 font-mono text-[11px] text-gray-600 dark:bg-gray-800 dark:text-gray-300">
                        {file}
                      </span>
                    ))}
                  </div>
                </div>
              )}
            </div>
          ) : (
            <div className="flex h-full flex-col items-center justify-center text-center">
              <div className="relative mb-4">
                <div className="absolute -inset-1 rounded-full bg-violet-100 opacity-50 blur dark:bg-violet-900/30" />
                <div className="relative flex h-16 w-16 items-center justify-center rounded-2xl bg-gray-100 dark:bg-gray-800">
                  <BookOpenText size={28} className="text-gray-400 dark:text-gray-500" />
                </div>
              </div>
              <h2 className="text-sm font-medium text-gray-700 dark:text-gray-200">{t('skills.selectTitle')}</h2>
              <p className="mt-1 max-w-sm text-xs leading-5 text-gray-500 dark:text-gray-400">{t('skills.selectHint')}</p>
            </div>
          )}
        </section>
      </div>

      {/* ── Deploy overview modal (all viewports, replaces the side panel) ── */}
      {syncOpen && (
        <DeployDialog
          onClose={() => setSyncOpen(false)}
          onNotice={(message) => setNotice(message)}
        />
      )}

      {/* ── Publish modal ── */}
      {publishOpen && selected && (
        <PublishDialog
          skill={selected}
          targets={publishTargets}
          setTargets={setPublishTargets}
          loading={loading}
          onConfirm={() => { void confirmPublish() }}
          onClose={() => setPublishOpen(false)}
        />
      )}

      {/* ── Floating toast for notices ── */}
      {notice && (
        <div className="pointer-events-none fixed bottom-5 left-1/2 z-50 -translate-x-1/2" role="status">
          <div className="pointer-events-auto flex max-w-md items-center gap-2 rounded-xl border border-gray-200 bg-white px-4 py-2.5 text-xs text-gray-700 shadow-lg dark:border-gray-700 dark:bg-gray-900 dark:text-gray-200">
            <span className="min-w-0 flex-1">{notice}</span>
            <button onClick={() => setNotice(null)} aria-label={t('skills.closeNoticeAria')} className="shrink-0 rounded-md p-1 text-gray-400 hover:bg-gray-100 hover:text-gray-600 dark:hover:bg-gray-800 dark:hover:text-gray-300">
              <X size={13} />
            </button>
          </div>
        </div>
      )}
    </div>
  )
})

// ── Sub-components ──

function FilterPill({ active, onClick, children }: { active: boolean; onClick: () => void; children: React.ReactNode }) {
  return (
    <button
      onClick={onClick}
      className={`inline-flex items-center rounded-full px-2.5 py-1 text-xs font-medium transition-all ${
        active
          ? 'bg-violet-600 text-white shadow-sm shadow-violet-500/25'
          : 'bg-gray-100 text-gray-600 hover:bg-gray-200 dark:bg-gray-800 dark:text-gray-300 dark:hover:bg-gray-700'
      }`}
    >
      {children}
    </button>
  )
}

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
          <button onClick={onClose} aria-label={t('skills.dialogCancel')} className="ml-auto rounded-md p-1.5 text-gray-400 transition hover:bg-gray-100 hover:text-gray-600 dark:hover:bg-gray-800 dark:hover:text-gray-300">
            <X size={15} />
          </button>
        </div>

        <div className="mt-3 rounded-lg border border-gray-200 p-3 dark:border-gray-700">
          <div className="flex items-center gap-2">
            <span className={`inline-block h-2 w-2 rounded-full ${sourceColor(skill.item.source)}`} />
            <span className="truncate text-sm font-medium text-gray-800 dark:text-gray-100">{skill.item.name}</span>
            <span className="ml-auto shrink-0 text-xs text-gray-400">{skill.item.source}</span>
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

        <p className="mt-3 text-xs leading-5 text-gray-400 dark:text-gray-500">{t('skills.publishDialogHint')}</p>

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

/** 部署体检对话框：一屏展示每个 Agent 收到的 Skill 副本状态，支持按 Agent 一键修复。
 *  发布/调整装备已改为即时同步，这里定位为手动核对与补救入口。 */
function DeployDialog({ onClose, onNotice }: {
  onClose: () => void
  onNotice: (message: string) => void
}) {
  const { t } = useTranslation()
  const { skills, previewSkillSync, applySkillSync } = useMemoryStore()
  const [previews, setPreviews] = useState<Record<string, SkillSyncPreview | 'error'>>({})
  const [busyAgent, setBusyAgent] = useState<string | null>(null)
  const [checking, setChecking] = useState(true)
  // 钻取查看某个「待更新」Skill 的本地更改 diff。
  const [drift, setDrift] = useState<{ target: string, skill: SkillItem } | null>(null)

  const refreshAgent = async (agent: string) => {
    try {
      const result = await previewSkillSync(agent)
      setPreviews((prev) => ({ ...prev, [agent]: result }))
    } catch {
      setPreviews((prev) => ({ ...prev, [agent]: 'error' }))
    }
  }

  useEffect(() => {
    let mounted = true
    setChecking(true)
    Promise.all(SOURCES.map(({ id }) => previewSkillSync(id).then(
      (result) => [id, result] as const,
      () => [id, 'error'] as const,
    ))).then((entries) => {
      if (mounted) {
        setPreviews(Object.fromEntries(entries))
        setChecking(false)
      }
    })
    return () => { mounted = false }
  }, [previewSkillSync, skills])

  async function redeploy(agent: string, label: string) {
    setBusyAgent(agent)
    try {
      const result = await applySkillSync(agent, true)
      const created = result.create.length + result.missing.length
      const updated = result.update.length + result.conflict.length
      const deployed = created + updated
      onNotice(t('skills.deployDone', { label, created, updated }))
      if (deployed > 0) await refreshAgent(agent)
    } catch (error) {
      onNotice(t('skills.deployFailed', { label, error: String(error) }))
    } finally {
      setBusyAgent(null)
    }
  }

  return (
    <div className="fixed inset-0 z-40 flex items-center justify-center p-4" role="dialog" aria-modal="true" aria-label={t('skills.deployDialogAria')}>
      <button aria-label={t('skills.dialogCancel')} onClick={onClose} className="absolute inset-0 cursor-default bg-gray-950/40 backdrop-blur-[2px]" />
      <div className="relative flex max-h-[85vh] w-full max-w-lg flex-col overflow-hidden rounded-2xl border border-gray-200 bg-white shadow-2xl dark:border-gray-700 dark:bg-gray-900">
        {/* header */}
        <div className="flex items-center gap-2 border-b border-gray-100 px-5 py-4 dark:border-gray-800">
          <div className="rounded-lg bg-violet-500/10 p-1.5 text-violet-600 dark:text-violet-400">
            <Rocket size={16} />
          </div>
          <div className="min-w-0">
            <h2 className="text-sm font-semibold text-gray-800 dark:text-gray-100">{t('skills.deployDialogTitle')}</h2>
            <p className="text-[11px] text-gray-400 dark:text-gray-500">{t('skills.deployDialogHint')}</p>
          </div>
          <button onClick={onClose} aria-label={t('skills.dialogCancel')} className="ml-auto shrink-0 rounded-md p-1.5 text-gray-400 transition hover:bg-gray-100 hover:text-gray-600 dark:hover:bg-gray-800 dark:hover:text-gray-300">
            <X size={15} />
          </button>
        </div>

        {/* agent cards */}
        <div className="min-h-0 flex-1 space-y-2.5 overflow-y-auto p-5">
          {checking && (
            <div className="flex items-center justify-center gap-2 py-8 text-xs text-gray-400">
              <Loader2 size={14} className="animate-spin motion-reduce:animate-none" />
              {t('skills.deployChecking')}
            </div>
          )}
          {!checking && SOURCES.map((agent) => {
            const preview = previews[agent.id]
            const equipped = skills.filter((s) => s.status === 'published' && s.assigned_agents.includes(agent.id) && s.source !== agent.id).length
            if (preview === undefined) return null
            if (preview === 'error') {
              return (
                <div key={agent.id} className="flex items-center gap-3 rounded-xl border border-gray-200 p-3 dark:border-gray-700">
                  <span className={`h-2 w-2 shrink-0 rounded-full ${agent.color}`} />
                  <span className="text-xs font-medium text-gray-700 dark:text-gray-200">{agent.label}</span>
                  <span className="ml-auto text-xs text-rose-500">{t('skills.deployReadFailed')}</span>
                </div>
              )
            }
            const pending = preview.create.length + preview.update.length + preview.conflict.length + preview.missing.length
            const clean = pending === 0
            return (
              <div key={agent.id} className="rounded-xl border border-gray-200 p-3 dark:border-gray-700">
                <div className="flex items-center gap-2">
                  <span className={`h-2 w-2 shrink-0 rounded-full ${agent.color}`} />
                  <span className="text-xs font-semibold text-gray-800 dark:text-gray-100">{agent.label}</span>
                  <span className="text-[11px] text-gray-400 dark:text-gray-500">{t('skills.deployEquipped', { count: equipped })}</span>
                  {clean && (
                    <span className="ml-auto inline-flex items-center gap-1 text-[11px] font-medium text-emerald-600 dark:text-emerald-400">
                      <ShieldCheck size={11} /> {t('skills.deployUpToDate')}
                    </span>
                  )}
                </div>
                <div className="mt-2 flex flex-wrap items-center gap-1.5">
                  {preview.create.length > 0 && <DeployBadge label={t('skills.badgeToCreate')} count={preview.create.length} tone="emerald" />}
                  {preview.update.length > 0 && <DeployBadge label={t('skills.badgeToUpdate')} count={preview.update.length} tone="amber" />}
                  {preview.conflict.length > 0 && <DeployBadge label={t('skills.badgeConflict')} count={preview.conflict.length} tone="rose" />}
                  {preview.missing.length > 0 && <DeployBadge label={t('skills.badgeMissing')} count={preview.missing.length} tone="rose" />}
                  {preview.unchanged.length > 0 && <DeployBadge label={t('skills.badgeUnchanged')} count={preview.unchanged.length} tone="gray" />}
                  <button
                    onClick={() => { void redeploy(agent.id, agent.label) }}
                    disabled={busyAgent === agent.id || clean}
                    className="ml-auto inline-flex items-center gap-1.5 rounded-lg bg-violet-600 px-3 py-1.5 text-xs font-medium text-white transition hover:bg-violet-500 active:scale-[0.98] disabled:opacity-40"
                  >
                    {busyAgent === agent.id ? <Loader2 size={12} className="animate-spin motion-reduce:animate-none" /> : <Rocket size={12} />}
                    {t('skills.deployAction')}
                  </button>
                </div>
                {preview.update.length > 0 && (
                  <div className="mt-1.5 space-y-0.5 border-t border-gray-100 pt-1.5 dark:border-gray-800">
                    {preview.update.map(skill => (
                      <button
                        key={`${skill.source}:${skill.name}`}
                        onClick={() => setDrift({ target: agent.id, skill })}
                        className="flex w-full items-center gap-1.5 rounded-md px-2 py-1 text-left text-xs text-gray-600 transition hover:bg-gray-50 dark:text-gray-300 dark:hover:bg-gray-800"
                      >
                        <FileCode2 size={11} className="shrink-0 text-amber-500" />
                        <span className="truncate">{skill.name}</span>
                        <span className="ml-auto shrink-0 text-[10px] font-medium text-violet-600 dark:text-violet-400">{t('skills.driftView')} →</span>
                      </button>
                    ))}
                  </div>
                )}
              </div>
            )
          })}
        </div>
      </div>
      {drift && (
        <DriftDialog
          target={drift.target}
          skill={drift.skill}
          onClose={() => setDrift(null)}
          onDone={() => { setDrift(null); void refreshAgent(drift.target) }}
          onNotice={onNotice}
        />
      )}
    </div>
  )
}

function DeployBadge({ label, count, tone }: { label: string; count: number; tone: 'emerald' | 'amber' | 'rose' | 'gray' }) {
  const tones: Record<string, string> = {
    emerald: 'bg-emerald-500/10 text-emerald-700 dark:text-emerald-400',
    amber:   'bg-amber-500/10 text-amber-700 dark:text-amber-400',
    rose:    'bg-rose-500/10 text-rose-700 dark:text-rose-400',
    gray:    'bg-gray-100 text-gray-500 dark:bg-gray-800 dark:text-gray-400',
  }
  return (
    <span className={`inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-[11px] font-medium ${tones[tone]}`}>
      {label} <span className="font-mono font-semibold tabular-nums">{count}</span>
    </span>
  )
}

type DiffLine = { kind: 'same' | 'del' | 'add', text: string }

/** 简单 LCS 行级 diff：del 行取旧文本（共享库），add 行取新文本（Agent 本地）。 */
function lineDiff(oldText: string, newText: string): DiffLine[] {
  const a = oldText.split('\n')
  const b = newText.split('\n')
  const n = a.length
  const m = b.length
  const dp: number[][] = Array.from({ length: n + 1 }, () => new Array<number>(m + 1).fill(0))
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      dp[i][j] = a[i] === b[j] ? dp[i + 1][j + 1] + 1 : Math.max(dp[i + 1][j], dp[i][j + 1])
    }
  }
  const out: DiffLine[] = []
  let i = 0
  let j = 0
  while (i < n && j < m) {
    if (a[i] === b[j]) { out.push({ kind: 'same', text: a[i] }); i++; j++ }
    else if (dp[i + 1][j] >= dp[i][j + 1]) { out.push({ kind: 'del', text: a[i] }); i++ }
    else { out.push({ kind: 'add', text: b[j] }); j++ }
  }
  while (i < n) { out.push({ kind: 'del', text: a[i] }); i++ }
  while (j < m) { out.push({ kind: 'add', text: b[j] }); j++ }
  return out
}

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
      <div className="flex items-center gap-3 text-[10px] text-gray-400 dark:text-gray-500">
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

/** 单个「待更新」Skill 的本地更改对比：文件级列表 + 行级 diff，
 *  操作为「用共享库覆盖」（丢弃本地改动）或「采纳本地版本」（回写共享库
 *  并同步到其他已装备 Agent）。 */
function DriftDialog({ target, skill, onClose, onDone, onNotice }: {
  target: string
  skill: SkillItem
  onClose: () => void
  onDone: () => void
  onNotice: (message: string) => void
}) {
  const { t } = useTranslation()
  const { skillDriftDetail, adoptLocalSkill, applySkillSyncOne } = useMemoryStore()
  const [files, setFiles] = useState<SkillDriftFile[] | null>(null)
  const [selected, setSelected] = useState(0)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  useEffect(() => {
    let mounted = true
    skillDriftDetail(target, skill.source, skill.name)
      .then((result) => { if (mounted) { setFiles(result); setSelected(0) } })
      .catch((e) => { if (mounted) setError(String(e)) })
    return () => { mounted = false }
  }, [target, skill.source, skill.name, skillDriftDetail])

  const agentLabel = SOURCES.find(s => s.id === target)?.label ?? target

  async function overwriteWithShared() {
    setBusy(true)
    try {
      await applySkillSyncOne(target, skill.source, skill.name)
      onDone()
    } catch (e) {
      setError(String(e))
      setBusy(false)
    }
  }

  async function adoptLocal() {
    if (!window.confirm(t('skills.driftAdoptConfirm', { name: skill.name }))) return
    setBusy(true)
    try {
      const result = await adoptLocalSkill(target, skill.source, skill.name)
      onNotice(t('skills.driftAdoptDone', {
        name: skill.name,
        synced: result.synced.length,
        skipped: result.skipped.length,
      }))
      onDone()
    } catch (e) {
      setError(String(e))
      setBusy(false)
    }
  }

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center p-4" role="dialog" aria-modal="true" aria-label={t('skills.driftTitle')}>
      <button aria-label={t('skills.dialogCancel')} onClick={onClose} className="absolute inset-0 cursor-default bg-gray-950/50 backdrop-blur-[2px]" />
      <div className="relative flex max-h-[85vh] w-full max-w-2xl flex-col overflow-hidden rounded-2xl border border-gray-200 bg-white shadow-2xl dark:border-gray-700 dark:bg-gray-900">
        <div className="flex items-center gap-2 border-b border-gray-100 px-5 py-4 dark:border-gray-800">
          <div className="rounded-lg bg-amber-500/10 p-1.5 text-amber-600 dark:text-amber-400">
            <FileCode2 size={16} />
          </div>
          <div className="min-w-0">
            <h2 className="truncate text-sm font-semibold text-gray-800 dark:text-gray-100">{skill.name}</h2>
            <p className="text-[11px] text-gray-400 dark:text-gray-500">{t('skills.driftSubtitle', { agent: agentLabel })}</p>
          </div>
          <button onClick={onClose} aria-label={t('skills.dialogCancel')} className="ml-auto shrink-0 rounded-md p-1.5 text-gray-400 transition hover:bg-gray-100 hover:text-gray-600 dark:hover:bg-gray-800 dark:hover:text-gray-300">
            <X size={15} />
          </button>
        </div>

        <div className="min-h-0 flex-1 overflow-y-auto p-5">
          {error && <p className="text-xs text-rose-500">{t('skills.driftLoadFailed', { error })}</p>}
          {!files && !error && (
            <div className="flex items-center justify-center gap-2 py-8 text-xs text-gray-400">
              <Loader2 size={14} className="animate-spin motion-reduce:animate-none" />
              {t('skills.deployChecking')}
            </div>
          )}
          {files && files.length === 0 && (
            <p className="py-8 text-center text-xs text-gray-400">{t('skills.driftEmpty')}</p>
          )}
          {files && files.length > 0 && (
            <SkillDiffPane files={files} selected={selected} onSelect={setSelected} />
          )}
        </div>

        <div className="flex flex-wrap items-center gap-2 border-t border-gray-100 px-5 py-3 dark:border-gray-800">
          <button
            onClick={() => { void overwriteWithShared() }}
            disabled={busy || !files}
            className="inline-flex items-center gap-1.5 rounded-lg border border-gray-300 px-3 py-1.5 text-xs font-medium text-gray-700 transition hover:bg-gray-50 disabled:opacity-40 dark:border-gray-600 dark:text-gray-200 dark:hover:bg-gray-800"
          >
            <RotateCcw size={13} />
            {t('skills.driftOverwrite')}
          </button>
          <button
            onClick={() => { void adoptLocal() }}
            disabled={busy || !files}
            className="inline-flex items-center gap-1.5 rounded-lg bg-violet-600 px-3 py-1.5 text-xs font-medium text-white transition hover:bg-violet-500 disabled:opacity-40"
          >
            {busy ? <Loader2 size={12} className="animate-spin motion-reduce:animate-none" /> : <ArrowRightLeft size={12} />}
            {t('skills.driftAdopt')}
          </button>
          <p className="ml-auto max-w-[45%] text-[10px] leading-4 text-gray-400 dark:text-gray-500">{t('skills.driftHint')}</p>
        </div>
      </div>
    </div>
  )
}
