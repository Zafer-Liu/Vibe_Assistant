import { useEffect, useMemo } from 'react'
import { useTranslation } from 'react-i18next'
import { Loader2, MessagesSquare, X } from 'lucide-react'
import type { MemoryConversationDetail } from '../types/memory'
import { intlLocale } from '../i18n'

export function sourceLabel(source: string) {
  return ({ codex: 'Codex', claude: 'Claude Code', qoder: 'Qoder', workbuddy: 'WorkBuddy', minimax: 'MiniMax Code', kimi: 'Kimi', copilot: 'GitHub Copilot', zcode: 'ZCode', dsh: 'DeepSeek Harness' } as Record<string, string>)[source] ?? source
}

export function displayTime(value: string) {
  const date = new Date(value.trim().replace(/(\.\d{3})\d+(?=(Z|[+-]\d{2}:\d{2})$)/, '$1'))
  return Number.isNaN(date.getTime())
    ? value
    : new Intl.DateTimeFormat(intlLocale(), { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', hour12: false }).format(date)
}

/** 秒级完整时间，供审计日志等需要精确时刻的列表使用。 */
export function displayFullTime(value: string) {
  const date = new Date(value.trim().replace(/(\.\d{3})\d+(?=(Z|[+-]\d{2}:\d{2})$)/, '$1'))
  return Number.isNaN(date.getTime())
    ? value
    : new Intl.DateTimeFormat(intlLocale(), { year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false }).format(date)
}

/** `labelKey` holds the i18n key rendered via `t()` at display time. */
export const STATE_META: Record<string, { labelKey: string; badge: string }> = {
  pending: { labelKey: 'memory.dialog.statePending', badge: 'bg-violet-500/10 text-violet-700 dark:text-violet-300' },
  retrying: { labelKey: 'memory.dialog.stateRetrying', badge: 'bg-amber-500/10 text-amber-700 dark:text-amber-300' },
  failed: { labelKey: 'memory.dialog.stateFailed', badge: 'bg-red-500/10 text-red-700 dark:text-red-300' },
  stored: { labelKey: 'memory.dialog.stateStored', badge: 'bg-emerald-500/10 text-emerald-700 dark:text-emerald-300' },
}

interface ConversationMessage {
  role: 'user' | 'assistant' | 'other'
  text: string
}

/** 账本正文格式：`\n\n` 分块，块首为 `[用户]\n` 或 `[助手]\n`。 */
function parseConversation(text: string): ConversationMessage[] {
  return text
    .split('\n\n')
    .map((block) => {
      if (block.startsWith('[用户]\n')) return { role: 'user' as const, text: block.slice(4).trim() }
      if (block.startsWith('[助手]\n')) return { role: 'assistant' as const, text: block.slice(4).trim() }
      return { role: 'other' as const, text: block.trim() }
    })
    .filter((message) => message.text.length > 0)
}

export function ConversationDialog({ detail, error, onClose }: {
  detail: MemoryConversationDetail | null
  error: string | null
  onClose: () => void
}) {
  const { t } = useTranslation()
  const messages = useMemo(() => (detail ? parseConversation(detail.conversation_text) : []), [detail])
  const meta = detail ? STATE_META[detail.l1_state] ?? STATE_META.pending : null

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onClose()
    }
    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [onClose])

  return <div className="fixed inset-0 z-50 flex items-center justify-center p-4 sm:p-8" role="dialog" aria-modal="true" aria-label={t('memory.dialog.title')}>
    <div className="absolute inset-0 bg-slate-900/45 backdrop-blur-[2px]" onClick={onClose} />
    <div className="relative flex max-h-full w-full max-w-3xl flex-col overflow-hidden rounded-2xl border border-slate-200 bg-white shadow-2xl dark:border-slate-700 dark:bg-slate-900">
      <header className="flex flex-wrap items-center gap-x-3 gap-y-1 border-b border-slate-100 px-5 py-4 dark:border-slate-800">
        <span className="grid h-9 w-9 shrink-0 place-items-center rounded-xl bg-violet-500/10 text-violet-600 dark:text-violet-300"><MessagesSquare size={18} /></span>
        {detail ? <>
          {meta && <span className={`inline-flex shrink-0 items-center rounded px-1.5 py-0.5 text-xs ${meta.badge}`}>{t(meta.labelKey)}</span>}
          <span className="shrink-0 text-sm font-semibold">{sourceLabel(detail.source)}</span>
          <span className="min-w-0 flex-1 truncate font-mono text-xs text-slate-500" title={detail.session_id}>{detail.session_id}</span>
          <span className="shrink-0 font-mono text-xs text-slate-500">{displayTime(detail.occurred_at)}</span>
          <span className="shrink-0 text-xs text-slate-500">{detail.message_count} {t('memory.dialog.messages')}</span>
        </> : <span className="text-sm text-slate-500 dark:text-slate-400">{t('memory.dialog.title')}</span>}
        <button type="button" onClick={onClose} aria-label={t('common.close')} className="ml-auto inline-flex h-8 w-8 shrink-0 items-center justify-center rounded-lg text-slate-500 transition hover:bg-slate-100 hover:text-slate-700 focus:outline-none focus-visible:ring-2 focus-visible:ring-violet-500 dark:hover:bg-slate-800 dark:hover:text-slate-200"><X size={17} /></button>
      </header>
      <div className="min-h-0 flex-1 overflow-y-auto px-5 py-4">
        {error ? (
          <p className="rounded-lg bg-red-500/10 px-3 py-2.5 text-sm text-red-700 dark:text-red-300">{error}</p>
        ) : !detail ? (
          <div className="flex min-h-40 items-center justify-center gap-2 text-sm text-slate-500"><Loader2 size={16} className="animate-spin motion-reduce:animate-none" />{t('memory.dialog.loading')}</div>
        ) : messages.length === 0 ? (
          <p className="py-10 text-center text-sm text-slate-500">{t('memory.dialog.empty')}</p>
        ) : (
          <ol className="space-y-3">
            {messages.map((message, index) => (
              <li key={index} className={`flex ${message.role === 'user' ? 'justify-end' : 'justify-start'}`}>
                <div className={`max-w-[85%] whitespace-pre-wrap break-words rounded-2xl px-3.5 py-2.5 text-sm leading-6 ${
                  message.role === 'user'
                    ? 'rounded-br-md bg-violet-600 text-white'
                    : message.role === 'assistant'
                      ? 'rounded-bl-md bg-slate-100 text-slate-800 dark:bg-slate-800 dark:text-slate-100'
                      : 'rounded-md bg-amber-500/10 text-xs text-amber-800 dark:text-amber-200'
                }`}>{message.text}</div>
              </li>
            ))}
          </ol>
        )}
      </div>
      {detail?.error && <footer className="border-t border-slate-100 px-5 py-3 text-xs text-red-600 dark:border-slate-800 dark:text-red-400">{t('memory.dialog.lastFailed')}{detail.error}</footer>}
    </div>
  </div>
}
