import { useEffect, useId, useMemo, useRef, useState, type ReactNode } from 'react'
import { useTranslation } from 'react-i18next'
import { ArrowRight, Search, X } from 'lucide-react'
export interface NavigationDestination { id: string; label: string; group: string; icon: ReactNode }
export function NavigationSearch({ destinations, onNavigate, open, onClose }: {
  destinations: NavigationDestination[]; onNavigate: (id: string) => void; open: boolean; onClose: () => void
}) {
  const { t } = useTranslation()
  const dialog = useRef<HTMLDialogElement>(null)
  const search = useRef<HTMLInputElement>(null)
  const [query, setQuery] = useState('')
  const [index, setIndex] = useState(0)
  const listId = useId()
  const filtered = useMemo(() => destinations.filter(d => `${d.label} ${d.group}`.toLocaleLowerCase().includes(query.trim().toLocaleLowerCase())), [destinations, query])
  const current = Math.min(index, Math.max(0, filtered.length - 1))
  useEffect(() => {
    if (!open) return
    const previous = document.activeElement
    const modal = dialog.current
    setQuery(''); setIndex(0)
    modal?.showModal(); search.current?.focus()
    return () => { modal?.close(); if (previous instanceof HTMLElement && previous.isConnected) previous.focus() }
  }, [open])
  useEffect(() => { document.getElementById(`${listId}-${current}`)?.scrollIntoView({ block: 'nearest' }) }, [current, listId])
  function choose(id: string) { onNavigate(id); onClose() }
  return <dialog ref={dialog} className="app-command-dialog" aria-label={t('shell.searchPages')} onCancel={onClose} onClick={e => { if (e.target === e.currentTarget) onClose() }}>
    <div className="app-command-search"><Search size={20} aria-hidden="true" /><input ref={search} role="combobox" aria-label={t('shell.searchPages')} aria-expanded="true" aria-controls={listId} aria-autocomplete="list" aria-activedescendant={filtered[current] ? `${listId}-${current}` : undefined} value={query} placeholder={t('shell.searchPlaceholder')} onChange={e => { setQuery(e.target.value); setIndex(0) }} onKeyDown={e => {
      if (e.key === 'ArrowDown') { e.preventDefault(); setIndex(i => Math.min(i + 1, filtered.length - 1)) }
      if (e.key === 'ArrowUp') { e.preventDefault(); setIndex(i => Math.max(0, i - 1)) }
      if (e.key === 'Enter' && filtered[current]) { e.preventDefault(); choose(filtered[current].id) }
    }} /><button type="button" className="app-icon-button" aria-label={t('common.close')} onClick={onClose}><X size={16} /></button></div>
    <div id={listId} role="listbox" className="app-command-list" aria-label={t('shell.pageResults')}>
      {filtered.length === 0 && <p role="status" className="p-5 text-sm text-gray-500 dark:text-gray-400">{t('shell.noPageResults')}</p>}
      {filtered.map((d, i) => <button key={d.id} id={`${listId}-${i}`} type="button" role="option" aria-selected={current === i} data-selected={current === i} tabIndex={-1} className="app-command-option" onMouseEnter={() => setIndex(i)} onClick={() => choose(d.id)}>{d.icon}<span>{d.label}</span><span className="app-command-group">{d.group}</span><ArrowRight size={14} aria-hidden="true" /></button>)}
    </div>
    <div className="app-command-footer"><span>{t('shell.searchKeyboard')}</span><span>{t('shell.searchScope')}</span></div>
  </dialog>
}
