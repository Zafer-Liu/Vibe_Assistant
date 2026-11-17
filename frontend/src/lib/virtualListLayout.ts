export interface VirtualRow {
  key: string
  first: number
  count: number
  top: number
  height: number
}

export interface VirtualLayout {
  rows: VirtualRow[]
  totalHeight: number
}

export function buildVirtualLayout(
  keys: readonly string[],
  measured: ReadonlyMap<string, number>,
  estimateHeight: number,
  gap: number,
  columns = 1,
): VirtualLayout {
  const rows: VirtualRow[] = []
  const lanes = Math.max(1, Math.floor(columns))
  const estimate = Math.max(1, estimateHeight)
  const spacing = Math.max(0, gap)
  let top = 0
  for (let first = 0; first < keys.length; first += lanes) {
    const count = Math.min(lanes, keys.length - first)
    const key = JSON.stringify(keys.slice(first, first + count))
    const height = Math.max(1, measured.get(key) ?? estimate)
    rows.push({ key, first, count, top, height })
    top += height + spacing
  }
  return { rows, totalHeight: rows.length ? top - spacing : 0 }
}

/** Find the row containing this offset, including the gap after that row. */
export function virtualRowAt(rows: readonly VirtualRow[], offset: number): number {
  if (!rows.length) return -1
  let low = 0
  let high = rows.length - 1
  while (low < high) {
    const middle = Math.ceil((low + high) / 2)
    if (rows[middle].top <= offset) low = middle
    else high = middle - 1
  }
  return low
}

/** A half-open row range; independent of total inventory size. */
export function virtualWindow(
  rows: readonly VirtualRow[], scrollTop: number, viewportHeight: number, overscan: number,
): readonly [number, number] {
  if (!rows.length) return [0, 0]
  const extra = Math.max(0, Math.floor(overscan))
  const first = virtualRowAt(rows, Math.max(0, scrollTop))
  const last = virtualRowAt(rows, Math.max(0, scrollTop) + Math.max(0, viewportHeight))
  return [Math.max(0, first - extra), Math.min(rows.length, last + extra + 1)]
}
