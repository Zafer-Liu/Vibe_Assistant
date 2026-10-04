import { createContext, useContext } from 'react'
import { useTranslation } from 'react-i18next'
import { ArrowRight, RotateCcw, Settings2 } from 'lucide-react'

export type RecoveryTarget = 'ports' | 'settings' | 'mcp-library'

// Navigation stays owned by App; an error never changes configuration automatically.
export const RecoveryNavigationContext = createContext<((target: RecoveryTarget) => void) | null>(null)

function recoveryTarget(error: string): RecoveryTarget | undefined {
  if (/eaddrinuse|address already in use|os error 10048|端口.*占用/i.test(error)) return 'ports'
  if (/api[ _-]?key|unauthorized|\b401\b|\b403\b|llm|provider|模型|未配置.*密钥/i.test(error)) return 'settings'
  if (/\bmcp\b|model context protocol/i.test(error)) return 'mcp-library'
  return undefined
}

interface Props {
  error?: string
  /** Known context for errors that contain no diagnostic text. */
  fallback?: RecoveryTarget
  onRetry?: () => void
  onConfigure?: () => void
  disabled?: boolean
}

const hintKeys: Record<RecoveryTarget, string> = {
  ports: 'recovery.portHint',
  settings: 'recovery.modelHint',
  'mcp-library': 'recovery.mcpHint',
}
const actionKeys: Record<RecoveryTarget, string> = {
  ports: 'recovery.openPorts',
  settings: 'recovery.openSettings',
  'mcp-library': 'recovery.openMcp',
}

/** A next step alongside the original diagnostic, not a replacement for it. */
export function ErrorRecovery({ error = '', fallback, onRetry, onConfigure, disabled = false }: Props) {
  const { t } = useTranslation()
  const navigate = useContext(RecoveryNavigationContext)
  const target = fallback ?? recoveryTarget(error)
  if (!(target && navigate) && !onRetry && !onConfigure) return null

  const buttonClass = 'inline-flex items-center gap-1 rounded-md border border-current/20 px-2.5 py-1.5 text-xs font-medium transition-colors hover:bg-black/5 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 disabled:opacity-50 dark:hover:bg-white/5'
  return (
    <div className="mt-2 space-y-2 text-xs">
      <p className="leading-5">{t(target ? hintKeys[target] : 'recovery.genericHint')}</p>
      <div className="flex flex-wrap gap-2">
        {target && navigate && <button type="button" disabled={disabled} onClick={() => navigate(target)} className={buttonClass}>
          {t(actionKeys[target])}<ArrowRight size={13} aria-hidden="true" />
        </button>}
        {onConfigure && <button type="button" disabled={disabled} onClick={onConfigure} className={buttonClass}>
          <Settings2 size={13} aria-hidden="true" />{t('recovery.configureAgent')}
        </button>}
        {onRetry && <button type="button" disabled={disabled} onClick={onRetry} className={buttonClass}>
          <RotateCcw size={13} aria-hidden="true" />{t('recovery.retry')}
        </button>}
      </div>
    </div>
  )
}
