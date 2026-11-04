import { memo, useCallback, useEffect, useImperativeHandle, useRef, type Ref } from 'react'
import { useTranslation } from 'react-i18next'
import { Network } from 'lucide-react'
import type { GraphNode, GraphTopology, GraphViewportController } from './types'
import { getDomainSurface } from './domainSurface'

interface GraphCanvasViewportProps {
  active: boolean
  isDark: boolean
  topology: GraphTopology
  selectedNodeId: string | null
  searchActive: boolean
  loading: boolean
  adjacencyMap: Map<string, Set<string>>
  showCrossSynapses: boolean
  onSelectNode: (nodeId: string | null) => void
  controllerRef?: Ref<GraphViewportController>
}

export const GraphCanvasViewport = memo(function GraphCanvasViewport(props: GraphCanvasViewportProps) {
  const { t } = useTranslation()
  const { active, topology, controllerRef, onSelectNode } = props
  const canvasRef = useRef<HTMLCanvasElement | null>(null)
  const containerRef = useRef<HTMLDivElement | null>(null)
  const cameraRef = useRef({ x: 0, y: 0, scale: 1, targetX: 0, targetY: 0, targetScale: 1 })
  const nodesRef = useRef<GraphNode[]>([])
  const nodeMapRef = useRef(new Map<string, GraphNode>())
  const hoveredRef = useRef<string | null>(null)
  const invalidateRef = useRef<() => void>(() => {})
  const fitRef = useRef<() => void>(() => {})
  const fittedRef = useRef(true)
  const dragRef = useRef<{
    startX: number; startY: number; cameraX: number; cameraY: number; moved: boolean
  } | null>(null)
  const latestPropsRef = useRef(props)
  latestPropsRef.current = props

  useEffect(() => {
    nodesRef.current = topology.nodes
    nodeMapRef.current = topology.nodeById
    hoveredRef.current = null
    // Each cloud selection is a new bounded view; fit it without retaining the previous domain's pan.
    fittedRef.current = true
    fitRef.current()
    invalidateRef.current()
  }, [topology])

  const focusWorldPoint = useCallback((wx: number, wy: number, scale = 1.15) => {
    fittedRef.current = false
    const cam = cameraRef.current
    cam.targetX = -wx * scale
    cam.targetY = -wy * scale
    cam.targetScale = scale
    invalidateRef.current()
  }, [])
  const resetCamera = useCallback(() => {
    fittedRef.current = true
    fitRef.current()
    invalidateRef.current()
  }, [])
  const zoom = useCallback((factor: number) => {
    fittedRef.current = false
    const cam = cameraRef.current
    const scale = Math.min(3.2, Math.max(0.1, cam.targetScale * factor))
    const ratio = scale / cam.targetScale
    cam.targetX *= ratio
    cam.targetY *= ratio
    cam.targetScale = scale
    invalidateRef.current()
  }, [])
  useImperativeHandle(controllerRef, () => ({
    zoomIn: () => zoom(1.22),
    zoomOut: () => zoom(0.82),
    resetCamera,
    focusWorldPoint,
    focusNodeById: (nodeId: string, scale = 1.15) => {
      const node = nodeMapRef.current.get(nodeId)
      if (node) focusWorldPoint(node.x, node.y, scale)
    },
  }), [focusWorldPoint, resetCamera, zoom])

  const screenToWorld = useCallback((clientX: number, clientY: number) => {
    const rect = canvasRef.current?.getBoundingClientRect()
    if (!rect) return { wx: 0, wy: 0 }
    const cam = cameraRef.current
    return {
      wx: (clientX - rect.left - rect.width / 2 - cam.x) / cam.scale,
      wy: (clientY - rect.top - rect.height / 2 - cam.y) / cam.scale,
    }
  }, [])
  const findNodeAt = useCallback((wx: number, wy: number): GraphNode | null => {
    const nodes = nodesRef.current
    const scale = cameraRef.current.scale
    let nearest: GraphNode | null = null
    let nearestDistance = Infinity
    for (const node of nodes) {
      const dx = wx - node.x
      const dy = wy - node.y
      if (node.kind === 'lobe') {
        if (Math.abs(dx) <= node.width / 2 && Math.abs(dy) <= Math.max(node.height / 2, 12 / scale)) return node
        continue
      }
      const radius = Math.max((node.radius ?? 4.5) + 3 / scale, 10 / scale)
      const distance = dx * dx + dy * dy
      if (distance <= radius * radius && distance < nearestDistance) {
        nearest = node
        nearestDistance = distance
      }
    }
    return nearest
  }, [])

  useEffect(() => {
    const canvas = canvasRef.current
    if (!canvas) return
    const onWheel = (event: WheelEvent) => {
      event.preventDefault()
      fittedRef.current = false
      const cam = cameraRef.current
      const rect = canvas.getBoundingClientRect()
      const mx = event.clientX - rect.left - rect.width / 2
      const my = event.clientY - rect.top - rect.height / 2
      const scale = Math.min(3.2, Math.max(0.1, cam.targetScale * (event.deltaY < 0 ? 1.12 : 0.89)))
      const ratio = scale / cam.targetScale
      cam.targetX = mx - (mx - cam.targetX) * ratio
      cam.targetY = my - (my - cam.targetY) * ratio
      cam.targetScale = scale
      invalidateRef.current()
    }
    canvas.addEventListener('wheel', onWheel, { passive: false })
    return () => canvas.removeEventListener('wheel', onWheel)
  }, [])

  useEffect(() => {
    if (!active) return
    const canvas = canvasRef.current
    const container = containerRef.current
    const ctx = canvas?.getContext('2d', { alpha: false })
    if (!canvas || !container || !ctx) return
    let frameId = 0
    let width = 1
    let height = 1
    let dpr = 1
    const reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)')

    const renderFrame = () => {
      frameId = 0
      if (document.hidden) return
      const latest = latestPropsRef.current
      const cam = cameraRef.current
      const moving = Math.abs(cam.targetX - cam.x) > 0.1 ||
        Math.abs(cam.targetY - cam.y) > 0.1 || Math.abs(cam.targetScale - cam.scale) > 0.0005
      const amount = reducedMotion.matches || !moving ? 1 : 0.3
      cam.x += (cam.targetX - cam.x) * amount
      cam.y += (cam.targetY - cam.y) * amount
      cam.scale += (cam.targetScale - cam.scale) * amount
      const surface = latest.isDark ? '#272727' : '#ffffff'
      const ink = latest.isDark ? '#f3f3f3' : '#242424'
      const muted = latest.isDark ? '#bdbdbd' : '#616161'
      const accent = latest.isDark ? '#60cdff' : '#0067c0'
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0)
      ctx.globalAlpha = 1
      ctx.fillStyle = surface
      ctx.fillRect(0, 0, width, height)
      ctx.translate(width / 2 + cam.x, height / 2 + cam.y)
      ctx.scale(cam.scale, cam.scale)

      const left = (-width / 2 - cam.x) / cam.scale
      const right = (width / 2 - cam.x) / cam.scale
      const top = (-height / 2 - cam.y) / cam.scale
      const bottom = (height / 2 - cam.y) / cam.scale
      const focusId = hoveredRef.current ?? latest.selectedNodeId
      const neighbors = focusId ? latest.adjacencyMap.get(focusId) : undefined
      const focusNode = focusId ? nodeMapRef.current.get(focusId) : undefined
      for (const region of latest.topology.regions) {
        if (region.x + region.width / 2 < left || region.x - region.width / 2 > right ||
          region.y + region.height / 2 < top || region.y - region.height / 2 > bottom) continue
        const palette = getDomainSurface(region.domainId)
        ctx.globalAlpha = 0.5
        ctx.beginPath()
        ctx.ellipse(region.x, region.y, region.width / 2, region.height / 2, 0, 0, Math.PI * 2)
        ctx.fillStyle = latest.isDark ? palette.dark : palette.light
        ctx.fill()
      }
      ctx.lineCap = 'round'
      if (latest.showCrossSynapses) {
        for (const edge of latest.topology.edges) {
          const src = nodeMapRef.current.get(edge.sourceId)
          const dst = nodeMapRef.current.get(edge.targetId)
          if (!src || !dst) continue
          if (Math.max(src.x, dst.x) < left || Math.min(src.x, dst.x) > right ||
            Math.max(src.y, dst.y) < top || Math.min(src.y, dst.y) > bottom) continue
          const focused = src.id === focusId || dst.id === focusId ||
            (focusNode?.kind === 'lobe' && (src.domainId === focusNode.domainId || dst.domainId === focusNode.domainId))
          const bridge = edge.kind === 'shared-keyword' || edge.kind === 'shared-memory'
          ctx.globalAlpha = focusId ? (focused ? 0.75 : 0.06) : bridge ? 0.42 : 0.28
          ctx.strokeStyle = focused ? accent : muted
          ctx.lineWidth = (focused ? 1.7 : bridge ? 1.05 : 0.7) / Math.max(0.55, cam.scale)
          ctx.beginPath()
          ctx.moveTo(src.x, src.y)
          ctx.lineTo(dst.x, dst.y)
          ctx.stroke()
        }
      }
      const labelNodes: GraphNode[] = []
      for (const node of nodesRef.current) {
        const margin = 12 / cam.scale
        if (node.x + node.width / 2 + margin < left || node.x - node.width / 2 - margin > right ||
          node.y + node.height / 2 + margin < top || node.y - node.height / 2 - margin > bottom) continue
        const focused = node.id === focusId || node.id === latest.selectedNodeId
        const sameDomain = focusNode?.kind === 'lobe' && node.domainId === focusNode.domainId
        const emphasized = focused || sameDomain || neighbors?.has(node.id)
        if (node.kind === 'lobe') {
          labelNodes.push(node)
          continue
        }
        const palette = getDomainSurface(node.domainId)
        const radius = Math.max(node.radius ?? 4.5, (node.kind === 'keyword' ? 4 : 1.8) / cam.scale)
        ctx.globalAlpha = focusId && !emphasized ? 0.22 : 0.95
        ctx.fillStyle = latest.isDark ? palette.strokeLight : palette.marker
        ctx.strokeStyle = surface
        ctx.lineWidth = 0.8 / cam.scale
        ctx.beginPath()
        if (node.kind === 'keyword') {
          ctx.moveTo(node.x, node.y - radius)
          ctx.lineTo(node.x + radius, node.y)
          ctx.lineTo(node.x, node.y + radius)
          ctx.lineTo(node.x - radius, node.y)
          ctx.closePath()
        } else {
          ctx.arc(node.x, node.y, radius, 0, Math.PI * 2)
        }
        ctx.fill()
        ctx.stroke()
        if (focused) {
          ctx.globalAlpha = 1
          ctx.strokeStyle = accent
          ctx.lineWidth = 1.5 / cam.scale
          ctx.beginPath()
          ctx.arc(node.x, node.y, radius + 3 / cam.scale, 0, Math.PI * 2)
          ctx.stroke()
        }
        if (focused || (node.kind === 'keyword' && (node.showLabel || neighbors?.has(node.id) || cam.scale >= 0.85))) {
          labelNodes.push(node)
        }
      }
      // Prioritize external category titles and selection; suppress colliding secondary labels.
      labelNodes.sort((a, b) => Number(b.kind === 'lobe') - Number(a.kind === 'lobe') ||
        Number(b.id === focusId) - Number(a.id === focusId))
      const labelRects: Array<{ x: number; y: number; width: number; height: number }> = []
      ctx.textAlign = 'center'
      ctx.textBaseline = 'middle'
      ctx.lineJoin = 'round'
      for (const node of labelNodes) {
        const focused = node.id === focusId || node.id === latest.selectedNodeId
        if (labelRects.length >= 32 && !focused && node.kind !== 'lobe') continue
        const fontSize = Math.max(node.fontSize, (node.kind === 'lobe' ? 12 : focused ? 12 : 10) / cam.scale)
        const label = node.kind === 'memory' && node.label.length > 20 ? `${node.label.slice(0, 20)}…` : node.label
        ctx.font = `${node.fontWeight} ${fontSize}px "Segoe UI", "Microsoft YaHei UI", sans-serif`
        const labelWidth = Math.min(ctx.measureText(label).width + 8 / cam.scale, (width - 24) / cam.scale)
        const labelHeight = fontSize * 1.25
        const labelX = Math.max(left + labelWidth / 2 + 6 / cam.scale,
          Math.min(right - labelWidth / 2 - 6 / cam.scale, node.x))
        const labelY = node.kind === 'lobe' ? node.y : node.y + (node.radius ?? 5) + labelHeight / 2 + 5 / cam.scale
        const box = { x: labelX, y: labelY, width: labelWidth, height: labelHeight }
        if (!focused && node.kind !== 'lobe' && labelRects.some((other) =>
          Math.abs(box.x - other.x) < (box.width + other.width) / 2 &&
          Math.abs(box.y - other.y) < (box.height + other.height) / 2)) continue
        labelRects.push(box)
        ctx.globalAlpha = 1
        ctx.strokeStyle = surface
        ctx.lineWidth = 3 / cam.scale
        ctx.strokeText(label, labelX, labelY, labelWidth - 4 / cam.scale)
        ctx.fillStyle = focused ? accent : ink
        ctx.fillText(label, labelX, labelY, labelWidth - 4 / cam.scale)
      }
      ctx.globalAlpha = 1
      if (moving && !reducedMotion.matches) invalidate()
    }
    const invalidate = () => {
      if (!frameId && !document.hidden) frameId = window.requestAnimationFrame(renderFrame)
    }
    invalidateRef.current = invalidate
    const fit = () => {
      if (!nodesRef.current.length) return
      let minX = Infinity
      let minY = Infinity
      let maxX = -Infinity
      let maxY = -Infinity
      const bounds = [...latestPropsRef.current.topology.regions,
        ...nodesRef.current.filter((node) => node.kind === 'lobe')]
      for (const region of bounds) {
        minX = Math.min(minX, region.x - region.width / 2)
        maxX = Math.max(maxX, region.x + region.width / 2)
        minY = Math.min(minY, region.y - region.height / 2)
        maxY = Math.max(maxY, region.y + region.height / 2)
      }
      const cam = cameraRef.current
      const scale = Math.max(0.1, Math.min(1.25,
        Math.max(1, width - 64) / Math.max(1, maxX - minX),
        Math.max(1, height - 100) / Math.max(1, maxY - minY)))
      cam.targetScale = scale
      cam.targetX = -(minX + maxX) / 2 * scale
      cam.targetY = -(minY + maxY) / 2 * scale - 14
    }
    fitRef.current = fit
    const resize = () => {
      const rect = container.getBoundingClientRect()
      width = Math.max(1, Math.floor(rect.width))
      height = Math.max(1, Math.floor(rect.height))
      dpr = Math.min(2, window.devicePixelRatio || 1)
      canvas.width = Math.floor(width * dpr)
      canvas.height = Math.floor(height * dpr)
      if (fittedRef.current) fit()
      invalidate()
    }
    const observer = new ResizeObserver(resize)
    observer.observe(container)
    const onVisibilityChange = () => {
      if (document.hidden) {
        window.cancelAnimationFrame(frameId)
        frameId = 0
      } else invalidate()
    }
    document.addEventListener('visibilitychange', onVisibilityChange)
    resize()
    return () => {
      window.cancelAnimationFrame(frameId)
      observer.disconnect()
      document.removeEventListener('visibilitychange', onVisibilityChange)
      invalidateRef.current = () => {}
      fitRef.current = () => {}
      dragRef.current = null
    }
  }, [active])

  useEffect(() => {
    invalidateRef.current()
  }, [props.isDark, props.selectedNodeId, props.adjacencyMap, props.showCrossSynapses])

  const handlePointerDown = useCallback((event: React.PointerEvent<HTMLCanvasElement>) => {
    if (event.button !== 0) return
    event.currentTarget.focus()
    event.currentTarget.setPointerCapture(event.pointerId)
    dragRef.current = {
      startX: event.clientX, startY: event.clientY,
      cameraX: cameraRef.current.x, cameraY: cameraRef.current.y, moved: false,
    }
  }, [])
  const handlePointerMove = useCallback((event: React.PointerEvent<HTMLCanvasElement>) => {
    const drag = dragRef.current
    if (drag) {
      const dx = event.clientX - drag.startX
      const dy = event.clientY - drag.startY
      if (Math.abs(dx) > 3 || Math.abs(dy) > 3) drag.moved = true
      if (!drag.moved) return
      fittedRef.current = false
      const cam = cameraRef.current
      cam.x = cam.targetX = drag.cameraX + dx
      cam.y = cam.targetY = drag.cameraY + dy
      invalidateRef.current()
      return
    }
    const { wx, wy } = screenToWorld(event.clientX, event.clientY)
    const hit = findNodeAt(wx, wy)
    const nextId = hit?.id ?? null
    if (hoveredRef.current !== nextId) {
      hoveredRef.current = nextId
      event.currentTarget.style.cursor = hit ? 'pointer' : 'grab'
      event.currentTarget.title = hit ? (hit.kind === 'keyword'
        ? t('memory.brainGraph.wordTooltip', { word: hit.label, count: hit.memoryIds.length, frequency: hit.frequency })
        : hit.label) : ''
      invalidateRef.current()
    }
  }, [screenToWorld, findNodeAt, t])
  const handlePointerUp = useCallback((event: React.PointerEvent<HTMLCanvasElement>) => {
    const drag = dragRef.current
    dragRef.current = null
    if (event.currentTarget.hasPointerCapture(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId)
    if (!drag || drag.moved) return
    const { wx, wy } = screenToWorld(event.clientX, event.clientY)
    onSelectNode(findNodeAt(wx, wy)?.id ?? null)
  }, [screenToWorld, findNodeAt, onSelectNode])
  const clearHover = useCallback(() => {
    hoveredRef.current = null
    if (canvasRef.current) {
      canvasRef.current.style.cursor = 'grab'
      canvasRef.current.title = ''
    }
    invalidateRef.current()
  }, [])
  const handleKeyDown = useCallback((event: React.KeyboardEvent<HTMLCanvasElement>) => {
    if (event.key === 'Escape') {
      hoveredRef.current = null
      onSelectNode(null)
      invalidateRef.current()
      return
    }
    if (!['ArrowRight', 'ArrowDown', 'ArrowLeft', 'ArrowUp'].includes(event.key)) return
    event.preventDefault()
    const nodes = nodesRef.current
    if (!nodes.length) return
    const current = nodes.findIndex((node) => node.id === latestPropsRef.current.selectedNodeId)
    const direction = event.key === 'ArrowRight' || event.key === 'ArrowDown' ? 1 : -1
    const index = current < 0 ? 0 : (current + direction + nodes.length) % nodes.length
    const node = nodes[index]
    hoveredRef.current = null
    focusWorldPoint(node.x, node.y)
    onSelectNode(node.id)
  }, [onSelectNode, focusWorldPoint])

  return (
    <div ref={containerRef} className="relative h-full min-w-0 flex-1 overflow-hidden">
      <canvas
        ref={canvasRef}
        tabIndex={0}
        role="group"
        aria-label={t('memory.brainGraph.title')}
        aria-describedby="memory-graph-interaction-hint"
        onPointerDown={handlePointerDown}
        onPointerMove={handlePointerMove}
        onPointerUp={handlePointerUp}
        onPointerCancel={() => { dragRef.current = null; clearHover() }}
        onLostPointerCapture={() => { dragRef.current = null }}
        onPointerLeave={clearHover}
        onKeyDown={handleKeyDown}
        className="block h-full w-full touch-none cursor-grab select-none focus-visible:outline focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-blue-500 active:cursor-grabbing"
      />
      {props.loading && topology.keywordCount === 0 && (
        <div role="status" className="pointer-events-none absolute inset-0 flex items-center justify-center text-sm text-gray-600 dark:text-gray-300">{t('memory.brainGraph.keywordLoading')}</div>
      )}
      <div role="status" aria-live="polite" className="sr-only">{props.selectedNodeId ? topology.nodeById.get(props.selectedNodeId)?.label : ''}</div>
      {!props.loading && topology.keywordCount === 0 && (
        <div role="status" className="pointer-events-none absolute inset-0 flex items-center justify-center p-6">
          <div className="max-w-sm text-center">
            <Network size={28} className="mx-auto text-gray-500 dark:text-gray-300" />
            <p className="mt-2 text-sm font-semibold text-gray-900 dark:text-gray-100">
              {t(props.searchActive ? 'memory.brainGraph.noSearchResults' : 'memory.brainGraph.emptyTitle')}
            </p>
            {!props.searchActive && <p className="mt-1 text-xs text-gray-600 dark:text-gray-300">{t('memory.brainGraph.emptyHint')}</p>}
          </div>
        </div>
      )}
      <div className="pointer-events-none absolute bottom-3 left-3 right-3 flex flex-wrap items-center gap-x-4 gap-y-1 rounded-md border border-gray-200 bg-white px-3 py-2 text-[11px] text-gray-700 dark:border-gray-700 dark:bg-neutral-800 dark:text-gray-200">
        <span className="inline-flex items-center gap-1.5"><span className="h-2.5 w-2.5 rotate-45 bg-gray-500 dark:bg-gray-300" />{t('memory.brainGraph.keywordLegend')}</span>
        <span className="inline-flex items-center gap-1.5"><span className="h-2 w-2 rounded-full bg-gray-500 dark:bg-gray-300" />{t('memory.brainGraph.sampleMemoryLegend')}</span>
        <span>{t('memory.brainGraph.sizeLegend')}</span>
        <span className="inline-flex items-center gap-1.5"><span className="w-5 border-t border-gray-500" />{t('memory.brainGraph.bridgeLegend')}</span>
        <span id="memory-graph-interaction-hint" className="text-gray-600 dark:text-gray-300">{t('memory.brainGraph.interactionHint')}</span>
      </div>
    </div>
  )
})
