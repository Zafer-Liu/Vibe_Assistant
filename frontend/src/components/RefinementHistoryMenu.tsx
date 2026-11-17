import { useEffect, useId, useLayoutEffect, useRef, useState } from 'react'
import { Check, ChevronDown, Trash2 } from 'lucide-react'

interface HistoryOption {
  id: string
  label: string
  canDelete: boolean
}

/** History needs two independent actions per row, rather than buttons nested in native options. */
export function RefinementHistoryMenu({ value, options, label, placeholder, disabled, className,
  deleteLabel, deleteHint, onSelect, onDelete }: {
  value: string
  options: readonly HistoryOption[]
  label: string
  placeholder: string
  disabled: boolean
  className: string
  deleteLabel: string
  deleteHint: string
  onSelect: (id: string) => void
  onDelete: (id: string) => void
}) {
  const id = useId()
  const trigger = useRef<HTMLButtonElement>(null)
  const popup = useRef<HTMLDivElement>(null)
  const initialIndex = useRef(0)
  const [expanded, setExpanded] = useState(false)
  const open = expanded && !disabled && options.length > 0
  const selected = options.find((item) => item.id === value)

  function close(focus = true) {
    setExpanded(false)
    if (focus) trigger.current?.focus()
  }

  useEffect(() => { if (disabled) setExpanded(false) }, [disabled])

  useLayoutEffect(() => {
    const field = trigger.current
    const menu = popup.current
    if (!field || !menu || !open) return
    const rect = field.getBoundingClientRect()
    const width = Math.min(rect.width, window.innerWidth - 16)
    menu.style.width = `${width}px`
    menu.style.left = `${Math.max(8, Math.min(rect.left, window.innerWidth - width - 8))}px`
    menu.style.top = `${rect.bottom + 4}px`
    menu.style.maxHeight = `${Math.max(0, Math.min(320, window.innerHeight - rect.bottom - 12))}px`
    menu.showPopover()
    const buttons = menu.querySelectorAll<HTMLButtonElement>('[data-history-select]')
    const index = Math.max(0, Math.min(initialIndex.current, buttons.length - 1))
    buttons[index]?.focus({ preventScroll: true })
    buttons[index]?.scrollIntoView({ block: 'nearest' })
    return () => { if (menu.matches(':popover-open')) menu.hidePopover() }
  }, [open])

  useEffect(() => {
    if (!open) return
    function dismiss(event: Event) {
      if (event.target instanceof Node && (popup.current?.contains(event.target) || trigger.current?.contains(event.target))) return
      setExpanded(false)
    }
    function resize() { setExpanded(false) }
    document.addEventListener('pointerdown', dismiss)
    document.addEventListener('focusin', dismiss)
    document.addEventListener('scroll', dismiss, true)
    window.addEventListener('resize', resize)
    return () => {
      document.removeEventListener('pointerdown', dismiss)
      document.removeEventListener('focusin', dismiss)
      document.removeEventListener('scroll', dismiss, true)
      window.removeEventListener('resize', resize)
    }
  }, [open])

  return <>
    <button ref={trigger} type="button" className={`${className} flex items-center gap-2 text-left`}
      disabled={disabled || options.length === 0} aria-haspopup="dialog" aria-expanded={open}
      aria-controls={open ? `${id}-popup` : undefined} aria-label={`${label}: ${selected?.label ?? placeholder}`}
      onClick={() => {
        initialIndex.current = Math.max(0, options.findIndex((item) => item.id === value))
        setExpanded((previous) => !previous)
      }}
      onKeyDown={(event) => {
        if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
          event.preventDefault()
          initialIndex.current = event.key === 'ArrowUp' ? options.length - 1 : Math.max(0, options.findIndex((item) => item.id === value))
          setExpanded(true)
        }
      }}>
      <span className="min-w-0 flex-1 truncate">{selected?.label ?? placeholder}</span>
      <ChevronDown size={14} className={`shrink-0 ${open ? 'rotate-180' : ''}`} aria-hidden="true" />
    </button>
    <div ref={popup} id={`${id}-popup`} popover="manual" role="dialog" aria-label={label}
      className="app-combobox-list" onKeyDown={(event) => {
        if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); close(); return }
        const rows = Array.from(popup.current?.querySelectorAll<HTMLDivElement>('[data-history-row]') ?? [])
        const index = rows.findIndex((row) => row.contains(document.activeElement))
        if (index < 0) return
        if (event.key === 'ArrowDown' || event.key === 'ArrowUp' || event.key === 'Home' || event.key === 'End') {
          event.preventDefault()
          const next = event.key === 'Home' ? 0 : event.key === 'End' ? rows.length - 1
            : Math.max(0, Math.min(rows.length - 1, index + (event.key === 'ArrowDown' ? 1 : -1)))
          rows[next]?.querySelector<HTMLButtonElement>('[data-history-select]')?.focus()
        } else if (event.key === 'ArrowRight' || event.key === 'ArrowLeft') {
          event.preventDefault()
          rows[index]?.querySelector<HTMLButtonElement>(event.key === 'ArrowRight'
            ? '[data-history-delete]:not(:disabled)' : '[data-history-select]')?.focus()
        }
      }}>
      {options.map((item) => <div key={item.id} data-history-row className={`flex items-center gap-1 rounded ${item.id === value ? 'bg-[var(--app-accent-soft)]' : 'hover:bg-[var(--app-hover)]'}`}>
        <button type="button" data-history-select aria-current={item.id === value ? 'true' : undefined}
          className="flex min-h-8 min-w-0 flex-1 items-center gap-2 rounded px-2 py-1.5 text-left focus-visible:outline-2 focus-visible:outline-offset-[-2px] focus-visible:outline-[var(--app-focus)]"
          onClick={() => { close(); onSelect(item.id) }}>
          <span className="w-4 shrink-0" aria-hidden="true">{item.id === value && <Check size={14} className="text-[var(--app-accent)]" />}</span>
          <span className="min-w-0 break-words [overflow-wrap:anywhere]">{item.label}</span>
        </button>
        <button type="button" data-history-delete disabled={!item.canDelete}
          aria-label={`${deleteLabel}: ${item.label}`} title={item.canDelete ? deleteLabel : deleteHint}
          className="inline-flex min-h-8 min-w-8 shrink-0 items-center justify-center rounded text-red-600 hover:bg-red-50 focus-visible:outline-2 focus-visible:outline-offset-[-2px] focus-visible:outline-[var(--app-focus)] disabled:cursor-not-allowed disabled:opacity-40 dark:text-red-400 dark:hover:bg-red-950/40"
          onClick={() => { close(); onDelete(item.id) }}><Trash2 size={14} aria-hidden="true" /></button>
      </div>)}
    </div>
  </>
}
