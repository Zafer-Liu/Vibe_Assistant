import { useEffect, useRef, type ReactNode } from 'react'

// 通用原生 <dialog> 模态：由调用方条件渲染（{open && <Modal>}），挂载时 showModal()，卸载时关闭并还原焦点。
// Esc 与（可选的）点击背景都会走 onClose；busy 为 true 时一律不关闭。
// 背景点击默认不关闭，避免编辑类弹窗误触丢失未保存内容。
// 子元素里带 data-autofocus 的元素会在打开时获得初始焦点，否则落在第一个可聚焦元素上。
export function Modal({ label, labelledBy, busy = false, dismissOnBackdrop = false, onClose, children }: {
  label?: string
  labelledBy?: string
  busy?: boolean
  dismissOnBackdrop?: boolean
  onClose: () => void
  children: ReactNode
}) {
  const modal = useRef<HTMLDialogElement>(null)
  const pressedBackdrop = useRef(false)
  const unmounting = useRef(false)
  useEffect(() => {
    const dialog = modal.current
    if (!dialog) return
    const previous = document.activeElement
    unmounting.current = false
    dialog.showModal()
    dialog.querySelector<HTMLElement>('[data-autofocus]')?.focus()
    return () => {
      unmounting.current = true
      dialog.close()
      if (previous instanceof HTMLElement && previous.isConnected) previous.focus()
    }
  }, [])
  return <dialog ref={modal} aria-label={label} aria-labelledby={labelledBy}
    onCancel={event => { event.preventDefault(); if (!busy) onClose() }}
    // 原生 close：Chromium/WebView2 对「没有新用户激活的第二次 Esc」不允许取消 cancel，对话框会被直接关掉。
    // busy 时把它重新打开，避免界面状态仍是「已打开」而弹窗不可见、触发按钮又无法再次打开；
    // 非 busy 时按普通关闭处理。卸载引起的 close、以及被重新 showModal 的瞬间都直接忽略。
    onClose={event => {
      const dialog = event.currentTarget
      if (unmounting.current || dialog.open) return
      if (busy) dialog.showModal()
      else onClose()
    }}
    onPointerDown={event => { pressedBackdrop.current = event.target === event.currentTarget }}
    onClick={event => { if (dismissOnBackdrop && !busy && pressedBackdrop.current && event.target === event.currentTarget) onClose() }}
    className="fixed inset-0 m-0 h-full max-h-none w-full max-w-none border-0 bg-transparent p-4 backdrop:bg-black/40 open:flex open:items-center open:justify-center">
    {children}
  </dialog>
}
