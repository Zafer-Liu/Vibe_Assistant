import i18n from 'i18next'
import { initReactI18next } from 'react-i18next'
import zh from './locales/zh'
import en from './locales/en'

export const LANGUAGES = [
  { code: 'zh', label: '中文' },
  { code: 'en', label: 'English' },
] as const

export type LangCode = (typeof LANGUAGES)[number]['code']

const STORAGE_KEY = 'app.lang'

function detectInitialLang(): LangCode {
  const saved = localStorage.getItem(STORAGE_KEY)
  if (saved === 'zh' || saved === 'en') return saved
  // 跟随浏览器语言，默认中文
  const nav = navigator.language?.toLowerCase() ?? ''
  return nav.startsWith('en') ? 'en' : 'zh'
}

// 应用语言代码 -> BCP-47，用于 <html lang>（屏幕阅读器、拼写检查、字体选择依赖它）
const HTML_LANG: Record<LangCode, string> = { zh: 'zh-CN', en: 'en' }

function applyDocumentLang(lang: LangCode) {
  document.documentElement.lang = HTML_LANG[lang]
}

const initialLang = detectInitialLang()

i18n
  .use(initReactI18next)
  .init({
    resources: {
      zh: { translation: zh },
      en: { translation: en },
    },
    lng: initialLang,
    fallbackLng: 'zh',
    interpolation: { escapeValue: false },
    returnNull: false,
  })
applyDocumentLang(initialLang)

export function setLanguage(lang: LangCode) {
  i18n.changeLanguage(lang)
  applyDocumentLang(lang)
  localStorage.setItem(STORAGE_KEY, lang)
}

export function getLanguage(): LangCode {
  return (i18n.language as LangCode) || 'zh'
}

/** Intl locale matching the current app language, for Intl.DateTimeFormat etc. */
export function intlLocale(): string {
  return getLanguage() === 'en' ? 'en-US' : 'zh-CN'
}

export default i18n
