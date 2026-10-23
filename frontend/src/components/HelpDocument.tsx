import { memo, useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { ArrowLeft, ArrowUp } from 'lucide-react'
import { DocumentMarkdown } from './DocumentMarkdown'

export interface HelpSection {
  id: string
  title: string
  body: string
}

interface HelpDocumentProps {
  /** 所属模块，显示在标题上方，例如「记忆中心」。 */
  domain: string
  /** 文档标题，例如「帮助说明」。 */
  title: string
  /** 一句话说明这份文档覆盖什么。 */
  summary: string
  sections: readonly HelpSection[]
  onBack?: () => void
  backLabel?: string
}

const anchorId = (id: string) => `help-${id}`

const prefersReducedMotion = () =>
  typeof window !== 'undefined' && window.matchMedia('(prefers-reduced-motion: reduce)').matches

/**
 * 帮助说明的统一文档版式：左侧目录 + 右侧正文。
 *
 * 记忆中心与 MCP 连接中心共用同一个组件，避免两处帮助各长一套样式。
 */
export const HelpDocument = memo(function HelpDocument({ domain, title, summary, sections, onBack, backLabel }: HelpDocumentProps) {
  const { t } = useTranslation()
  const scrollRef = useRef<HTMLDivElement | null>(null)
  const [activeId, setActiveId] = useState(sections[0]?.id ?? '')

  const sectionIds = useMemo(() => sections.map(section => section.id), [sections])

  // Scroll spy: 取最后一个已经滚到阈值之上的章节作为当前章节。
  useEffect(() => {
    const root = scrollRef.current
    if (!root || sectionIds.length === 0) return
    let frame = 0
    const sync = () => {
      frame = 0
      const rootTop = root.getBoundingClientRect().top
      let next = sectionIds[0]
      for (const id of sectionIds) {
        const element = root.querySelector<HTMLElement>(`#${anchorId(id)}`)
        if (element && element.getBoundingClientRect().top - rootTop <= 96) next = id
      }
      setActiveId(current => (current === next ? current : next))
    }
    const schedule = () => {
      if (!frame) frame = requestAnimationFrame(sync)
    }
    sync()
    root.addEventListener('scroll', schedule, { passive: true })
    window.addEventListener('resize', schedule)
    return () => {
      root.removeEventListener('scroll', schedule)
      window.removeEventListener('resize', schedule)
      if (frame) cancelAnimationFrame(frame)
    }
  }, [sectionIds])

  const scrollToSection = useCallback((id: string) => {
    const root = scrollRef.current
    const element = root?.querySelector<HTMLElement>(`#${anchorId(id)}`)
    if (!root || !element) return
    const top = element.getBoundingClientRect().top - root.getBoundingClientRect().top + root.scrollTop - 24
    root.scrollTo({ top: Math.max(top, 0), behavior: prefersReducedMotion() ? 'auto' : 'smooth' })
    setActiveId(id)
  }, [])

  const scrollToTop = useCallback(() => {
    scrollRef.current?.scrollTo({ top: 0, behavior: prefersReducedMotion() ? 'auto' : 'smooth' })
  }, [])

  return (
    <div ref={scrollRef} className="h-full overflow-y-auto bg-[var(--app-surface)]">
      <div className="mx-auto flex w-full max-w-[1100px] justify-center gap-12 px-6 py-10 lg:px-10">
        <aside className="hidden w-56 shrink-0 xl:block">
          <nav aria-label={t('help.onThisPage')} className="sticky top-10 max-h-[calc(100vh-6rem)] overflow-y-auto pb-6 pr-1">
            <p className="mb-3 text-[11px] font-semibold uppercase tracking-[.16em] text-gray-500 dark:text-gray-400">{t('help.onThisPage')}</p>
            <ol className="border-l border-gray-200 dark:border-gray-800">
              {sections.map((section, index) => {
                const active = section.id === activeId
                return (
                  <li key={section.id}>
                    <button
                      type="button"
                      onClick={() => scrollToSection(section.id)}
                      aria-current={active ? 'location' : undefined}
                      className={`-ml-px flex w-full items-baseline gap-2 border-l-2 py-1.5 pl-3 pr-2 text-left text-[13px] leading-5 transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-violet-500 ${active
                        ? 'border-violet-600 font-medium text-gray-900 dark:border-violet-400 dark:text-gray-100'
                        : 'border-transparent text-gray-500 hover:border-gray-300 hover:text-gray-900 dark:text-gray-400 dark:hover:border-gray-600 dark:hover:text-gray-100'}`}
                    >
                      <span className={`shrink-0 font-mono text-[10px] tabular-nums ${active ? 'text-violet-600 dark:text-violet-300' : 'text-gray-400 dark:text-gray-500'}`}>{String(index + 1).padStart(2, '0')}</span>
                      <span className="min-w-0">{section.title}</span>
                    </button>
                  </li>
                )
              })}
            </ol>
          </nav>
        </aside>

        <article className="min-w-0 w-full max-w-3xl pb-6" aria-label={title}>
          <header className="border-b border-gray-200 pb-8 dark:border-gray-800">
            {onBack && (
              <button type="button" onClick={onBack} className="mb-6 inline-flex items-center gap-1.5 rounded-md text-xs font-medium text-gray-500 transition-colors hover:text-gray-900 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-violet-500 dark:text-gray-400 dark:hover:text-gray-100">
                <ArrowLeft size={13} aria-hidden="true" />{backLabel}
              </button>
            )}
            <p className="text-[11px] font-semibold uppercase tracking-[.16em] text-violet-600 dark:text-violet-300">{domain}</p>
            <h1 className="mt-2 text-2xl font-semibold tracking-tight text-gray-900 dark:text-gray-100">{title}</h1>
            <p className="mt-3 text-sm leading-7 text-gray-600 dark:text-gray-300">{summary}</p>
          </header>

          {sections.map((section, index) => (
            <section key={section.id} id={anchorId(section.id)} className="scroll-mt-6 border-b border-gray-100 py-8 last:border-b-0 dark:border-gray-800/70">
              <h2 className="flex items-baseline gap-3 text-lg font-semibold tracking-tight text-gray-900 dark:text-gray-100">
                <span className="shrink-0 font-mono text-xs text-violet-600 dark:text-violet-300">{String(index + 1).padStart(2, '0')}</span>
                <span className="min-w-0">{section.title}</span>
              </h2>
              <DocumentMarkdown content={section.body} className="mt-3" />
            </section>
          ))}

          <footer className="pt-6">
            <button type="button" onClick={scrollToTop} className="inline-flex items-center gap-1.5 rounded-md text-xs font-medium text-gray-500 transition-colors hover:text-gray-900 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-violet-500 dark:text-gray-400 dark:hover:text-gray-100">
              <ArrowUp size={13} aria-hidden="true" />{t('help.backToTop')}
            </button>
          </footer>
        </article>
      </div>
    </div>
  )
})
