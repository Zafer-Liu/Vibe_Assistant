import { useTranslation } from 'react-i18next'
import { ArrowRight, MapPin } from 'lucide-react'
import { ONBOARDING_MODULES, type OnboardingModule } from './modules'

export function StepModules({ module, onNext }: { module: OnboardingModule; onNext: () => void }) {
  const { t } = useTranslation()
  const current = ONBOARDING_MODULES.find(item => item.key === module)!
  const Icon = current.icon
  const prefix = `onboarding.modules.${module}` as const
  return (
    <div>
      <div className="mb-4 inline-flex rounded-lg bg-blue-50 p-3 text-blue-600 dark:bg-blue-500/15 dark:text-blue-400"><Icon size={26} aria-hidden="true" /></div>
      <h2 className="text-xl font-semibold text-gray-900 dark:text-gray-100">{t(`${prefix}.title`)}</h2>
      <p className="mt-2 text-sm leading-6 text-gray-600 dark:text-gray-300">{t(`${prefix}.desc`)}</p>
      <dl className="my-5 divide-y divide-gray-200 border-y border-gray-200 dark:divide-gray-700 dark:border-gray-700">
        {(['one', 'two', 'three'] as const).map(feature => <div key={feature} className="py-3 sm:grid sm:grid-cols-[9rem_1fr] sm:gap-5">
          <dt className="text-sm font-semibold text-gray-800 dark:text-gray-100">{t(`${prefix}.${feature}.title`)}</dt>
          <dd className="mt-1 text-sm leading-6 text-gray-600 dark:text-gray-300 sm:mt-0">{t(`${prefix}.${feature}.desc`)}</dd>
        </div>)}
      </dl>
      <div className="flex gap-2 rounded-lg bg-blue-50 p-3 text-blue-800 dark:bg-blue-950/40 dark:text-blue-200">
        <MapPin size={17} className="mt-0.5 shrink-0" aria-hidden="true" />
        <div><h3 className="text-sm font-semibold">{t('onboarding.modules.firstAction')}</h3><p className="mt-1 text-sm leading-6">{t(`${prefix}.firstAction`)}</p></div>
      </div>
      <p className="mt-3 text-xs leading-5 text-gray-500 dark:text-gray-400">{t(`${prefix}.note`)}</p>
      <button type="button" onClick={onNext} className="mt-6 inline-flex w-full items-center justify-center gap-2 rounded-lg bg-blue-600 px-4 py-2.5 text-sm font-medium text-white hover:bg-blue-500">
        {t(`${prefix}.next`)}<ArrowRight size={16} aria-hidden="true" />
      </button>
    </div>
  )
}
