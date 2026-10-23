import { useTranslation } from 'react-i18next'
import { ArrowRight, CheckCircle2 } from 'lucide-react'
import { ONBOARDING_MODULES, type OnboardingDestination } from './modules'

export function StepDone({ memoryReady, onFinish, onOpenModule }: { memoryReady: boolean; onFinish: () => void; onOpenModule?: (page: OnboardingDestination) => void }) {
  const { t } = useTranslation()
  return (
    <div className="flex flex-col items-center text-center">
      <div className="mb-6 flex h-16 w-16 items-center justify-center rounded-2xl bg-emerald-50 dark:bg-emerald-500/15">
        <CheckCircle2 className="h-8 w-8 text-emerald-600 dark:text-emerald-400" />
      </div>
      <h2 className="text-xl font-bold text-gray-900 dark:text-gray-100">
        {t('onboarding.done.title')}
      </h2>
      <p className="mt-3 text-sm leading-relaxed text-gray-500 dark:text-gray-400">
        {t(memoryReady ? 'onboarding.done.memoryReady' : 'onboarding.done.memorySkipped')}
      </p>
      <p className="mt-2 text-xs leading-5 text-gray-500 dark:text-gray-400">{t('onboarding.done.reopenHint')}</p>
      {onOpenModule && <div className="mt-6 w-full text-left">
        <h3 className="mb-2 text-sm font-semibold text-gray-800 dark:text-gray-100">{t('onboarding.done.exploreTitle')}</h3>
        <div className="divide-y divide-gray-200 border-y border-gray-200 dark:divide-gray-700 dark:border-gray-700">
          {ONBOARDING_MODULES.map(({ key, page, icon: Icon }) => <button key={key} type="button" onClick={() => onOpenModule(page)} className="flex w-full items-center gap-3 rounded-md px-2 py-3 text-left hover:bg-gray-50 dark:hover:bg-gray-800">
            <Icon size={20} className="shrink-0 text-blue-600 dark:text-blue-400" aria-hidden="true" />
            <span className="min-w-0 flex-1"><span className="block text-sm font-medium text-gray-800 dark:text-gray-100">{t(`onboarding.modules.${key}.title`)}</span><span className="mt-0.5 block text-xs leading-5 text-gray-500 dark:text-gray-400">{t(`onboarding.done.${key}Action`)}</span></span>
            <ArrowRight size={16} className="shrink-0 text-gray-500 dark:text-gray-400" aria-hidden="true" />
          </button>)}
        </div>
      </div>}
      <button
        onClick={onFinish}
        className="mt-8 w-full rounded-xl bg-blue-600 px-4 py-2.5 text-sm font-medium text-white transition-colors hover:bg-blue-500"
      >
        {t('onboarding.done.enter')}
      </button>
    </div>
  )
}
