import { useTranslation } from 'react-i18next'
import { Brain, BookOpenText, Plug, BarChart3, Settings2, ArrowRight } from 'lucide-react'

const OVERVIEW = [
  { key: 'memory', icon: Brain },
  { key: 'skills', icon: BookOpenText },
  { key: 'mcp', icon: Plug },
  { key: 'utilities', icon: BarChart3 },
  { key: 'settings', icon: Settings2 },
] as const

export function StepWelcome({ onNext }: { onNext: () => void }) {
  const { t } = useTranslation()
  return (
    <div>
      <div className="mb-4 inline-flex rounded-lg bg-blue-50 p-3 text-blue-600 dark:bg-blue-500/15 dark:text-blue-400"><Brain size={26} aria-hidden="true" /></div>
      <h2 className="text-xl font-semibold text-gray-900 dark:text-gray-100">{t('onboarding.welcome.title')}</h2>
      <p className="mt-2 text-sm leading-6 text-gray-600 dark:text-gray-300">{t('onboarding.welcome.pitch')}</p>
      <p className="mt-3 text-sm leading-6 text-gray-600 dark:text-gray-300">{t('onboarding.welcome.guide')}</p>
      <h3 className="mt-5 text-sm font-semibold text-gray-800 dark:text-gray-100">{t('onboarding.welcome.overviewTitle')}</h3>
      <dl className="mt-2 divide-y divide-gray-200 dark:divide-gray-700">
        {OVERVIEW.map(({ key, icon: Icon }) => <div key={key} className="flex gap-3 py-3">
          <Icon size={18} className="mt-0.5 shrink-0 text-gray-500 dark:text-gray-400" aria-hidden="true" />
          <div><dt className="text-sm font-semibold text-gray-800 dark:text-gray-100">{t(`onboarding.welcome.overview.${key}.title`)}</dt><dd className="mt-0.5 text-sm leading-6 text-gray-600 dark:text-gray-300">{t(`onboarding.welcome.overview.${key}.desc`)}</dd></div>
        </div>)}
      </dl>
      <button type="button" onClick={onNext} className="mt-5 inline-flex w-full items-center justify-center gap-2 rounded-lg bg-blue-600 px-4 py-2.5 text-sm font-medium text-white hover:bg-blue-500">
        {t('onboarding.welcome.start')}<ArrowRight size={16} aria-hidden="true" />
      </button>
    </div>
  )
}
