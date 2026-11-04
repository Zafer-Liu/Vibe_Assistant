import type { MemoryDomainItem } from '../../types/memory'

export interface LobePalette {
  id: string
  primary: string
  glow: string
  ring: string
  badgeBg: string
  badgeText: string
}

export const LOBE_PALETTES: readonly LobePalette[] = [
  { id: 'cyan', primary: '#22d3ee', glow: 'rgba(34, 211, 238, 0.42)', ring: 'rgba(103, 232, 249, 0.88)', badgeBg: 'bg-cyan-500/15', badgeText: 'text-cyan-300' },
  { id: 'purple', primary: '#a855f7', glow: 'rgba(168, 85, 247, 0.42)', ring: 'rgba(216, 180, 254, 0.88)', badgeBg: 'bg-purple-500/15', badgeText: 'text-purple-300' },
  { id: 'emerald', primary: '#34d399', glow: 'rgba(52, 211, 153, 0.42)', ring: 'rgba(110, 231, 183, 0.88)', badgeBg: 'bg-emerald-500/15', badgeText: 'text-emerald-300' },
  { id: 'amber', primary: '#fbbf24', glow: 'rgba(251, 191, 36, 0.42)', ring: 'rgba(253, 224, 71, 0.88)', badgeBg: 'bg-amber-500/15', badgeText: 'text-amber-300' },
  { id: 'rose', primary: '#fb7185', glow: 'rgba(251, 113, 133, 0.42)', ring: 'rgba(253, 164, 175, 0.88)', badgeBg: 'bg-rose-500/15', badgeText: 'text-rose-300' },
  { id: 'sky', primary: '#38bdf8', glow: 'rgba(56, 189, 248, 0.42)', ring: 'rgba(125, 211, 252, 0.88)', badgeBg: 'bg-sky-500/15', badgeText: 'text-sky-300' },
  { id: 'indigo', primary: '#818cf8', glow: 'rgba(129, 140, 248, 0.42)', ring: 'rgba(165, 180, 252, 0.88)', badgeBg: 'bg-indigo-500/15', badgeText: 'text-indigo-300' },
  { id: 'pink', primary: '#f472b6', glow: 'rgba(244, 114, 182, 0.42)', ring: 'rgba(249, 168, 212, 0.88)', badgeBg: 'bg-pink-500/15', badgeText: 'text-pink-300' },
]

export const UNCATEGORIZED_PALETTE: LobePalette = {
  id: 'slate',
  primary: '#94a3b8',
  glow: 'rgba(148, 163, 184, 0.34)',
  ring: 'rgba(203, 213, 225, 0.78)',
  badgeBg: 'bg-slate-500/15',
  badgeText: 'text-slate-300',
}

export const CORE_PALETTE: LobePalette = {
  id: 'core',
  primary: '#c084fc',
  glow: 'rgba(192, 132, 252, 0.55)',
  ring: 'rgba(243, 232, 255, 0.95)',
  badgeBg: 'bg-violet-500/20',
  badgeText: 'text-violet-200',
}

export const UNCATEGORIZED_ID = '__uncategorized__'

export interface EffectiveLobe {
  item: MemoryDomainItem
  palette: LobePalette
  slotIndex: number
}

export interface GraphNode {
  id: string
  kind: 'lobe' | 'keyword' | 'memory'
  label: string
  keyword?: string
  x: number
  y: number
  targetX: number
  targetY: number
  width: number
  height: number
  fontSize: number
  fontWeight: number
  radius?: number
  labelWidth?: number
  showLabel?: boolean
  domainId: string
  domainItem: MemoryDomainItem
  memoryIds: string[]
  frequency: number
  searchTextLower: string
}

export interface KeywordEdge {
  id: string
  sourceId: string
  targetId: string
  kind: 'cooccurrence' | 'shared-keyword' | 'keyword-memory' | 'shared-memory'
  weight: number
}

export interface GraphCloudRegion {
  domainId: string
  x: number
  y: number
  width: number
  height: number
}

export interface GraphTopology {
  regions: GraphCloudRegion[]
  nodes: GraphNode[]
  edges: KeywordEdge[]
  nodeById: Map<string, GraphNode>
  keywordCount: number
  cloudCount: number
  totalCloudCount: number
  representedMemoryCount: number
  sampledMemoryCount: number
}

export interface GraphViewportController {
  zoomIn: () => void
  zoomOut: () => void
  resetCamera: () => void
  focusWorldPoint: (wx: number, wy: number, scale?: number) => void
  focusNodeById: (nodeId: string, scale?: number) => void
}
