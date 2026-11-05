import { Fragment, useCallback, useEffect, useId, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { AlertTriangle, Info } from 'lucide-react'
import { Modal } from './Modal'

export interface ConfirmDetail {
  label: string
  value: string
}

export interface ConfirmOptions {
  title: string
  message: string
  /** 受影响对象清单（技能名、客户端、记忆内容等），让用户在确认前看清具体范围。 */
  detailLines?: ConfirmDetail[]
  /** 确认按钮文案：写明具体动作（如“删除”“覆盖”），不要用笼统的“确定”。 */
  confirmLabel: string
  /** 缺省为 common.cancel。 */
  cancelLabel?: string
  /** danger：红色确认按钮，且初始焦点落在取消；default：普通确认。 */
  tone?: 'danger' | 'default'
}

// 基于 Modal 的确认弹窗，取代原生 window.confirm：可展示受影响对象，不阻塞 WebView。
// Esc 与取消都按“取消”处理；Modal 默认不响应背景点击，因此点背景不会确认。
export function ConfirmDialog({ title, message, detailLines, confirmLabel, cancelLabel, tone = 'default', onResolve }: ConfirmOptions & {
  onResolve: (confirmed: boolean) => void
}) {
  const { t } = useTranslation()
  const titleId = useId()
  const danger = tone === 'danger'
  return (
    <Modal labelledBy={titleId} onClose={() => onResolve(false)}>
      <div className="w-full max-w-md space-y-3 rounded-xl border border-gray-200 bg-white p-4 dark:border-gray-700 dark:bg-gray-800">
        <div className="flex items-start gap-2">
          {danger
            ? <AlertTriangle size={18} className="mt-0.5 shrink-0 text-red-500" />
            : <Info size={18} className="mt-0.5 shrink-0 text-violet-500" />}
          <h3 id={titleId} className="text-sm font-semibold leading-6 text-gray-800 dark:text-gray-100">{title}</h3>
        </div>
        <p className="whitespace-pre-line text-sm leading-6 text-gray-600 dark:text-gray-300">{message}</p>
        {detailLines && detailLines.length > 0 && (
          <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1 rounded-md bg-gray-50 px-3 py-2 text-xs dark:bg-gray-900/40">
            {detailLines.map((line, index) => (
              <Fragment key={`${index}:${line.label}`}>
                <dt className="text-gray-500 dark:text-gray-400">{line.label}</dt>
                <dd className="max-h-24 min-w-0 overflow-y-auto whitespace-pre-wrap break-words font-medium text-gray-800 dark:text-gray-100">{line.value}</dd>
              </Fragment>
            ))}
          </dl>
        )}
        <div className="flex justify-end gap-2">
          <button
            type="button"
            data-autofocus={danger || undefined}
            onClick={() => onResolve(false)}
            className="rounded-md border border-gray-300 px-3 py-1.5 text-sm text-gray-600 hover:bg-gray-50 dark:border-gray-600 dark:text-gray-300 dark:hover:bg-gray-700"
          >
            {cancelLabel ?? t('common.cancel')}
          </button>
          <button
            type="button"
            data-autofocus={danger ? undefined : true}
            onClick={() => onResolve(true)}
            className={`rounded-md px-3 py-1.5 text-sm font-medium text-white ${danger ? 'bg-red-600 hover:bg-red-500' : 'bg-violet-600 hover:bg-violet-500'}`}
          >
            {confirmLabel}
          </button>
        </div>
      </div>
    </Modal>
  )
}

// 命令式确认：调用方在 JSX 中渲染一次 {confirmDialog}，随后
//   if (!(await confirm({ title, message, confirmLabel }))) return
// 取消、Esc、被新的确认顶替、或组件卸载时，挂起的 Promise 都以 false 结束。
export function useConfirmDialog() {
  const [pending, setPending] = useState<{ id: number; options: ConfirmOptions } | null>(null)
  const resolver = useRef<((confirmed: boolean) => void) | null>(null)
  const sequence = useRef(0)

  const settle = useCallback((confirmed: boolean) => {
    const resolve = resolver.current
    resolver.current = null
    setPending(null)
    resolve?.(confirmed)
  }, [])

  const confirm = useCallback((options: ConfirmOptions) => new Promise<boolean>((resolve) => {
    resolver.current?.(false)
    resolver.current = resolve
    sequence.current += 1
    setPending({ id: sequence.current, options })
  }), [])

  useEffect(() => () => {
    resolver.current?.(false)
    resolver.current = null
  }, [])

  const confirmDialog = pending
    ? <ConfirmDialog key={pending.id} {...pending.options} onResolve={settle} />
    : null
  return { confirm, confirmDialog }
}
