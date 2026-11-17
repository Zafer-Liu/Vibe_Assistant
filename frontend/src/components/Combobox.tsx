import { useEffect, useId, useLayoutEffect, useRef, useState } from 'react'
import { ChevronDown } from 'lucide-react'

interface ComboboxProps {
  value: string
  options: readonly string[]
  onValueChange: (value: string) => void
  label: string
  placeholder?: string
  disabled?: boolean
  className?: string
}

/** Editable suggestions; focus stays in the input and arbitrary values remain valid. */
export function Combobox({ value, options, onValueChange, label, placeholder, disabled, className = '' }: ComboboxProps) {
  const id = useId()
  const input = useRef<HTMLInputElement>(null)
  const list = useRef<HTMLDivElement>(null)
  const [expanded, setExpanded] = useState(false)
  const [activeIndex, setActiveIndex] = useState(-1)
  const matches = [...new Set(options)].filter(option => option.toLocaleLowerCase().includes(value.toLocaleLowerCase().trim()))
  const open = expanded && !disabled && matches.length > 0
  const active = activeIndex >= 0 ? Math.min(activeIndex, matches.length - 1) : -1

  useLayoutEffect(() => {
    const popup = list.current
    const field = input.current
    if (!popup || !field || !open) return

    const rect = field.getBoundingClientRect()
    const width = Math.min(rect.width, window.innerWidth - 16)
    popup.style.width = `${width}px`
    popup.style.left = `${Math.max(8, Math.min(rect.left, window.innerWidth - width - 8))}px`
    popup.showPopover()
    const below = window.innerHeight - rect.bottom - 12
    popup.style.maxHeight = `${Math.max(0, Math.min(300, below))}px`
    popup.style.top = `${rect.bottom + 4}px`

    return () => { popup.hidePopover() }
  }, [open, matches.length])

  useEffect(() => {
    if (!open) return
    function dismiss() { setExpanded(false) }
    function onScroll(event: Event) {
      if (event.target instanceof Node && list.current?.contains(event.target)) return
      dismiss()
    }
    window.addEventListener('resize', dismiss)
    function onPointerDown(event: PointerEvent) {
      if (event.target instanceof Node && (input.current?.contains(event.target) || list.current?.contains(event.target))) return
      dismiss()
    }
    document.addEventListener('scroll', onScroll, true)
    document.addEventListener('pointerdown', onPointerDown)
    return () => {
      window.removeEventListener('resize', dismiss)
      document.removeEventListener('scroll', onScroll, true)
      document.removeEventListener('pointerdown', onPointerDown)
    }
  }, [open])

  useEffect(() => {
    if (open && active >= 0) document.getElementById(`${id}-option-${active}`)?.scrollIntoView({ block: 'nearest' })
  }, [active, id, open])

  function choose(option: string) {
    onValueChange(option)
    setExpanded(false)
    setActiveIndex(-1)
  }

  return <div className={`app-combobox ${className}`}>
    <input
      ref={input}
      role="combobox"
      aria-label={label}
      aria-expanded={open}
      aria-controls={open ? `${id}-list` : undefined}
      aria-autocomplete="list"
      aria-activedescendant={open && active >= 0 ? `${id}-option-${active}` : undefined}
      autoComplete="off"
      value={value}
      placeholder={placeholder}
      disabled={disabled}
      className="app-combobox-input"
      onFocus={() => { setExpanded(true); setActiveIndex(-1) }}
      onClick={() => setExpanded(true)}
      onBlur={() => setExpanded(false)}
      onChange={event => { onValueChange(event.target.value); setExpanded(true); setActiveIndex(-1) }}
      onKeyDown={event => {
        if (event.nativeEvent.isComposing) return
        if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
          if (matches.length === 0) return
          event.preventDefault()
          setExpanded(true)
          setActiveIndex(event.key === 'ArrowDown'
            ? (open ? Math.min(active + 1, matches.length - 1) : 0)
            : (open && active >= 0 ? Math.max(active - 1, 0) : matches.length - 1))
        } else if (event.key === 'Enter' && open && active >= 0) {
          event.preventDefault()
          choose(matches[active])
        } else if (event.key === 'Escape' && open) {
          event.preventDefault()
          event.stopPropagation()
          setExpanded(false)
        } else if (event.key === 'Tab') {
          setExpanded(false)
        }
      }}
    />
    {options.length > 0 && <ChevronDown size={14} className="app-combobox-chevron" aria-hidden="true" />}
    <div ref={list} id={`${id}-list`} role="listbox" aria-label={label} popover="manual" className="app-combobox-list">
      {matches.map((option, index) => <div
        key={option}
        id={`${id}-option-${index}`}
        role="option"
        aria-selected={option === value}
        data-selected={option === value}
        data-active={index === active}
        className="app-combobox-option"
        onPointerDown={event => event.preventDefault()}
        onPointerMove={() => setActiveIndex(index)}
        onClick={event => { event.preventDefault(); choose(option) }}
      >{option}</div>)}
    </div>
  </div>
}
