import { useEffect, useRef, useState, type KeyboardEvent } from 'react'
import { useTranslation } from 'react-i18next'
import { StepLanguage } from './onboarding/StepLanguage'
import { StepWelcome } from './onboarding/StepWelcome'
import { StepLlm } from './onboarding/StepLlm'
import { StepDone } from './onboarding/StepDone'
import { StepModules } from './onboarding/StepModules'
import type { OnboardingDestination } from './onboarding/modules'
import { hasChosenLanguage } from '../i18n'

const STORAGE_KEY = 'onboarding-complete'
type Step = 'language' | 'welcome' | 'memory' | 'skills' | 'mcp' | 'llm' | 'done'
const STEPS: Step[] = ['welcome', 'memory', 'skills', 'mcp', 'llm', 'done']

export function needsOnboarding() {
  try { return !localStorage.getItem(STORAGE_KEY) } catch { return true }
}

/** First-run setup can also be reopened from Settings. */
export function Onboarding({ onFinish, onNavigate }: { onFinish: () => void; onNavigate?: (page: OnboardingDestination) => void }) {
  const { t } = useTranslation()
  // 语言是第一步：首次启动（还没明确选过语言）时先选语言，选中立即生效；
  // 已经选过的用户从设置里重新打开指引时跳过这一步。步骤列表只在挂载时确定，选择语言不会让它中途变化。
  const [steps] = useState<Step[]>(() => (hasChosenLanguage() ? STEPS : ['language', ...STEPS]))
  const [step, setStep] = useState<Step>(steps[0])
  const [busy, setBusy] = useState(false)
  const [memoryReady, setMemoryReady] = useState(false)
  const panel = useRef<HTMLDivElement>(null)
  const dialog = useRef<HTMLDialogElement>(null)

  useEffect(() => {
    const previous = document.activeElement
    const modal = dialog.current
    // A modal dialog makes the background inert, including when busy controls lose focus.
    modal?.showModal()
    panel.current?.focus()
    return () => {
      modal?.close()
      if (previous instanceof HTMLElement && previous.isConnected) previous.focus()
    }
  }, [])

  useEffect(() => {
    panel.current?.scrollTo({ top: 0 })
    panel.current?.focus()
  }, [step])

  useEffect(() => {
    const focused = document.activeElement
    if (busy && (!panel.current?.contains(focused) || focused?.matches(':disabled'))) {
      panel.current?.focus()
    }
  }, [busy])

  function finish(destination?: OnboardingDestination) {
    if (busy) return
    try { localStorage.setItem(STORAGE_KEY, '1') } catch { /* A storage-disabled webview may show setup again. */ }
    onFinish()
    if (destination) onNavigate?.(destination)
  }

  function trapFocus(event: KeyboardEvent<HTMLDivElement>) {
    if (event.key !== 'Tab') return
    const controls = Array.from(panel.current?.querySelectorAll<HTMLElement>(
      'button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), a[href], [tabindex="0"]',
    ) ?? []).filter(element => element.getClientRects().length > 0 && !element.matches(':disabled'))
    const first = controls[0]
    const last = controls[controls.length - 1]
    if (!first) { event.preventDefault(); return }
    if (event.shiftKey && (document.activeElement === first || document.activeElement === panel.current)) {
      event.preventDefault(); last.focus()
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault(); first.focus()
    }
  }

  const stepIndex = steps.indexOf(step)
  return (
    <dialog ref={dialog} onCancel={event => event.preventDefault()} aria-label={t('onboarding.guideTitle')}
      className="fixed inset-0 m-0 h-full max-h-none w-full max-w-none border-0 bg-transparent p-4 backdrop:bg-black/40 backdrop:backdrop-blur-sm open:flex open:items-center open:justify-center">
      <div ref={panel} tabIndex={-1} onKeyDown={trapFocus}
        className="max-h-[90vh] w-full max-w-3xl overflow-y-auto rounded-2xl bg-white p-5 shadow-2xl outline-none dark:bg-gray-900 sm:p-8">
        <div className="mb-6 flex items-center justify-between gap-4">
          <div className="min-w-0">
            <p role="status" aria-live="polite" className="text-xs font-medium text-gray-600 dark:text-gray-300">{t('onboarding.progress', { current: stepIndex + 1, total: steps.length, label: t(`onboarding.steps.${step}`) })}</p>
            <div className="mt-2 flex gap-1.5" aria-hidden="true">
              {steps.map((item, index) => <div key={item} className={`h-1 rounded-full ${index <= stepIndex ? 'w-5 bg-blue-500' : 'w-3 bg-gray-200 dark:bg-gray-700'}`} />)}
            </div>
          </div>
          <div className="flex items-center gap-3">
            {stepIndex > 0 && step !== 'done' && <button type="button" onClick={() => setStep(steps[stepIndex - 1])} disabled={busy} className="rounded text-xs text-gray-500 hover:text-gray-700 focus-visible:outline-2 focus-visible:outline-blue-500 disabled:opacity-50 dark:text-gray-400">{t('common.back')}</button>}
            {step !== 'done' && <button type="button" onClick={() => finish()} disabled={busy} className="rounded text-xs text-gray-500 hover:text-gray-700 focus-visible:outline-2 focus-visible:outline-blue-500 disabled:opacity-50 dark:text-gray-400">{t('onboarding.skip')}</button>}
          </div>
        </div>
        {step === 'language' && <StepLanguage onNext={() => setStep('welcome')} />}
        {step === 'welcome' && <StepWelcome onNext={() => setStep('memory')} />}
        {(step === 'memory' || step === 'skills' || step === 'mcp') && <StepModules module={step} onNext={() => setStep(steps[stepIndex + 1])} />}
        {step === 'llm' && <StepLlm onNext={ready => { setMemoryReady(ready); setStep('done') }} onBusyChange={setBusy} />}
        {step === 'done' && <StepDone memoryReady={memoryReady} onFinish={() => finish()} onOpenModule={onNavigate ? destination => finish(destination) : undefined} />}
      </div>
    </dialog>
  )
}
