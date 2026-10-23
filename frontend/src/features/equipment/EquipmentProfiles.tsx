import { useEffect, useRef, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { useTranslation } from 'react-i18next'
import { ArrowLeft, Layers, RefreshCw } from 'lucide-react'
import type { McpCatalogEntry } from '../../types/mcp'
import type { SkillItem } from '../../types/skill'
interface Profile { name: string; servers: string[]; skills: { source: string; name: string }[] }
interface Plan { revision: string; items: { kind: string; source: string; name: string; action: string }[] }
interface Log { id: string; profile: string; target: string; at: string; items: { kind: string; name: string; state: string; detail: string }[] }
const TARGETS = [['claude_cli', 'Claude Code'], ['codex_cli', 'Codex'], ['qoder', 'Qoder'], ['workbuddy', 'WorkBuddy'], ['minimax', 'MiniMax'], ['kimi', 'Kimi'], ['zcode', 'ZCode'], ['dsh', 'DeepSeek Harness']]
export function EquipmentProfiles({ onBack }: { onBack: () => void }) {
  const { t } = useTranslation()
  const [profiles, setProfiles] = useState<Profile[]>([])
  const [servers, setServers] = useState<McpCatalogEntry[]>([])
  const [skills, setSkills] = useState<SkillItem[]>([])
  const [logs, setLogs] = useState<Log[]>([])
  const [selected, setSelected] = useState('')
  const [draft, setDraft] = useState<Profile>({ name: '', servers: [], skills: [] })
  const [target, setTarget] = useState('claude_cli')
  const [plan, setPlan] = useState<Plan | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [notice, setNotice] = useState('')
  const [loaded, setLoaded] = useState(false)
  const generation = useRef(0)
  async function load() {
    const values = await Promise.all([invoke<Profile[]>('equipment_profile_list'), invoke<McpCatalogEntry[]>('mcp_catalog_list'), invoke<SkillItem[]>('skill_list'), invoke<Log[]>('equipment_profile_logs')])
    setProfiles(values[0]); setServers(values[1]); setSkills(values[2].filter(s => s.status === 'published')); setLogs(values[3]); setLoaded(true)
  }
  useEffect(() => { const pending = generation; void load().catch(e => setError(String(e))); return () => { ++pending.current } }, [])
  function choose(name: string) { ++generation.current; setSelected(name); setDraft(profiles.find(p => p.name === name) ?? { name: '', servers: [], skills: [] }); setPlan(null); setError(''); setNotice('') }
  function change(value: Profile) { setDraft(value); setPlan(null); setNotice('') }
  async function action(kind: 'save' | 'preview' | 'apply' | 'delete') {
    const seq = ++generation.current
    setBusy(true); setError(''); setNotice('')
    try {
      if (kind === 'save') {
        const saved = await invoke<Profile>('equipment_profile_save', { profile: draft }); await load(); setDraft(saved); setSelected(saved.name); setPlan(null); setNotice(t('equipment.saved'))
      } else if (kind === 'delete') { await invoke('equipment_profile_delete', { name: selected }); await load(); choose('') }
      else if (kind === 'preview') { const value = await invoke<Plan>('equipment_profile_preview', { name: selected, target }); if (seq === generation.current) setPlan(value) }
      else if (plan) { await invoke<Log>('equipment_profile_apply', { name: selected, target, revision: plan.revision }); setPlan(null); await load(); setNotice(t('equipment.applied')) }
    } catch (e) { if (seq === generation.current) { setError(String(e)); if (kind !== 'save') setPlan(null) } }
    finally { setBusy(false) }
  }
  const input = 'w-full rounded-lg border border-gray-300 bg-white px-3 py-2 text-sm focus-visible:ring-2 focus-visible:ring-violet-500 dark:border-gray-700 dark:bg-gray-900'
  const button = 'inline-flex items-center justify-center gap-2 rounded-lg border border-gray-300 px-3 py-2 text-sm hover:bg-gray-50 focus-visible:ring-2 focus-visible:ring-violet-500 disabled:opacity-50 dark:border-gray-700 dark:hover:bg-gray-800'
  const dirty = JSON.stringify(draft) !== JSON.stringify(profiles.find(p => p.name === selected))
  return <section className="h-full overflow-y-auto px-5 py-6 text-gray-900 dark:text-gray-100">
    <header className="flex flex-wrap items-start gap-3"><button type="button" disabled={busy} className={button} onClick={onBack}><ArrowLeft size={16} />{t('equipment.back')}</button><div className="order-first w-full min-w-0 sm:order-none sm:w-auto sm:flex-1"><h1 className="text-xl font-semibold">{t('equipment.title')}</h1><p className="mt-2 max-w-3xl text-sm leading-relaxed text-gray-600 dark:text-gray-300">{t('equipment.hint')}</p></div><button type="button" disabled={busy} className={button} onClick={() => { setPlan(null); void load().catch(e => setError(String(e))) }}><RefreshCw size={15} />{t('common.refresh')}</button></header>
    {error && <p role="alert" className="mt-4 break-words text-sm text-red-700 dark:text-red-300">{error}</p>}
    {notice && <p role="status" className="mt-4 text-sm text-emerald-700 dark:text-emerald-300">{notice}</p>}
    {!loaded && <p role="status" className="mt-6 text-sm">{t('common.loading')}</p>}
    <fieldset disabled={busy || !loaded} className="mt-6 grid gap-7 lg:grid-cols-2">
      <div className="min-w-0 space-y-4">
        <label className="block text-sm font-medium">{t('equipment.select')}<select className={`${input} mt-2`} value={selected} onChange={e => choose(e.target.value)}><option value="">{t('equipment.new')}</option>{profiles.map(p => <option key={p.name}>{p.name}</option>)}</select></label>
        <label className="block text-sm font-medium">{t('equipment.name')}<input maxLength={64} className={`${input} mt-2`} value={draft.name} onChange={e => change({ ...draft, name: e.target.value })} /></label>
        <fieldset><legend className="text-sm font-medium">MCP</legend><div className="mt-2 max-h-52 overflow-y-auto divide-y divide-gray-200 dark:divide-gray-800">{servers.map(s => <label key={s.name} className="flex items-start gap-2 py-2 text-sm"><input type="checkbox" className="mt-1 accent-violet-600" checked={draft.servers.includes(s.name)} onChange={() => change({ ...draft, servers: draft.servers.includes(s.name) ? draft.servers.filter(v => v !== s.name) : [...draft.servers, s.name] })} /><span className="break-all">{s.name}</span></label>)}{draft.servers.filter(name => !servers.some(s => s.name === name)).map(name => <label key={name} className="flex gap-2 py-2 text-sm text-amber-700 dark:text-amber-300"><input type="checkbox" checked onChange={() => change({ ...draft, servers: draft.servers.filter(s => s !== name) })} />{name} · {t('equipment.missing')}</label>)}</div></fieldset>
        <fieldset><legend className="text-sm font-medium">{t('equipment.skills')}</legend><div className="mt-2 max-h-64 overflow-y-auto divide-y divide-gray-200 dark:divide-gray-800">{skills.map(s => <label key={`${s.source}:${s.name}`} className="flex items-start gap-2 py-2 text-sm"><input type="checkbox" className="mt-1 accent-violet-600" checked={draft.skills.some(v => v.source === s.source && v.name === s.name)} onChange={() => change({ ...draft, skills: draft.skills.some(v => v.source === s.source && v.name === s.name) ? draft.skills.filter(v => v.source !== s.source || v.name !== s.name) : [...draft.skills, { source: s.source, name: s.name }] })} /><span className="break-all">{s.name}<span className="ml-2 text-xs text-gray-600 dark:text-gray-300">{s.source}</span></span></label>)}{draft.skills.filter(s => !skills.some(v => v.source === s.source && v.name === s.name)).map(s => <label key={`${s.source}:${s.name}`} className="flex gap-2 py-2 text-sm text-amber-700 dark:text-amber-300"><input type="checkbox" checked onChange={() => change({ ...draft, skills: draft.skills.filter(v => v.source !== s.source || v.name !== s.name) })} />{s.name} · {t('equipment.missing')}</label>)}</div></fieldset>
        <div className="flex flex-wrap gap-2"><button type="button" className={button} disabled={!draft.name.trim() || !dirty} onClick={() => void action('save')}>{t('equipment.save')}</button>{selected && <button type="button" className={button} onClick={() => void action('delete')}>{t('equipment.delete')}</button>}</div>
      </div>
      <div className="min-w-0">
        <h2 className="font-semibold">{t('equipment.preview')}</h2><p className="mt-2 text-xs leading-relaxed text-gray-600 dark:text-gray-300">{t('equipment.applyHint')}</p>
        <label className="mt-4 block text-sm font-medium">{t('equipment.target')}<select className={`${input} mt-2`} value={target} onChange={e => { ++generation.current; setTarget(e.target.value); setPlan(null) }}>{TARGETS.map(([id, label]) => <option key={id} value={id}>{label}</option>)}</select></label>
        <div className="mt-3 flex flex-wrap gap-2"><button type="button" className={button} disabled={!selected || dirty} onClick={() => void action('preview')}>{t('equipment.preview')}</button><button type="button" className={`${button} bg-violet-600 text-white hover:bg-violet-500 dark:hover:bg-violet-500`} disabled={!plan || !plan.items.some(i => ['add', 'keep'].includes(i.action))} onClick={() => void action('apply')}>{t('equipment.apply')}</button></div>
        {dirty && <p className="mt-2 text-xs text-gray-600 dark:text-gray-300">{t('equipment.saveFirst')}</p>}
        {plan && <ul className="mt-4 divide-y divide-gray-200 dark:divide-gray-800">{plan.items.map(i => <li key={`${i.kind}:${i.source}:${i.name}`} className="flex flex-wrap justify-between gap-2 py-3 text-sm"><span className="break-all">{i.kind.toUpperCase()} · {i.name}</span><span className={['add', 'keep'].includes(i.action) ? 'text-emerald-700 dark:text-emerald-300' : 'text-amber-700 dark:text-amber-300'}>{t(`equipment.action_${i.action}`)}</span></li>)}</ul>}
      </div>
    </fieldset>
    <section className="mt-8 border-t border-gray-200 pt-5 dark:border-gray-800"><h2 className="font-semibold">{t('equipment.logs')}</h2><p className="mt-2 text-xs text-gray-600 dark:text-gray-300">{t('equipment.logHint')}</p>{logs.length === 0 && <p className="mt-3 text-sm text-gray-600 dark:text-gray-300">{t('equipment.noLogs')}</p>}{logs.map(log => <details key={log.id} className="mt-3 text-sm"><summary className="cursor-pointer rounded py-2 focus-visible:ring-2 focus-visible:ring-violet-500">{log.profile} · {log.target} · {new Date(log.at).toLocaleString()} · {log.items.filter(i => i.state === 'applied').length}/{log.items.length}</summary><ul className="ml-4 space-y-2 py-2">{log.items.map((i, index) => <li key={index} className="break-words">{i.kind.toUpperCase()} · {i.name} · {t(`equipment.result_${i.state}`)}{i.detail && <p className="mt-1 text-xs text-gray-600 dark:text-gray-300">{i.detail}</p>}</li>)}</ul></details>)}</section>
  </section>
}
export function EquipmentProfilesButton({ onClick }: { onClick: () => void }) { const { t } = useTranslation(); return <button type="button" onClick={onClick} className="inline-flex items-center gap-1.5 rounded-lg border border-gray-300 px-3 py-1.5 text-xs hover:bg-gray-50 focus-visible:ring-2 focus-visible:ring-violet-500 dark:border-gray-700 dark:hover:bg-gray-800"><Layers size={14} />{t('equipment.title')}</button> }
