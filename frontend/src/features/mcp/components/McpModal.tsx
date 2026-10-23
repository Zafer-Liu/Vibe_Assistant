import { useEffect, useRef, type ReactNode } from 'react'

export function McpModal({ label, busy = false, size = 'lg', onClose, children }: {
  label: string
  busy?: boolean
  size?: 'md' | 'lg' | '2xl'
  onClose: () => void
  children: ReactNode
}) {
  const modal = useRef<HTMLDialogElement>(null)
  useEffect(() => {
    const previous = document.activeElement
    const dialog = modal.current
    dialog?.showModal()
    return () => {
      dialog?.close()
      if (previous instanceof HTMLElement && previous.isConnected) previous.focus()
    }
  }, [])
  const widths = { md: 'max-w-md', lg: 'max-w-lg', '2xl': 'max-w-2xl' }
  return <dialog ref={modal} aria-label={label}
    onCancel={event => { event.preventDefault(); if (!busy) onClose() }}
    onClick={event => { if (event.target === event.currentTarget && !busy) onClose() }}
    className={`fixed inset-0 m-auto max-h-[90vh] w-[calc(100vw-2rem)] ${widths[size]} overflow-visible border-0 bg-transparent p-0 text-gray-900 backdrop:bg-gray-950/40 dark:text-gray-100`}>
    {children}
  </dialog>
}
