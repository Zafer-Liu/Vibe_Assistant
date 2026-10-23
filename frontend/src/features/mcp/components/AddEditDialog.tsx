import { McpModal } from './McpModal'
import { mcpApi } from '../../../api/mcp'

import { useId, useState } from 'react'
import { open } from '@tauri-apps/plugin-dialog'
import { useTranslation } from 'react-i18next'

import { Plug, Loader2, X, Check, FolderOpen } from 'lucide-react'
import { useMcpStore } from '../../../store/mcpStore'

import type { McpCatalogEntry } from '../../../types/mcp'

import { type EntryForm, EMPTY_FORM, InvalidMcpConfig, remoteUrlFromText, remoteUrlFromForm, formFromEntry, entryFromForm, smartParse, applyScan } from '../entryForm'

// ── 添加 / 编辑对话框 ─────────────────────────────────────────────────────────

export function AddEditDialog({ initial, editing, onClose, onSaved }: {
  initial: McpCatalogEntry | null
  editing: boolean
  onClose: () => void
  onSaved: (message: string) => void
}) {
  const { t } = useTranslation()
  const fieldId = useId()
  const upsertMcpEntry = useMcpStore((s) => s.upsertMcpEntry)
  const [tab, setTab] = useState<'manual' | 'paste' | 'scan'>('manual')
  const [form, setForm] = useState<EntryForm>(initial ? formFromEntry(initial) : EMPTY_FORM)
  const [pasteText, setPasteText] = useState('')
  const [scanDir, setScanDir] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const set = (patch: Partial<EntryForm>) => setForm((prev) => ({ ...prev, ...patch }))

  const isRemote = form.transport === 'sse' || form.transport === 'http'

  function changeTransport(transport: string) {
    setForm((prev) => ({
      ...prev,
      transport,
      // A URL already present in stdio proxy args should not need to be entered again.
      url: transport === 'stdio' || prev.url.trim()
        ? prev.url
        : remoteUrlFromForm(prev) || remoteUrlFromText(pasteText),
    }))
  }

  function applyPaste() {
    try {
      const patch = smartParse(pasteText)
      if (Object.keys(patch).length === 0) {
        setError(t('mcpLibrary.parseUnsupported'))
        return
      }
      setError('')
      set(patch)
      setTab('manual')
    } catch (e) {
      setError(t('mcpLibrary.invalidConfig', { error: e instanceof Error ? e.message : String(e) }))
    }
  }

  async function pickDir() {
    setBusy(true)
    setError('')
    try {
      const picked = await open({ directory: true, multiple: false })
      if (typeof picked !== 'string') return
      setScanDir(picked)
      const result = await mcpApi.scanMcpLocal({ dir: picked, provider: null })
      const next = applyScan(form, result)
      entryFromForm(next)
      setForm(next)
      setTab('manual')
    } catch (e) {
      setError(t('mcpLibrary.scanFailed', { error: String(e) }))
    } finally {
      setBusy(false)
    }
  }

  async function save() {
    if (busy || tab !== 'manual') return
    setBusy(true)
    setError('')
    try {
      const entry = entryFromForm(form, initial ?? undefined)
      await upsertMcpEntry(entry)
      onSaved(entry.name)
      onClose()
    } catch (e) {
      setError(e instanceof InvalidMcpConfig
        ? t('mcpLibrary.invalidConfig', { error: e.message })
        : t('mcpLibrary.saveFailed', { error: String(e) }))
    } finally {
      setBusy(false)
    }
  }

  const input = 'w-full rounded-lg border border-gray-300 bg-white px-3 py-2 text-sm text-gray-800 outline-none transition focus:border-violet-400 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100'
  const label = 'mb-1 block text-xs font-medium text-gray-500 dark:text-gray-400'

  return (
    <McpModal label={t(editing ? 'mcpLibrary.editTitle' : 'mcpLibrary.addTitle')} busy={busy} size="lg" onClose={onClose}>
      <div className="fluent-dialog-panel relative flex max-h-[85vh] w-full max-w-lg flex-col overflow-hidden rounded-2xl border border-gray-200 bg-white shadow-2xl dark:border-gray-700 dark:bg-gray-900">
        <div className="flex items-center gap-2 border-b border-gray-100 px-5 py-4 dark:border-gray-800">
          <div className="rounded-lg bg-violet-500/10 p-1.5 text-violet-600 dark:text-violet-400">
            <Plug size={16} />
          </div>
          <h2 className="text-sm font-semibold text-gray-900 dark:text-gray-100">
            {editing ? t('mcpLibrary.editTitle') : t('mcpLibrary.addTitle')}
          </h2>
          <button disabled={busy} onClick={onClose} className="ml-auto rounded-md p-1 text-gray-500 transition hover:text-gray-600 dark:hover:text-gray-300" aria-label={t('mcpLibrary.cancel')}>
            <X size={16} />
          </button>
        </div>

        {!editing && (
          <div className="flex gap-1 border-b border-gray-100 px-5 pt-3 dark:border-gray-800">
            {([['manual', t('mcpLibrary.tabManual')], ['paste', t('mcpLibrary.tabPaste')], ['scan', t('mcpLibrary.tabScan')]] as const).map(([id, title]) => (
              <button
                key={id}
                disabled={busy}
                aria-pressed={tab === id}
                onClick={() => setTab(id)}
                className={`rounded-t-lg px-3 py-1.5 text-xs font-medium transition ${tab === id ? 'border-b-2 border-violet-500 text-violet-700 dark:text-violet-300' : 'text-gray-500 hover:text-gray-700 dark:text-gray-400 dark:hover:text-gray-200'}`}
              >
                {title}
              </button>
            ))}
          </div>
        )}

        <div className="min-h-0 flex-1 space-y-3 overflow-y-auto px-5 py-4">
          {tab === 'paste' && !editing && (
            <div className="space-y-2">
              <textarea
                aria-label={t('mcpLibrary.tabPaste')}
                disabled={busy}
                value={pasteText}
                onChange={(e) => setPasteText(e.target.value)}
                placeholder={t('mcpLibrary.pastePlaceholder')}
                rows={6}
                className={`${input} font-mono text-xs`}
              />
              <p className="text-[11px] text-gray-500">{t('mcpLibrary.pasteHint')}</p>
              <button onClick={applyPaste} className="inline-flex items-center gap-1 rounded-md bg-violet-600 px-3 py-1.5 text-xs font-medium text-white transition hover:bg-violet-500">
                <Check size={12} />{t('mcpLibrary.tabPaste')}
              </button>
            </div>
          )}
          {tab === 'scan' && !editing && (
            <div className="space-y-2">
              <button onClick={() => { void pickDir() }} disabled={busy} className="inline-flex w-full items-center justify-center gap-2 rounded-lg border border-dashed border-gray-300 px-3 py-6 text-sm text-gray-500 transition hover:border-violet-400 hover:text-violet-600 disabled:opacity-50 dark:border-gray-600 dark:text-gray-400 dark:hover:border-violet-400">
                {busy ? <Loader2 size={16} className="animate-spin" /> : <FolderOpen size={16} />}
                {scanDir || t('mcpLibrary.scanPick')}
              </button>
              <p className="text-[11px] text-gray-500">{t('mcpLibrary.scanHint')}</p>
            </div>
          )}
          {tab === 'manual' && (
            <>
              <div>
                <label htmlFor={`${fieldId}-name`} className={label}>{t('mcpLibrary.fieldName')}</label>
                <input id={`${fieldId}-name`} value={form.name} onChange={(e) => set({ name: e.target.value })} placeholder="playwright" className={`${input} font-mono ${editing ? 'opacity-60' : ''}`} disabled={editing || busy} />
                <p className="mt-1 text-[11px] text-gray-500">{t('mcpLibrary.fieldNameHint')}</p>
              </div>
              <div>
                <label htmlFor={`${fieldId}-description`} className={label}>{t('mcpLibrary.fieldDescription')}</label>
                <input id={`${fieldId}-description`} disabled={busy} value={form.description} onChange={(e) => set({ description: e.target.value })} className={input} />
              </div>
              <div>
                <label htmlFor={`${fieldId}-transport`} className={label}>{t('mcpLibrary.fieldTransport')}</label>
                <select id={`${fieldId}-transport`} disabled={busy} value={form.transport} onChange={(e) => changeTransport(e.target.value)} className={input}>
                  <option value="stdio">{t('mcpLibrary.transportStdio')}</option>
                  <option value="sse">{t('mcpLibrary.transportSse')}</option>
                  <option value="http">{t('mcpLibrary.transportHttp')}</option>
                </select>
              </div>
              {isRemote ? (
                <>
                  <div>
                    <label htmlFor={`${fieldId}-url`} className={label}>{t('mcpLibrary.fieldUrl')}</label>
                    <input id={`${fieldId}-url`} disabled={busy} value={form.url} onChange={(e) => set({ url: e.target.value })} placeholder="https://example.com/mcp" className={`${input} font-mono text-xs`} />
                  </div>
                  <div>
                    <label htmlFor={`${fieldId}-headersText`} className={label}>{t('mcpLibrary.fieldHeadersJson')}</label>
                    <textarea id={`${fieldId}-headersText`} disabled={busy} value={form.headersText} onChange={(e) => set({ headersText: e.target.value })} rows={3} placeholder={'{"Authorization": "Bearer token"}'} className={`${input} font-mono text-xs`} />
                  </div>
                </>
              ) : (
                <>
                  <div>
                    <label htmlFor={`${fieldId}-command`} className={label}>{t('mcpLibrary.fieldCommand')}</label>
                    <input id={`${fieldId}-command`} disabled={busy} value={form.command} onChange={(e) => set({ command: e.target.value })} placeholder="npx" className={`${input} font-mono text-xs`} />
                  </div>
                  <div>
                    <label htmlFor={`${fieldId}-argsText`} className={label}>{t('mcpLibrary.fieldArgsJson')}</label>
                    <textarea id={`${fieldId}-argsText`} disabled={busy} value={form.argsText} onChange={(e) => set({ argsText: e.target.value })} rows={3} placeholder={'["-y", "@playwright/mcp"]'} className={`${input} font-mono text-xs`} />
                  </div>
                  <div>
                    <label htmlFor={`${fieldId}-envText`} className={label}>{t('mcpLibrary.fieldEnvJson')}</label>
                    <textarea id={`${fieldId}-envText`} disabled={busy} value={form.envText} onChange={(e) => set({ envText: e.target.value })} rows={3} placeholder={'{"API_KEY": "your_key"}'} className={`${input} font-mono text-xs`} />
                  </div>
                </>
              )}
            </>
          )}
          {error && <p role="alert" className="rounded-lg bg-red-50 px-3 py-2 text-xs text-red-600 dark:bg-red-900/30 dark:text-red-300">{error}</p>}
        </div>

        <div className="flex justify-end gap-2 border-t border-gray-100 px-5 py-3 dark:border-gray-800">
          <button disabled={busy} onClick={onClose} className="rounded-lg px-3 py-1.5 text-xs font-medium text-gray-500 transition hover:text-gray-700 dark:text-gray-400 dark:hover:text-gray-200">{t('mcpLibrary.cancel')}</button>
          <button onClick={() => { void save() }} disabled={busy || tab !== 'manual'} className="inline-flex items-center gap-1 rounded-lg bg-violet-600 px-3 py-1.5 text-xs font-medium text-white transition hover:bg-violet-500 disabled:opacity-50">
            {busy ? <Loader2 size={12} className="animate-spin" /> : <Check size={12} />}
            {t('mcpLibrary.save')}
          </button>
        </div>
      </div>
    </McpModal>
  )
}
