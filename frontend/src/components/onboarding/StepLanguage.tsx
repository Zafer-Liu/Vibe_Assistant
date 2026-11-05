import { useTranslation } from 'react-i18next'
import { ArrowRight, Check, Languages } from 'lucide-react'
import { LANGUAGES, getLanguage, setLanguage, type LangCode } from '../../i18n'

// 选择之前界面语言还没定：标题和卡片说明始终用各自的母语书写，不随当前语言翻译。
const CAPTIONS: Record<LangCode, string> = { zh: '使用简体中文界面', en: 'Use the interface in English' }
const HTML_LANG: Record<LangCode, string> = { zh: 'zh-CN', en: 'en' }

/** 首次启动的第一步：先选语言，选中即刻生效（不必等到点「继续」），后面的步骤直接用新语言显示。 */
export function StepLanguage({ onNext }: { onNext: () => void }) {
  const { t } = useTranslation() // 订阅语言变化：选择后本页文字立刻重渲染
  const current = getLanguage()
  return (
    <div>
      <div className="mb-4 inline-flex rounded-lg bg-blue-50 p-3 text-blue-600 dark:bg-blue-500/15 dark:text-blue-400"><Languages size={26} aria-hidden="true" /></div>
      <h2 className="text-xl font-semibold text-gray-900 dark:text-gray-100"><span lang="zh-CN">选择语言</span> · <span lang="en">Choose your language</span></h2>
      <p className="mt-2 text-sm leading-6 text-gray-600 dark:text-gray-300">{t('onboarding.language.hint')}</p>
      <div role="radiogroup" aria-label="选择语言 / Choose your language" className="mt-5 grid gap-3 sm:grid-cols-2">
        {LANGUAGES.map(({ code, label }) => (
          <label key={code} className="flex cursor-pointer items-center gap-3 rounded-xl border border-gray-200 p-4 transition-colors hover:bg-gray-50 has-[:checked]:border-blue-500 has-[:checked]:bg-blue-50 has-[:focus-visible]:outline-2 has-[:focus-visible]:outline-offset-2 has-[:focus-visible]:outline-blue-500 dark:border-gray-700 dark:hover:bg-gray-800 dark:has-[:checked]:bg-blue-500/10">
            <input type="radio" name="onboarding-language" value={code} checked={current === code} onChange={() => setLanguage(code)} className="sr-only" />
            <span className="min-w-0 flex-1">
              <span lang={HTML_LANG[code]} className="block text-base font-semibold text-gray-900 dark:text-gray-100">{label}</span>
              <span lang={HTML_LANG[code]} className="mt-0.5 block text-xs text-gray-600 dark:text-gray-300">{CAPTIONS[code]}</span>
            </span>
            {current === code && <Check size={18} className="shrink-0 text-blue-600 dark:text-blue-400" aria-hidden="true" />}
          </label>
        ))}
      </div>
      <button type="button" onClick={onNext} className="mt-5 inline-flex w-full items-center justify-center gap-2 rounded-lg bg-blue-600 px-4 py-2.5 text-sm font-medium text-white hover:bg-blue-500">
        {t('onboarding.language.continue')}<ArrowRight size={16} aria-hidden="true" />
      </button>
    </div>
  )
}
