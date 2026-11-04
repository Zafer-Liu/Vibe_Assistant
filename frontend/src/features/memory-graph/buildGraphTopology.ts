import type { KeywordCorpus, KeywordStat } from './extractKeywords'
import type { EffectiveLobe, GraphCloudRegion, GraphNode, GraphTopology, KeywordEdge } from './types'

const MAX_CLOUDS = 6
const WORDS_PER_CLOUD = 10
const FOCUSED_WORDS = 18
const MEMORIES_PER_CLOUD = 32
const FOCUSED_MEMORIES = 72
const MAX_KEYWORD_EDGES = 120
const MAX_EDGES = 480
const GOLDEN_ANGLE = 2.399963229728653
const FONT_FAMILY = '"Segoe UI", "Microsoft YaHei UI", sans-serif'

interface Cluster {
  domain: EffectiveLobe
  words: KeywordStat[]
  keywords: GraphNode[]
  memories: GraphNode[]
  edges: KeywordEdge[]
  x: number
  y: number
  width: number
  height: number
}

function compareWords(a: KeywordStat, b: KeywordStat): number {
  return b.memoryIds.size - a.memoryIds.size || b.frequency - a.frequency || a.key.localeCompare(b.key)
}

function measureLabel(label: string, size: number, weight: number, ctx: CanvasRenderingContext2D | null): number {
  if (ctx) {
    ctx.font = `${weight} ${size}px ${FONT_FAMILY}`
    return Math.ceil(ctx.measureText(label).width) + 12
  }
  return Array.from(label).reduce((sum, letter) => sum + (letter.charCodeAt(0) > 0xff ? size : size * 0.7), 12)
}

function memoryPreview(text: string): string {
  const plain = text.replace(/[#*`>~]/g, '').replace(/\s+/g, ' ').trim()
  return plain.length > 44 ? `${plain.slice(0, 44)}…` : plain
}

function seedAngle(id: string): number {
  let hash = 0
  for (const character of id) hash = Math.imul(hash, 31) + character.codePointAt(0)!
  return ((hash >>> 0) % 6283) / 1000
}

function buildCluster(cluster: Cluster, corpus: KeywordCorpus, maxCount: number, focused: boolean, ctx: CanvasRenderingContext2D | null) {
  const domainItem = cluster.domain.item
  cluster.keywords = cluster.words.map((word, index) => {
    const ratio = maxCount === 1 ? 0.3 : Math.log(word.memoryIds.size) / Math.log(maxCount)
    const radius = 8 + ratio * 8
    const angle = index * GOLDEN_ANGLE
    const distance = Math.sqrt(index) * 72
    return {
      id: `keyword:${JSON.stringify([domainItem.id, word.key])}`,
      kind: 'keyword', label: word.label, keyword: word.key,
      x: Math.cos(angle) * distance * 1.08,
      y: Math.sin(angle) * distance * 0.85,
      targetX: 0, targetY: 0,
      width: radius * 2, height: radius * 2, radius,
      fontSize: 13, fontWeight: 600,
      labelWidth: measureLabel(word.label, 13, 600, ctx),
      showLabel: index < 2,
      domainId: domainItem.id, domainItem,
      memoryIds: [...word.memoryIds], frequency: word.frequency,
      searchTextLower: `${word.key} ${domainItem.name}`.toLowerCase(),
    }
  })

  // Round-robin source sampling gives each keyword a small, genuine neighborhood.
  const limit = focused ? FOCUSED_MEMORIES : MEMORIES_PER_CLOUD
  const chosen = new Set<string>()
  const cursors = cluster.keywords.map(() => 0)
  const localCounts = cluster.keywords.map(() => 0)
  let added = true
  while (chosen.size < limit && added) {
    added = false
    cluster.keywords.forEach((parent, index) => {
      if (chosen.size >= limit) return
      let cursor = cursors[index]
      while (cursor < parent.memoryIds.length && chosen.has(parent.memoryIds[cursor])) cursor++
      cursors[index] = cursor + 1
      const memoryId = parent.memoryIds[cursor]
      if (!memoryId) return
      const memory = corpus.memoryById.get(memoryId)
      if (!memory) return
      added = true
      chosen.add(memoryId)
      const ordinal = localCounts[index]++
      const angle = seedAngle(parent.id) + ordinal * GOLDEN_ANGLE
      const distance = 32 + Math.sqrt(ordinal) * 12
      const node: GraphNode = {
        id: `memory:${JSON.stringify([domainItem.id, memoryId])}`,
        kind: 'memory', label: memoryPreview(memory.memory),
        x: parent.x + Math.cos(angle) * distance,
        y: parent.y + Math.sin(angle) * distance,
        targetX: 0, targetY: 0,
        width: 9, height: 9, radius: 4.5,
        fontSize: 11, fontWeight: 400,
        labelWidth: measureLabel(memoryPreview(memory.memory), 11, 400, ctx),
        domainId: domainItem.id, domainItem,
        memoryIds: [memoryId], frequency: 0,
        searchTextLower: memory.memory.toLowerCase(),
      }
      cluster.memories.push(node)
      cluster.edges.push({ id: `source:${node.id}`, sourceId: parent.id, targetId: node.id, kind: 'keyword-memory', weight: 1 })
      const secondary = cluster.keywords.find((candidate, candidateIndex) =>
        candidate.id !== parent.id && cluster.words[candidateIndex].memoryIds.has(memoryId))
      if (secondary) {
        cluster.edges.push({ id: `source-secondary:${node.id}`, sourceId: secondary.id, targetId: node.id, kind: 'keyword-memory', weight: 1 })
      }
    })
  }

  const members = [...cluster.keywords, ...cluster.memories]
  // A small, bounded settling pass separates actual node circles, once per topology update.
  for (let pass = 0; pass < 18; pass++) {
    let moved = false
    for (let i = 0; i < members.length; i++) {
      for (let j = i + 1; j < members.length; j++) {
        const a = members[i]
        const b = members[j]
        const dx = b.x - a.x
        const dy = b.y - a.y
        const gap = (a.radius ?? 0) + (b.radius ?? 0) + 5
        if (Math.abs(dx) >= gap || Math.abs(dy) >= gap) continue
        const distance = Math.hypot(dx, dy)
        if (distance >= gap) continue
        const push = (gap - distance) / 2 + 0.2
        const nx = distance > 0.001 ? dx / distance : 1
        const ny = distance > 0.001 ? dy / distance : 0
        a.x -= nx * push
        a.y -= ny * push
        b.x += nx * push
        b.y += ny * push
        moved = true
      }
    }
    if (!moved) break
  }
  let rx = 100
  let ry = 80
  for (const node of members) {
    rx = Math.max(rx, Math.abs(node.x) + (node.radius ?? 0) + 18)
    ry = Math.max(ry, Math.abs(node.y) + (node.radius ?? 0) + 18)
  }
  let expansion = 1
  for (const node of members) {
    expansion = Math.max(expansion, Math.hypot(
      (Math.abs(node.x) + (node.radius ?? 0) + 10) / rx,
      (Math.abs(node.y) + (node.radius ?? 0) + 10) / ry,
    ))
  }
  cluster.width = Math.max(rx * expansion * 2, measureLabel(domainItem.name, 20, 600, ctx) + 24)
  cluster.height = ry * expansion * 2
}

/** Build bounded communities of keyword hubs and sampled source memories. */
export function buildGraphTopology(
  corpus: KeywordCorpus,
  domains: EffectiveLobe[],
  activeDomainId: string | null,
  searchQuery: string,
): GraphTopology {
  const query = searchQuery.trim().toLowerCase().normalize('NFKC')
  const ctx = typeof document === 'undefined' ? null : document.createElement('canvas').getContext('2d')
  const candidates = domains.flatMap((domain) => {
    const index = corpus.domains.get(domain.item.id)
    if (!index?.keywords.size || (activeDomainId && domain.item.id !== activeDomainId)) return []
    const domainMatches = `${domain.item.name} ${domain.item.description}`.toLowerCase().normalize('NFKC').includes(query)
    const words = [...index.keywords.values()]
      .filter((word) => !query || domainMatches || word.key.includes(query))
      .sort(compareWords)
    if (!words.length) return []
    return [{ domain, words, keywords: [], memories: [], edges: [], x: 0, y: 0, width: 0, height: 0 } as Cluster]
  }).sort((a, b) => {
    const countA = corpus.domains.get(a.domain.item.id)!.memoryIds.size
    const countB = corpus.domains.get(b.domain.item.id)!.memoryIds.size
    return countB - countA || a.domain.item.id.localeCompare(b.domain.item.id)
  })
  const clusters = candidates.slice(0, MAX_CLOUDS)
  const wordLimit = activeDomainId ? FOCUSED_WORDS : WORDS_PER_CLOUD
  let maxCount = 1
  for (const cluster of clusters) {
    cluster.words = cluster.words.slice(0, wordLimit)
    for (const word of cluster.words) maxCount = Math.max(maxCount, word.memoryIds.size)
  }
  clusters.forEach((cluster, index) => {
    buildCluster(cluster, corpus, maxCount, Boolean(activeDomainId), ctx)
    const angle = index * GOLDEN_ANGLE
    const distance = Math.sqrt(index) * 320
    cluster.x = Math.cos(angle) * distance * 1.25
    cluster.y = Math.sin(angle) * distance * 0.8
  })
  // Pack community bounds while leaving room for external topic labels.
  for (let pass = 0; pass < 48; pass++) {
    let moved = false
    for (let i = 0; i < clusters.length; i++) {
      for (let j = i + 1; j < clusters.length; j++) {
        const a = clusters[i]
        const b = clusters[j]
        const dx = b.x - a.x
        const dy = b.y - a.y
        const overlapX = (a.width + b.width) / 2 + 56 - Math.abs(dx)
        const overlapY = (a.height + b.height) / 2 + 100 - Math.abs(dy)
        if (overlapX <= 0 || overlapY <= 0) continue
        moved = true
        if (overlapX < overlapY * 1.6) {
          const push = (overlapX / 2 + 1) * (dx >= 0 ? 1 : -1)
          a.x -= push
          b.x += push
        } else {
          const push = (overlapY / 2 + 1) * (dy >= 0 ? 1 : -1)
          a.y -= push
          b.y += push
        }
      }
    }
    if (!moved) break
  }

  const nodes: GraphNode[] = []
  const regions: GraphCloudRegion[] = []
  const edges: KeywordEdge[] = []
  const representedMemories = new Set<string>()
  const sampledMemories = new Set<string>()
  for (const cluster of clusters) {
    for (const node of [...cluster.memories, ...cluster.keywords]) {
      node.x = node.targetX = node.x + cluster.x
      node.y = node.targetY = node.y + cluster.y
      nodes.push(node)
      for (const id of node.memoryIds) representedMemories.add(id)
      if (node.kind === 'memory') sampledMemories.add(node.memoryIds[0])
    }
    regions.push({ domainId: cluster.domain.item.id, x: cluster.x, y: cluster.y, width: cluster.width, height: cluster.height })
    const item = cluster.domain.item
    const heading: GraphNode = {
      id: `lobe:${item.id}`, kind: 'lobe', label: item.name,
      x: cluster.x, y: cluster.y - cluster.height / 2 - 26,
      targetX: cluster.x, targetY: cluster.y - cluster.height / 2 - 26,
      width: measureLabel(item.name, 20, 600, ctx), height: 30,
      fontSize: 20, fontWeight: 600, showLabel: true,
      domainId: item.id, domainItem: item,
      memoryIds: [...corpus.domains.get(item.id)!.memoryIds], frequency: 0,
      searchTextLower: `${item.name} ${item.description}`.toLowerCase(),
    }
    nodes.push(heading)
    edges.push(...cluster.edges)
  }

  const keywordCandidates: KeywordEdge[] = []
  for (const cluster of clusters) {
    const nodesByMemory = new Map<string, number[]>()
    cluster.keywords.forEach((node, index) => {
      for (const id of node.memoryIds) {
        const list = nodesByMemory.get(id)
        if (list) list.push(index)
        else nodesByMemory.set(id, [index])
      }
    })
    const size = cluster.keywords.length
    const pairCounts = new Uint32Array(size * size)
    for (const list of nodesByMemory.values()) {
      for (let i = 0; i < list.length; i++) {
        for (let j = i + 1; j < list.length; j++) pairCounts[list[i] * size + list[j]]++
      }
    }
    for (let i = 0; i < size; i++) {
      for (let j = i + 1; j < size; j++) {
        const count = pairCounts[i * size + j]
        if (!count) continue
        const source = cluster.keywords[i]
        const target = cluster.keywords[j]
        keywordCandidates.push({ id: JSON.stringify([source.id, target.id]), sourceId: source.id, targetId: target.id, kind: 'cooccurrence', weight: count })
      }
    }
  }
  const byKeyword = new Map<string, GraphNode[]>()
  const byMemory = new Map<string, GraphNode[]>()
  for (const node of nodes) {
    const key = node.kind === 'keyword' ? node.keyword : node.kind === 'memory' ? node.memoryIds[0] : undefined
    if (!key) continue
    const map = node.kind === 'keyword' ? byKeyword : byMemory
    const group = map.get(key)
    if (group) group.push(node)
    else map.set(key, [node])
  }
  for (const groups of [byKeyword, byMemory]) {
    for (const group of groups.values()) {
      // A chain connects copies across domains without creating every possible pair.
      for (let i = 1; i < group.length; i++) {
        const source = group[i - 1]
        const target = group[i]
        keywordCandidates.push({
          id: `bridge:${JSON.stringify([source.id, target.id])}`,
          sourceId: source.id, targetId: target.id,
          kind: source.kind === 'keyword' ? 'shared-keyword' : 'shared-memory',
          weight: source.kind === 'keyword' ? Math.min(source.memoryIds.length, target.memoryIds.length) : 1,
        })
      }
    }
  }
  keywordCandidates.sort((a, b) =>
    Number(b.kind !== 'cooccurrence') - Number(a.kind !== 'cooccurrence') || b.weight - a.weight || a.id.localeCompare(b.id))
  const degree = new Map<string, number>()
  let addedEdges = 0
  for (const edge of keywordCandidates) {
    if (edges.length >= MAX_EDGES || addedEdges >= MAX_KEYWORD_EDGES) break
    const sourceDegree = degree.get(edge.sourceId) ?? 0
    const targetDegree = degree.get(edge.targetId) ?? 0
    if (sourceDegree >= 4 || targetDegree >= 4) continue
    edges.push(edge)
    addedEdges++
    degree.set(edge.sourceId, sourceDegree + 1)
    degree.set(edge.targetId, targetDegree + 1)
  }
  const kindOrder = { memory: 0, keyword: 1, lobe: 2 }
  nodes.sort((a, b) => kindOrder[a.kind] - kindOrder[b.kind])
  return {
    regions, nodes, edges, nodeById: new Map(nodes.map((node) => [node.id, node])),
    keywordCount: nodes.filter((node) => node.kind === 'keyword').length,
    cloudCount: clusters.length, totalCloudCount: candidates.length,
    representedMemoryCount: representedMemories.size,
    sampledMemoryCount: sampledMemories.size,
  }
}
