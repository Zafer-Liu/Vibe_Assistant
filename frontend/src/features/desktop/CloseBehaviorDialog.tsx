import { useEffect, useId, useRef } from 'react'
import { useTranslation } from 'react-i18next'
import { listen } from '@tauri-apps/api/event'
import { Loader2, PanelBottomClose } from 'lucide-react'
import { useDesktopStore, type DesktopPreferences } from './store'

export function CloseBehaviorDialog() {
  const { t, i18n } = useTranslation()
  const { preferences, pending, working, error, accept, refresh, resolve, cancel } = useDesktopStore()
  const dialog = useRef<HTMLDialogElement>(null)
  const panel = useRef<HTMLDivElement>(null)
  const title = useId(), description = useId()
  const language = i18n.resolvedLanguage ?? i18n.language
  useEffect(() => {
    let active = true
    let unlisten: (() => void) | undefined
    void (async () => {
      const dispose = await listen<DesktopPreferences>('desktop-close-requested', event => { if (active) accept(event.payload) })
      if (!active) { dispose(); return }
      unlisten = dispose
      await refresh(language)
    })().catch(error => { if (active) useDesktopStore.setState({ error: String(error) }) })
    return () => { active = false; unlisten?.() }
  }, [accept, refresh, language])
  useEffect(() => {
    if (!pending) return
    const previous = document.activeElement, modal = dialog.current
    modal?.showModal(); panel.current?.focus()
    return () => { modal?.close(); if (previous instanceof HTMLElement && previous.isConnected) previous.focus() }
  }, [pending])
  return <dialog ref={dialog} aria-labelledby={title} aria-describedby={description} aria-busy={working} onCancel={event => { event.preventDefault(); void cancel() }} className="fixed inset-0 m-auto w-[calc(100%-32px)] max-w-lg overflow-y-auto rounded-xl border border-gray-200 bg-white p-0 text-gray-900 shadow-xl backdrop:bg-black/40 backdrop:backdrop-blur-sm dark:border-gray-700 dark:bg-gray-900 dark:text-gray-100">
    <div ref={panel} tabIndex={-1} className="p-6 outline-none">
      <PanelBottomClose size={25} className="mb-4 text-blue-600 dark:text-blue-400" aria-hidden="true" />
      <h2 id={title} className="text-lg font-semibold">{t('desktop.closeTitle')}</h2>
      <p id={description} className="mt-2 text-sm leading-6 text-gray-600 dark:text-gray-300">{t('desktop.closeDescription')}</p>
      <p className="mt-3 text-xs leading-5 text-gray-500 dark:text-gray-400">{t('desktop.rememberHint')}</p>
      {!preferences?.tray_available && <p role="status" className="mt-3 text-sm text-amber-700 dark:text-amber-300">{t('desktop.trayUnavailable')}</p>}
      {error && <p role="alert" className="mt-3 break-words text-sm text-red-600 dark:text-red-400">{t('desktop.actionFailed', { error })}</p>}
      <div className="mt-6 flex flex-wrap items-center justify-end gap-2">
        <button type="button" onClick={() => { void cancel() }} disabled={working} className="mr-auto rounded-md px-3 py-2 text-sm text-gray-600 hover:bg-gray-100 disabled:opacity-50 dark:text-gray-300 dark:hover:bg-gray-800">{t('common.cancel')}</button>
        <button type="button" onClick={() => { void resolve('exit') }} disabled={working} className="rounded-md border border-gray-300 px-3 py-2 text-sm font-medium hover:bg-gray-50 disabled:opacity-50 dark:border-gray-600 dark:hover:bg-gray-800">{t('desktop.quit')}</button>
        <button type="button" onClick={() => { void resolve('background') }} disabled={working || !preferences?.tray_available} className="inline-flex items-center gap-2 rounded-md bg-blue-600 px-3 py-2 text-sm font-medium text-white hover:bg-blue-500 disabled:opacity-50">{working && <Loader2 size={14} className="animate-spin" />}{t('desktop.background')}</button>
      </div>
    </div>
  </dialog>
}
