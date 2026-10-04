import { useEffect, useId, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { invoke } from '@tauri-apps/api/core'
import { Loader2 } from 'lucide-react'
import { LlmSettings, type LlmProvider } from '../../pages/LlmSettings'

export function StepLlm({ onNext, onBusyChange }: {
  onNext: (memoryReady: boolean) => void
  onBusyChange: (busy: boolean) => void
}) {
  const { t } = useTranslation()
  const pendingChangesId = useId()
  const [checking, setChecking] = useState(false)
  const [configuring, setConfiguring] = useState(true)
  const [ready, setReady] = useState(false)
  const [error, setError] = useState('')
  const busy = checking || configuring

  useEffect(() => {
    onBusyChange(busy)
    return () => onBusyChange(false)
  }, [busy, onBusyChange])

  async function continueWithModel() {
    if (busy || !ready) return
    setChecking(true)
    setError('')
    try {
      // Read the saved selection only after all drafts and errors are resolved.
      const [config, providers] = await Promise.all([
        invoke<{ provider_id: string | null }>('memory_extraction_config_get'),
        invoke<LlmProvider[]>('list_llm_providers'),
      ])
      const provider = providers.find(item => item.id === config.provider_id)
      if (!provider?.enabled || !provider.api_key.trim()) {
        setError(t('onboarding.llm.chooseMemoryModel'))
        return
      }
      // The returned object retains every provider field required by the IPC.
      await invoke('test_llm_provider', { provider })
      onNext(true)
    } catch (cause) {
      setError(t('onboarding.llm.validationFailed', { error: String(cause) }))
    } finally {
      setChecking(false)
    }
  }

  return (
    <div>
      <h2 className="text-lg font-bold text-gray-900 dark:text-gray-100">{t('onboarding.llm.title')}</h2>
      <p className="mt-2 text-sm leading-6 text-gray-500 dark:text-gray-400">{t('onboarding.llm.desc')}</p>
      <fieldset disabled={checking} className="mt-5 min-w-0 border-0 p-0 disabled:opacity-70">
        <LlmSettings embedded onBusyChange={setConfiguring} onReadinessChange={setReady} />
      </fieldset>
      {!ready && <p id={pendingChangesId} role="status" className="mt-3 text-sm text-amber-700 dark:text-amber-400">{t('onboarding.llm.pendingChanges')}</p>}
      {error && <p role="alert" className="mt-3 break-words rounded-lg bg-red-50 p-3 text-sm text-red-600 dark:bg-red-500/10 dark:text-red-400">{error}</p>}
      <div className="mt-6 flex flex-wrap gap-2">
        <button type="button" onClick={() => onNext(false)} disabled={busy} className="flex-1 rounded-xl border border-gray-200 px-4 py-2.5 text-sm font-medium text-gray-600 hover:bg-gray-50 focus-visible:outline-2 focus-visible:outline-blue-500 disabled:opacity-50 dark:border-gray-700 dark:text-gray-300 dark:hover:bg-gray-800">{t('onboarding.llm.skip')}</button>
        <button type="button" onClick={() => { void continueWithModel() }} disabled={busy || !ready} aria-describedby={!ready ? pendingChangesId : undefined} className="inline-flex flex-1 items-center justify-center gap-2 rounded-xl bg-blue-600 px-4 py-2.5 text-sm font-medium text-white hover:bg-blue-500 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-blue-500 disabled:opacity-50">
          {checking && <Loader2 size={16} className="animate-spin" />}{t('onboarding.llm.verifyAndNext')}
        </button>
      </div>
    </div>
  )
}
