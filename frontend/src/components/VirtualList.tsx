import { memo, useCallback, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from 'react'
import { buildVirtualLayout, virtualRowAt, virtualWindow, type VirtualRow } from '../lib/virtualListLayout'

interface VirtualListProps<T> {
  items: readonly T[]
  getKey: (item: T, index: number) => string
  renderItem: (item: T, index: number) => ReactNode
  estimateHeight?: number
  gap?: number
  overscan?: number
  className?: string
  contentClassName?: string
  active?: boolean
  resetKey?: string | number
  minimumColumnWidth?: number
  columnGap?: number
  'aria-label'?: string
}

const FOCUSABLE = 'button:not([disabled]),a[href],input:not([disabled]),select:not([disabled]),textarea:not([disabled]),[tabindex]:not([tabindex="-1"])'

const MeasuredRow = memo(function MeasuredRow({ row, columns, width, columnGap, onMeasure, children }: {
  row: VirtualRow
  columns: number
  width: number
  columnGap: number
  onMeasure: (key: string, height: number) => void
  children: ReactNode
}) {
  const element = useRef<HTMLDivElement>(null)
  useLayoutEffect(() => {
    const node = element.current
    if (!node) return
    const measure = () => {
      const bounds = node.getBoundingClientRect()
      if (bounds.width > 0) onMeasure(row.key, bounds.height)
    }
    measure()
    const observer = new ResizeObserver(measure)
    observer.observe(node)
    return () => observer.disconnect()
  }, [row.key, onMeasure, width])
  return <div ref={element} style={{
    position: 'absolute', top: 0, left: 0, right: 0,
    transform: `translateY(${row.top}px)`, display: 'grid',
    gridTemplateColumns: `repeat(${columns}, minmax(0, 1fr))`, gap: columnGap,
  }}>{children}</div>
})

/** Window variable-height cards while keeping selection/state in the caller. */
export function VirtualList<T>({
  items, getKey, renderItem, estimateHeight = 100, gap = 6, overscan = 4,
  className = '', contentClassName = '', active = true, resetKey,
  minimumColumnWidth, columnGap = 12, 'aria-label': ariaLabel,
}: VirtualListProps<T>) {
  const viewport = useRef<HTMLDivElement>(null)
  const content = useRef<HTMLDivElement>(null)
  const activeRef = useRef(active)
  const [measured, setMeasured] = useState<ReadonlyMap<string, number>>(() => new Map())
  const pendingMeasures = useRef(new Map<string, number>())
  const measureFrame = useRef<number | null>(null)
  const scrollFrame = useRef<number | null>(null)
  const anchor = useRef<{ key: string; offset: number } | null>(null)
  const pendingFocus = useRef<{ key: string; last: boolean; control?: number } | null>(null)
  const focusedControl = useRef<{ key: string; control: number } | null>(null)
  const previousReset = useRef(resetKey)
  const [focusedKey, setFocusedKey] = useState<string | null>(null)
  const [view, setView] = useState({ top: 0, height: 0, width: 0, paddingTop: 0 })
  const keys = useMemo(() => items.map(getKey), [items, getKey])
  const indexByKey = useMemo(() => new Map(keys.map((key, index) => [key, index])), [keys])
  const columns = minimumColumnWidth
    ? Math.max(1, Math.floor((view.width + columnGap) / (minimumColumnWidth + columnGap))) : 1
  const previousColumns = useRef(columns)
  const layout = useMemo(() => buildVirtualLayout(keys, measured, estimateHeight, gap, columns),
    [keys, measured, estimateHeight, gap, columns])
  const [start, end] = virtualWindow(layout.rows, Math.max(0, view.top - view.paddingTop),
    view.height || Math.min(600, estimateHeight * 8), overscan)
  const renderedRows = useMemo(() => {
    const indices = new Set<number>()
    for (let index = start; index < end; index += 1) indices.add(index)
    const focused = focusedKey === null ? undefined : indexByKey.get(focusedKey)
    if (focused !== undefined) indices.add(Math.floor(focused / columns))
    return [...indices].sort((left, right) => left - right).map((index) => layout.rows[index])
  }, [start, end, focusedKey, indexByKey, columns, layout.rows])

  const onMeasure = useCallback((key: string, height: number) => {
    if (!activeRef.current || !Number.isFinite(height) || height <= 0) return
    pendingMeasures.current.set(key, height)
    if (measureFrame.current !== null) return
    measureFrame.current = requestAnimationFrame(() => {
      measureFrame.current = null
      if (!activeRef.current) { pendingMeasures.current.clear(); return }
      const changes = new Map(pendingMeasures.current)
      pendingMeasures.current.clear()
      setMeasured((current) => {
        let next: Map<string, number> | null = null
        for (const [rowKey, size] of changes) {
          if (Math.abs((current.get(rowKey) ?? -1) - size) > 0.5) {
            next ??= new Map(current)
            next.set(rowKey, size)
          }
        }
        return next ?? current
      })
    })
  }, [])

  useLayoutEffect(() => { activeRef.current = active }, [active])
  useLayoutEffect(() => {
    const retained = new Set<string>()
    for (let first = 0; first < keys.length; first += columns) retained.add(JSON.stringify(keys.slice(first, first + columns)))
    for (const key of pendingMeasures.current.keys()) if (!retained.has(key)) pendingMeasures.current.delete(key)
    setMeasured((current) => {
      const next = new Map([...current].filter(([key]) => retained.has(key)))
      return next.size === current.size ? current : next
    })
  }, [keys, columns])
  useLayoutEffect(() => {
    if (!active) return
    const node = viewport.current
    const body = content.current
    if (!node || !body) return
    let frame: number | null = null
    const measure = () => {
      if (frame !== null) return
      frame = requestAnimationFrame(() => {
        frame = null
        if (!node.clientHeight || !body.clientWidth) return
        const style = getComputedStyle(body)
        // Width is tracked for column regrouping only. Height re-measurement is
        // owned by each row's ResizeObserver; wiping measurements on scrollbar
        // appearing/disappearing would oscillate: measure -> clear -> estimate
        // past the viewport -> scrollbar returns -> clear again.
        const width = body.clientWidth - parseFloat(style.paddingLeft || '0') - parseFloat(style.paddingRight || '0')
        const next = { top: node.scrollTop, height: node.clientHeight, width, paddingTop: parseFloat(style.paddingTop || '0') }
        setView((current) => current.top === next.top && current.height === next.height
          && current.width === next.width && current.paddingTop === next.paddingTop ? current : next)
      })
    }
    measure()
    const observer = new ResizeObserver(measure)
    observer.observe(node)
    observer.observe(body)
    return () => { observer.disconnect(); if (frame !== null) cancelAnimationFrame(frame) }
  }, [active])

  useLayoutEffect(() => {
    const node = viewport.current
    if (!node || !active) return
    if (previousReset.current !== resetKey) {
      previousReset.current = resetKey
      anchor.current = null
      pendingFocus.current = null
      focusedControl.current = null
      if (node.contains(document.activeElement)) node.focus({ preventScroll: true })
      setFocusedKey(null)
      node.scrollTop = 0
      setView((current) => ({ ...current, top: 0 }))
    } else if (anchor.current) {
      const index = indexByKey.get(anchor.current.key)
      if (index !== undefined) {
        const row = layout.rows[Math.floor(index / columns)]
        const target = row.top + anchor.current.offset + view.paddingTop
        if (Math.abs(node.scrollTop - target) > 1) {
          node.scrollTop = target
          setView((current) => ({ ...current, top: node.scrollTop }))
        }
      }
    }
    if (previousColumns.current !== columns) {
      previousColumns.current = columns
      const remembered = focusedControl.current
      if (remembered && (document.activeElement === document.body || document.activeElement === node)) {
        pendingFocus.current = { ...remembered, last: false }
      }
    }
    const requested = pendingFocus.current
    if (requested) {
      const item = [...node.querySelectorAll<HTMLElement>('[data-virtual-item]')]
        .find((element) => element.dataset.virtualItem === requested.key)
      if (item) {
        const controls = [...item.querySelectorAll<HTMLElement>(FOCUSABLE)].filter((element) => element.getClientRects().length)
        const target = requested.control === undefined
          ? requested.last ? controls.at(-1) : controls[0]
          : controls[requested.control]
        const focusTarget = target ?? item
        focusTarget.focus({ preventScroll: true })
        pendingFocus.current = null
      } else {
        // The target item is not mounted (deleted or outside the window).
        // Drop the request instead of letting it steal focus on a later render.
        pendingFocus.current = null
      }
    }
  }, [active, layout, indexByKey, columns, resetKey, renderedRows, view.paddingTop])

  useLayoutEffect(() => () => {
    if (measureFrame.current !== null) cancelAnimationFrame(measureFrame.current)
    if (scrollFrame.current !== null) cancelAnimationFrame(scrollFrame.current)
    measureFrame.current = null
    scrollFrame.current = null
    pendingMeasures.current.clear()
    activeRef.current = false
  }, [])

  const onScroll = () => {
    if (scrollFrame.current !== null) return
    scrollFrame.current = requestAnimationFrame(() => {
      scrollFrame.current = null
      const node = viewport.current
      if (!node || !activeRef.current) return
      const top = Math.max(0, node.scrollTop - view.paddingTop)
      const rowIndex = virtualRowAt(layout.rows, top)
      const row = layout.rows[rowIndex]
      anchor.current = row ? { key: keys[row.first], offset: top - row.top } : null
      setView((current) => ({ ...current, top: node.scrollTop }))
    })
  }

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key !== 'Tab' || !(event.target instanceof HTMLElement)) return
    const item = event.target.closest<HTMLElement>('[data-virtual-item]')
    const key = item?.dataset.virtualItem
    const index = key === undefined ? undefined : indexByKey.get(key)
    if (!item || index === undefined) return
    const controls = [...item.querySelectorAll<HTMLElement>(FOCUSABLE)].filter((element) => element.getClientRects().length)
    const edge = event.shiftKey ? controls[0] : controls.at(-1)
    if (edge !== event.target) return
    const next = index + (event.shiftKey ? -1 : 1)
    if (next < 0 || next >= items.length) return
    event.preventDefault()
    const row = layout.rows[Math.floor(next / columns)]
    const node = viewport.current
    if (!node) return
    const rowTop = row.top + view.paddingTop
    if (rowTop < node.scrollTop) node.scrollTop = rowTop
    else if (rowTop + row.height > node.scrollTop + node.clientHeight) node.scrollTop = rowTop + row.height - node.clientHeight
    anchor.current = { key: keys[row.first], offset: Math.max(0, node.scrollTop - view.paddingTop) - row.top }
    pendingFocus.current = { key: keys[next], last: event.shiftKey }
    setFocusedKey(keys[next])
    setView((current) => ({ ...current, top: node.scrollTop }))
  }

  return <div ref={viewport} className={className} role="list" aria-label={ariaLabel} tabIndex={-1}
    style={{ overflowY: 'auto', overflowAnchor: 'none' }} onScroll={onScroll} onKeyDownCapture={onKeyDown}
    onFocusCapture={(event) => {
      const item = (event.target as HTMLElement).closest<HTMLElement>('[data-virtual-item]')
      if (item?.dataset.virtualItem) {
        const controls = [...item.querySelectorAll<HTMLElement>(FOCUSABLE)].filter((element) => element.getClientRects().length)
        focusedControl.current = { key: item.dataset.virtualItem, control: Math.max(0, controls.indexOf(event.target as HTMLElement)) }
        setFocusedKey(item.dataset.virtualItem)
      }
    }} onBlurCapture={(event) => {
      // A responsive regroup can remove the focused DOM node; retain its key
      // until the layout effect restores it. Real focus leaving clears the pin.
      if (event.relatedTarget && !event.currentTarget.contains(event.relatedTarget as Node)) {
        focusedControl.current = null
        setFocusedKey(null)
      }
    }}>
    <div ref={content} className={contentClassName}>
      <div style={{ position: 'relative', height: layout.totalHeight }}>
        {renderedRows.map((row) => <MeasuredRow key={row.key} row={row} columns={columns} width={view.width} columnGap={columnGap} onMeasure={onMeasure}>
          {Array.from({ length: row.count }, (_, lane) => {
            const index = row.first + lane
            return <div key={keys[index]} data-virtual-item={keys[index]} role="listitem" aria-posinset={index + 1} aria-setsize={items.length} tabIndex={-1}>
              {renderItem(items[index], index)}
            </div>
          })}
        </MeasuredRow>)}
      </div>
    </div>
  </div>
}
