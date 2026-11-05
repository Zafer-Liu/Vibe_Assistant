import { useCallback, useEffect, useId, useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { invoke } from '@tauri-apps/api/core'
import { RefreshCw, Plus, Pencil, Trash2, AlertTriangle, Search, Variable } from 'lucide-react'
import { ErrorRecovery } from '../components/ErrorRecovery'
import { useConfirmDialog } from '../components/ConfirmDialog'
import { Modal } from '../components/Modal'

// ── 后端 env_manager.rs 的返回结构（字段为后端 snake_case 原样） ──

interface EnvVar {
  name: string
  /** 注册表原始存储值：REG_EXPAND_SZ 保留 %…% 引用原样。 */
  value: string
  /** 原始值展开后的效果预览；与 value 相同时为 null。 */
  expanded: string | null
  is_expand: boolean
}

type Scope = 'user' | 'system'

/** 分号分隔的路径列表变量：列表里逐条展示，编辑时按行编辑。 */
const PATH_LIST_NAMES = new Set(['path', 'psmodulepath', 'pathext', 'include', 'lib', 'classpath', 'pythonpath', 'gopath', 'java_home_ext'])

function isPathListVar(name: string, value: string): boolean {
  return value.includes(';') && PATH_LIST_NAMES.has(name.trim().toLowerCase())
}

/** 行列表 ⇄ 分号串：保存时空条目（空段在 Windows 里指当前目录）丢弃。 */
function joinLines(lines: string[]): string {
  return lines.map(line => line.trim()).filter(line => line !== '').join(';')
}

/** 新增 / 编辑共用的表单弹窗；name 为 null 表示新增。 */
function VarDialog({
  scope,
  initial,
  existing,
  onClose,
  onSaved,
}: {
  scope: Scope
  initial: EnvVar | null
  /** 当前作用域里已有的变量：新增时用来识别「同名覆盖」。 */
  existing: readonly EnvVar[]
  onClose: () => void
  onSaved: () => void
}) {
  const { t } = useTranslation()
  const titleId = useId()
  const { confirm, confirmDialog } = useConfirmDialog()
  // 作用域在句子里的叫法由代码给出，不用 ICU 的 select 语法：本项目的 i18next 没有 ICU 插件，
  // `{{scope, select, ...}}` 会原样漏出（曾让删除确认显示成「确定从user}变量中删除…」）。
  const scopeText = (scope === 'user' ? t('envVars.scopeUser') : t('envVars.scopeSystem')).toLowerCase()
  const [name, setName] = useState(initial?.name ?? '')
  const [raw, setRaw] = useState(initial?.value ?? '')
  const [lines, setLines] = useState<string[]>(() => (initial?.value ?? '').split(';'))
  // null = 跟随变量形态自动选择；用户手动切换后固定。
  const [modeOverride, setModeOverride] = useState<'lines' | 'raw' | null>(null)
  // 自动检测只翻向「分条」，编辑中删到只剩一条也不跳回单行（切换按钮仍可）。
  const [autoFlipped, setAutoFlipped] = useState(initial != null && isPathListVar(initial.name, initial.value))
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState('')
  const editing = initial != null

  const lineMode = modeOverride === 'raw' ? false : modeOverride === 'lines' || autoFlipped
  const value = lineMode ? joinLines(lines) : raw

  useEffect(() => {
    if (modeOverride === null && isPathListVar(name, raw)) {
      setLines(raw.split(';'))
      setAutoFlipped(true)
    }
  }, [name, raw, modeOverride])

  const switchMode = (target: 'lines' | 'raw') => {
    // 切换时把内容带到另一种形态，避免来回切换丢输入。
    const current = lineMode ? joinLines(lines) : raw
    if (target === 'lines') setLines(current.split(';'))
    else setRaw(current)
    setModeOverride(target)
  }

  const save = async () => {
    const trimmed = name.trim()
    // 同名覆盖、以及写入系统变量，保存后都没有撤销入口：先弹出确认，并列出新旧值供核对。
    const current = editing ? initial : existing.find(item => item.name.toLowerCase() === trimmed.toLowerCase()) ?? null
    const overwrite = !editing && current !== null
    if (overwrite || scope === 'system') {
      const confirmed = await confirm({
        title: overwrite ? t('envVars.overwriteTitle') : t('envVars.systemWriteTitle'),
        message: overwrite ? t('envVars.overwriteMessage', { name: trimmed, scopeLabel: scopeText }) : t('envVars.systemWriteMessage'),
        detailLines: [
          { label: t('envVars.name'), value: trimmed },
          ...(current ? [{ label: t('envVars.currentValue'), value: current.value }] : []),
          { label: t('envVars.newValue'), value },
        ],
        confirmLabel: overwrite ? t('envVars.overwriteConfirm') : t('envVars.systemWriteConfirm'),
        tone: overwrite ? 'danger' : 'default',
      })
      if (!confirmed) return
    }
    setSaving(true)
    setError('')
    try {
      await invoke('env_var_set', { scope, name: trimmed, value })
      onSaved()
    } catch (cause) {
      setError(String(cause))
      setSaving(false)
    }
  }

  // 点背景不关闭：编辑 PATH 这类长列表时一次误点会丢掉全部输入；Esc 与取消按钮仍可关闭。
  return (
    <>
    <Modal labelledBy={titleId} busy={saving} onClose={onClose}>
      <div className="w-[min(560px,100%)] rounded-2xl border border-gray-200 bg-white p-5 shadow-xl dark:border-gray-700 dark:bg-gray-900">
        <h3 id={titleId} className="text-sm font-semibold text-gray-900 dark:text-gray-100">
          {editing ? t('envVars.editTitle', { name: initial.name }) : t('envVars.add')}
        </h3>
        <div className="mt-4 space-y-3">
          <label className="block">
            <span className="text-xs font-medium text-gray-500 dark:text-gray-400">{t('envVars.name')}</span>
            <input
              value={name}
              onChange={e => { setName(e.target.value) }}
              disabled={editing}
              autoFocus={!editing}
              spellCheck={false}
              placeholder={t('envVars.namePlaceholder')}
              className="mt-1 w-full rounded-lg border border-gray-300 bg-white px-3 py-2 font-mono text-sm text-gray-900 placeholder:text-gray-400 focus:border-blue-500 focus:outline-none disabled:bg-gray-100 disabled:text-gray-500 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100 dark:disabled:bg-gray-800/50 dark:disabled:text-gray-500"
            />
          </label>
          <label className="block">
            <span className="flex items-center justify-between text-xs font-medium text-gray-500 dark:text-gray-400">
              <span>{t('envVars.value')}</span>
              <button
                type="button"
                onClick={() => { switchMode(lineMode ? 'raw' : 'lines') }}
                className="rounded px-1.5 py-0.5 text-[11px] font-normal text-blue-600 hover:bg-blue-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 dark:text-blue-400 dark:hover:bg-blue-900/30"
              >
                {lineMode ? t('envVars.rawMode') : t('envVars.lineMode')}
              </button>
            </span>
            {lineMode ? (
              <textarea
                value={lines.join('\n')}
                onChange={e => { setLines(e.target.value.split('\n')) }}
                spellCheck={false}
                rows={Math.min(14, Math.max(4, lines.length + 1))}
                placeholder={t('envVars.valuePlaceholder')}
                className="mt-1 w-full resize-y rounded-lg border border-gray-300 bg-white px-3 py-2 font-mono text-sm text-gray-900 placeholder:text-gray-400 focus:border-blue-500 focus:outline-none dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100"
              />
            ) : (
              <textarea
                value={raw}
                onChange={e => { setRaw(e.target.value) }}
                spellCheck={false}
                rows={4}
                placeholder={t('envVars.valuePlaceholder')}
                className="mt-1 w-full resize-y rounded-lg border border-gray-300 bg-white px-3 py-2 font-mono text-sm text-gray-900 placeholder:text-gray-400 focus:border-blue-500 focus:outline-none dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100"
              />
            )}
            {lineMode && (
              <p className="mt-1 text-xs text-gray-500 dark:text-gray-400">{t('envVars.lineModeHint')}</p>
            )}
            {!lineMode && value.includes('%') && (
              <p className="mt-1 text-xs text-gray-500 dark:text-gray-400">{t('envVars.expandHint')}</p>
            )}
          </label>
          {error && (
            <div role="alert" className="rounded-lg border border-red-200 bg-red-50 px-3 py-2 text-xs text-red-700 dark:border-red-900/60 dark:bg-red-950/30 dark:text-red-300">
              <p className="break-words">{error}</p>
              <ErrorRecovery error={error} onRetry={() => { void save() }} disabled={saving} />
            </div>
          )}
        </div>
        <div className="mt-5 flex justify-end gap-2">
          <button
            type="button"
            onClick={onClose}
            className="rounded-lg border border-gray-200 px-3 py-1.5 text-sm text-gray-600 hover:bg-gray-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 dark:border-gray-700 dark:text-gray-400 dark:hover:bg-gray-800"
          >
            {t('common.cancel')}
          </button>
          <button
            type="button"
            onClick={() => { void save() }}
            disabled={saving || name.trim().length === 0}
            className="rounded-lg bg-blue-600 px-3 py-1.5 text-sm font-medium text-white hover:bg-blue-700 disabled:opacity-40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 dark:bg-blue-500 dark:hover:bg-blue-600"
          >
            {saving ? t('common.saving') : t('common.save')}
          </button>
        </div>
      </div>
    </Modal>
    {confirmDialog}
    </>
  )
}

/**
 * 删除确认（二次确认）：先列出将被删除的变量名、作用域和当前值，再要求手动输入变量名才能点「删除」，
 * 避免误点或手滑；系统变量额外给出影响范围提示。受保护变量（PATH 等）仍由后端直接拒绝，错误在弹窗内显示。
 */
function DeleteDialog({
  scope,
  item,
  onClose,
  onSaved,
}: {
  scope: Scope
  item: EnvVar
  onClose: () => void
  onSaved: () => void
}) {
  const { t } = useTranslation()
  const titleId = useId()
  const scopeText = (scope === 'user' ? t('envVars.scopeUser') : t('envVars.scopeSystem')).toLowerCase()
  const [typed, setTyped] = useState('')
  const [deleting, setDeleting] = useState(false)
  const [error, setError] = useState('')
  const matched = typed.trim() === item.name
  // 路径列表逐条分行展示，和列表页保持一致。
  const currentValue = isPathListVar(item.name, item.value)
    ? item.value.split(';').filter(entry => entry !== '').join('\n')
    : item.value

  const remove = async () => {
    if (!matched || deleting) return
    setDeleting(true)
    setError('')
    try {
      await invoke('env_var_delete', { scope, name: item.name })
      onSaved()
    } catch (cause) {
      setError(String(cause))
      setDeleting(false)
    }
  }

  return (
    <Modal labelledBy={titleId} busy={deleting} onClose={onClose}>
      <form
        onSubmit={event => { event.preventDefault(); void remove() }}
        className="w-[min(480px,100%)] rounded-2xl border border-gray-200 bg-white p-5 shadow-xl dark:border-gray-700 dark:bg-gray-900"
      >
        <div className="flex items-start gap-2.5">
          <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-red-500" />
          <div className="min-w-0">
            <h3 id={titleId} className="text-sm font-semibold text-gray-900 dark:text-gray-100">{t('envVars.deleteTitle')}</h3>
            <p className="mt-1 break-all text-xs text-gray-500 dark:text-gray-400">
              {t('envVars.deleteConfirm', { name: item.name, scopeLabel: scopeText })}
            </p>
          </div>
        </div>
        <dl className="mt-3 grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1 rounded-lg bg-gray-50 px-3 py-2 text-xs dark:bg-gray-800/60">
          <dt className="text-gray-500 dark:text-gray-400">{t('envVars.name')}</dt>
          <dd className="break-all font-mono font-medium text-gray-900 dark:text-gray-100">
            {item.name}
            <span className={`ml-2 rounded px-1.5 py-0.5 font-sans text-[11px] font-medium ${scope === 'system' ? 'bg-red-50 text-red-700 dark:bg-red-900/30 dark:text-red-300' : 'bg-gray-200 text-gray-700 dark:bg-gray-700 dark:text-gray-200'}`}>
              {scope === 'user' ? t('envVars.scopeUser') : t('envVars.scopeSystem')}
            </span>
          </dd>
          <dt className="text-gray-500 dark:text-gray-400">{t('envVars.currentValue')}</dt>
          <dd className="max-h-28 overflow-y-auto whitespace-pre-wrap break-all font-mono text-gray-700 dark:text-gray-200">{currentValue}</dd>
        </dl>
        {scope === 'system' && (
          <p role="note" className="mt-3 rounded-lg border border-red-200 bg-red-50 px-3 py-2 text-xs leading-5 text-red-700 dark:border-red-900/60 dark:bg-red-950/30 dark:text-red-300">
            {t('envVars.deleteSystemWarn')}
          </p>
        )}
        <label className="mt-4 block">
          <span className="text-xs font-medium text-gray-700 dark:text-gray-300">{t('envVars.deleteTypeLabel', { name: item.name })}</span>
          <input
            value={typed}
            onChange={event => { setTyped(event.target.value) }}
            data-autofocus
            autoComplete="off"
            spellCheck={false}
            disabled={deleting}
            placeholder={t('envVars.deleteTypePlaceholder')}
            className="mt-1 w-full rounded-lg border border-gray-300 bg-white px-3 py-2 font-mono text-sm text-gray-900 placeholder:text-gray-400 focus:border-red-500 focus:outline-none dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100"
          />
        </label>
        {error && (
          <div role="alert" className="mt-3 rounded-lg border border-red-200 bg-red-50 px-3 py-2 text-xs text-red-700 dark:border-red-900/60 dark:bg-red-950/30 dark:text-red-300">
            <p className="break-words">{error}</p>
            <ErrorRecovery error={error} onRetry={() => { void remove() }} disabled={deleting || !matched} />
          </div>
        )}
        <div className="mt-5 flex justify-end gap-2">
          <button
            type="button"
            onClick={onClose}
            disabled={deleting}
            className="rounded-lg border border-gray-200 px-3 py-1.5 text-sm text-gray-600 hover:bg-gray-50 disabled:opacity-40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 dark:border-gray-700 dark:text-gray-400 dark:hover:bg-gray-800"
          >
            {t('common.cancel')}
          </button>
          <button
            type="submit"
            disabled={deleting || !matched}
            className="rounded-lg bg-red-600 px-3 py-1.5 text-sm font-medium text-white hover:bg-red-700 disabled:opacity-40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-red-500 dark:bg-red-500 dark:hover:bg-red-600"
          >
            {deleting ? t('common.saving') : t('envVars.delete')}
          </button>
        </div>
      </form>
    </Modal>
  )
}

export function EnvManager() {
  const { t } = useTranslation()
  const [scope, setScope] = useState<Scope>('user')
  const [vars, setVars] = useState<EnvVar[]>([])
  const [error, setError] = useState('')
  const [loading, setLoading] = useState(false)
  const [query, setQuery] = useState('')
  const [editing, setEditing] = useState<EnvVar | null>(null)
  const [deleting, setDeleting] = useState<EnvVar | null>(null)
  const [dialogOpen, setDialogOpen] = useState(false)
  // 上次写入成功后展示「需重启才生效」的提示条。
  const [changed, setChanged] = useState(false)

  const refresh = useCallback(async () => {
    setLoading(true)
    const started = Date.now()
    try {
      const list = await invoke<EnvVar[]>('env_vars_list', { scope })
      setVars(list)
      setError('')
    } catch (cause) {
      setError(String(cause))
    } finally {
      // 注册表读取通常几十毫秒完成，loading 一闪而过像按钮没反应；
      // 补足最短 600ms 让旋转动画可见（数据在到达时已先行渲染）。
      const remain = 600 - (Date.now() - started)
      if (remain > 0) await new Promise(resolve => { setTimeout(resolve, remain) })
      setLoading(false)
    }
  }, [scope])

  useEffect(() => { void refresh() }, [refresh])

  const filtered = useMemo(() => {
    const keyword = query.trim().toLowerCase()
    if (!keyword) return vars
    return vars.filter(v =>
      v.name.toLowerCase().includes(keyword) || v.value.toLowerCase().includes(keyword))
  }, [vars, query])

  return (
    <div className="flex h-full flex-col bg-[var(--app-canvas)]">
      {/* Header */}
      <div className="border-b border-gray-200 bg-white px-[var(--app-page-gutter)] py-4 dark:border-gray-700 dark:bg-gray-900">
        <div className="flex items-center justify-between">
          <div>
            <h1 className="font-semibold text-gray-900 dark:text-gray-100">{t('envVars.title')}</h1>
            <p className="text-xs text-gray-500 dark:text-gray-400">{t('envVars.subtitle', { count: vars.length })}</p>
          </div>
          <div className="flex items-center gap-2">
            <button
              type="button"
              onClick={() => { void refresh() }}
              disabled={loading}
              className="flex items-center gap-1.5 rounded-lg border border-gray-200 px-3 py-1.5 text-sm text-gray-600 hover:bg-gray-50 disabled:opacity-40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 dark:border-gray-700 dark:text-gray-400 dark:hover:bg-gray-800"
            >
              <RefreshCw className={`h-4 w-4 ${loading ? 'animate-spin' : ''}`} />
              {t('envVars.refresh')}
            </button>
            <button
              type="button"
              onClick={() => { setEditing(null); setDialogOpen(true) }}
              className="flex items-center gap-1.5 rounded-lg bg-blue-600 px-3 py-1.5 text-sm font-medium text-white hover:bg-blue-700 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 dark:bg-blue-500 dark:hover:bg-blue-600"
            >
              <Plus className="h-4 w-4" />
              {t('envVars.add')}
            </button>
          </div>
        </div>
      </div>

      <div className="flex-1 overflow-auto px-[var(--app-page-gutter)] py-6">
        {changed && (
          <div role="status" className="mb-4 flex items-start gap-2 rounded-xl border border-blue-200 bg-blue-50 px-4 py-3 text-sm text-blue-800 dark:border-blue-900/60 dark:bg-blue-950/30 dark:text-blue-300">
            <Variable className="mt-0.5 h-4 w-4 shrink-0" />
            <p>{t('envVars.restartHint')}</p>
          </div>
        )}
        {error && (
          <div role="alert" className="mb-4 rounded-xl border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800 dark:border-amber-900/60 dark:bg-amber-950/30 dark:text-amber-300">
            <div className="flex items-center gap-1.5 font-medium">
              <AlertTriangle className="h-4 w-4" />{t('envVars.loadFailed')}
            </div>
            <p className="mt-1 max-h-24 overflow-auto break-words text-xs">{error}</p>
            <ErrorRecovery error={error} onRetry={() => { void refresh() }} disabled={loading} />
          </div>
        )}

        {/* 作用域切换 + 搜索 */}
        <div className="flex flex-wrap items-center gap-2">
          <div className="flex rounded-lg border border-gray-200 p-0.5 dark:border-gray-700">
            {(['user', 'system'] as const).map(s => (
              <button
                key={s}
                type="button"
                onClick={() => { setScope(s); setQuery('') }}
                aria-pressed={scope === s}
                className={`rounded-md px-3 py-1.5 text-sm font-medium transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 ${
                  scope === s
                    ? 'bg-blue-600 text-white dark:bg-blue-500'
                    : 'text-gray-500 hover:bg-gray-100 hover:text-gray-800 dark:text-gray-400 dark:hover:bg-gray-800 dark:hover:text-gray-200'
                }`}
              >
                {s === 'user' ? t('envVars.scopeUser') : t('envVars.scopeSystem')}
              </button>
            ))}
          </div>
          <div className="relative min-w-0 flex-1 sm:max-w-xs">
            <Search className="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-gray-500" />
            <input
              value={query}
              onChange={e => { setQuery(e.target.value) }}
              spellCheck={false}
              placeholder={t('envVars.searchPlaceholder')}
              className="w-full rounded-lg border border-gray-300 bg-white py-1.5 pl-8 pr-3 text-sm text-gray-900 placeholder:text-gray-400 focus:border-blue-500 focus:outline-none dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100"
            />
          </div>
          {scope === 'system' && (
            <p className="text-xs text-gray-500 dark:text-gray-400">{t('envVars.systemAdminHint')}</p>
          )}
        </div>

        {/* 变量列表 */}
        <div className="mt-3 overflow-hidden rounded-2xl border border-gray-200 bg-white dark:border-gray-700 dark:bg-gray-900">
          {filtered.length === 0 ? (
            <p className="px-4 py-8 text-center text-sm text-gray-500 dark:text-gray-400">
              {error ? '' : t('envVars.empty')}
            </p>
          ) : (
            <ul className="divide-y divide-gray-100 dark:divide-gray-800">
              {filtered.map(v => (
                <li key={v.name} className="flex items-start gap-3 px-4 py-2.5">
                  <div className="min-w-0 flex-1">
                    <div className="flex flex-wrap items-center gap-2">
                      <span className="break-all font-mono text-sm font-medium text-gray-900 dark:text-gray-100">{v.name}</span>
                      {v.is_expand && (
                        <span
                          className="shrink-0 rounded bg-amber-50 px-1.5 py-0.5 font-sans text-[11px] font-medium text-amber-600 dark:bg-amber-900/30 dark:text-amber-400"
                          title={t('envVars.expandBadgeHint')}
                        >
                          {t('envVars.expandBadge')}
                        </span>
                      )}
                    </div>
                    {isPathListVar(v.name, v.value) ? (
                      /* 路径列表：逐条分行展示（空段不显示），不再挤成一长串。 */
                      <ul className="mt-0.5 space-y-0.5">
                        {v.value.split(';').filter(entry => entry !== '').map((entry, index) => (
                          <li key={index} className="break-all font-mono text-xs leading-relaxed text-gray-600 dark:text-gray-300">
                            <span className="mr-1 select-none text-gray-300 dark:text-gray-400">·</span>{entry}
                          </li>
                        ))}
                      </ul>
                    ) : (
                      <>
                        <p className="mt-0.5 break-all font-mono text-xs leading-relaxed text-gray-600 dark:text-gray-300">{v.value}</p>
                        {v.expanded && (
                          <p className="mt-0.5 break-all font-mono text-xs leading-relaxed text-gray-500 dark:text-gray-400">
                            <span className="mr-1 font-sans">→</span>{v.expanded}
                          </p>
                        )}
                      </>
                    )}
                  </div>
                  <div className="flex shrink-0 items-center gap-1 pt-0.5">
                    <button
                      type="button"
                      onClick={() => { setEditing(v); setDialogOpen(true) }}
                      title={t('envVars.edit')}
                      aria-label={`${t('envVars.edit')} ${v.name}`}
                      className="rounded p-1.5 text-gray-500 transition-colors hover:bg-gray-100 hover:text-gray-600 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 dark:hover:bg-gray-800 dark:hover:text-gray-300"
                    >
                      <Pencil className="h-3.5 w-3.5" />
                    </button>
                    <button
                      type="button"
                      onClick={() => { setDeleting(v) }}
                      title={t('envVars.delete')}
                      aria-label={`${t('envVars.delete')} ${v.name}`}
                      className="rounded p-1.5 text-gray-500 transition-colors hover:bg-red-50 hover:text-red-600 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-red-500 dark:hover:bg-red-950/40 dark:hover:text-red-400"
                    >
                      <Trash2 className="h-3.5 w-3.5" />
                    </button>
                  </div>
                </li>
              ))}
            </ul>
          )}
        </div>

        <p className="mt-3 text-xs text-gray-500 dark:text-gray-400">{t('envVars.footerHint')}</p>
      </div>

      {dialogOpen && (
        <VarDialog
          scope={scope}
          initial={editing}
          existing={vars}
          onClose={() => { setDialogOpen(false); setEditing(null) }}
          onSaved={() => { setDialogOpen(false); setEditing(null); setChanged(true); void refresh() }}
        />
      )}
      {deleting && (
        <DeleteDialog
          scope={scope}
          item={deleting}
          onClose={() => { setDeleting(null) }}
          onSaved={() => { setDeleting(null); setChanged(true); void refresh() }}
        />
      )}
    </div>
  )
}
