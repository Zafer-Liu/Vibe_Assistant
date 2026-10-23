import { useState, useRef, useEffect } from 'react'
import { useTranslation } from 'react-i18next'
import { Languages, Check } from 'lucide-react'
import { LANGUAGES, setLanguage, getLanguage, type LangCode } from '../i18n'

export function LanguageSwitcher() {
  const { i18n } = useTranslation()
  const [open, setOpen] = useState(false)
  const ref = useRef<HTMLDivElement>(null)
  const current = getLanguage()

  useEffect(() => {
    function onClick(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false)
    }
    document.addEventListener('mousedown', onClick)
    return () => document.removeEventListener('mousedown', onClick)
  }, [])

  function pick(code: LangCode) {
    setLanguage(code)
    setOpen(false)
  }

  // i18n.language 变化时让组件重渲染（current 重新计算）
  void i18n.language

  return (
    <div ref={ref} className="relative">
      <button
        onClick={() => setOpen(o => !o)}
        className="app-icon-button"
        title={LANGUAGES.find(l => l.code === current)?.label}
      >
        <Languages className="h-4 w-4" />
      </button>
      {open && (
        <div className="absolute right-0 z-50 mt-1 min-w-[120px] overflow-hidden rounded-lg border border-gray-200 bg-white py-1 shadow-lg dark:border-gray-700 dark:bg-gray-800">
          {LANGUAGES.map(lang => (
            <button
              key={lang.code}
              onClick={() => pick(lang.code)}
              className={`flex w-full items-center justify-between gap-2 px-3 py-1.5 text-sm transition-colors ${
                current === lang.code
                  ? 'text-blue-600 dark:text-blue-400'
                  : 'text-gray-600 hover:bg-gray-100 dark:text-gray-300 dark:hover:bg-gray-700'
              }`}
            >
              {lang.label}
              {current === lang.code && <Check className="h-3.5 w-3.5" />}
            </button>
          ))}
        </div>
      )}
    </div>
  )
}
