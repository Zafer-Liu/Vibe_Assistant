import { useId } from 'react'
import { useTranslation } from 'react-i18next'
import { Loader2 } from 'lucide-react'
import { useDesktopStore, type CloseBehavior } from './store'

export function WindowBehaviorSettings() {
  const { t, i18n } = useTranslation()
  const id = useId()
  const { preferences, working, error, save, refresh } = useDesktopStore()
  return <div className="space-y-3">
    {preferences ? <>
      <label htmlFor={id} className="block text-sm font-medium text-gray-800 dark:text-gray-100">{t('desktop.closeLabel')}</label>
      <div className="flex items-center gap-2">
        <select id={id} value={preferences.close_behavior} onChange={event => { void save(event.target.value as CloseBehavior) }} disabled={working} className="max-w-full rounded-md border border-gray-300 bg-white px-3 py-2 text-sm text-gray-800 disabled:opacity-50 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100">
          <option value="ask">{t('desktop.ask')}</option>
          <option value="background" disabled={!preferences.tray_available}>{t('desktop.background')}</option>
          <option value="exit">{t('desktop.quit')}</option>
        </select>
        {working && <Loader2 size={15} className="animate-spin text-blue-500" aria-label={t('common.loading')} />}
      </div>
      <p className="text-xs leading-5 text-gray-500 dark:text-gray-400">{t('desktop.settingHint')}</p>
      {!preferences.tray_available && <p className="text-sm text-amber-700 dark:text-amber-300">{t('desktop.trayUnavailable')}</p>}
    </> : !error && <p role="status" className="text-sm text-gray-500 dark:text-gray-400">{t('common.loading')}</p>}
    {error && <div role="alert" className="text-sm text-red-600 dark:text-red-400"><p className="break-words">{t('desktop.actionFailed', { error })}</p><button type="button" disabled={working} onClick={() => { void refresh(i18n.resolvedLanguage ?? i18n.language) }} className="mt-2 rounded-md border border-current px-2.5 py-1 text-xs disabled:opacity-50">{t('recovery.retry')}</button></div>}
  </div>
}
