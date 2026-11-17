import { invoke } from '@tauri-apps/api/core'
import type { ConsolidationCandidate, MemoryItem } from '../types/memory'

type Extractor = (inputs: string | string[], options?: Record<string, unknown>) => Promise<unknown>

const ZH_MODEL = 'Xenova/bge-small-zh-v1.5'
const EN_MODEL = 'Xenova/bge-small-en-v1.5'

const extractors = new Map<string, Promise<Extractor>>()

function hasChinese(text: string) {
  return /[一-鿿]/.test(text)
}

async function extractorFor(query: string): Promise<Extractor> {
  const model = hasChinese(query) ? ZH_MODEL : EN_MODEL
  let pending = extractors.get(model)
  if (!pending) {
    pending = import('@huggingface/transformers').then(async ({ pipeline }) => {
      const extractor = await pipeline('feature-extraction', model, { dtype: 'q8' })
      return extractor as unknown as Extractor
    })
    extractors.set(model, pending)
  }
  return pending
}

function toVectors(output: unknown): Float32Array[] {
  const tensor = output as { data?: Float32Array | number[], dims?: number[] }
  if (!tensor?.data || !tensor.dims?.length) throw new Error('BGE-small 未返回有效向量')
  const values = tensor.data instanceof Float32Array ? tensor.data : Float32Array.from(tensor.data)
  const size = tensor.dims.at(-1)
  if (!size || values.length % size !== 0) throw new Error('BGE-small 向量维度异常')
  return Array.from({ length: values.length / size }, (_, index) => values.slice(index * size, (index + 1) * size))
}

function cosine(left: Float32Array, right: Float32Array) {
  let dot = 0
  let leftNorm = 0
  let rightNorm = 0
  if (left.length !== right.length) throw new Error('BGE 向量维度不一致')
  const size = left.length
  for (let index = 0; index < size; index += 1) {
    dot += left[index] * right[index]
    leftNorm += left[index] * left[index]
    rightNorm += right[index] * right[index]
  }
  return leftNorm && rightNorm ? dot / Math.sqrt(leftNorm * rightNorm) : 0
}

// Local BGE inference runs on the WebView main thread. Yield between chunks
// so a large memory library never freezes the interface while embedding.
function yieldToUi(): Promise<void> {
  return new Promise((resolve) => { setTimeout(resolve, 0) })
}

// Embeddings are computed in small batches; each batch keeps the UI
// responsive at the cost of a few more extractor calls.
const EMBED_CHUNK_SIZE = 16

type RemoteEmbedResult = { vectors: number[][], dimension: number, model: string, embedding_space: string, latency_ms: number }

// Tracks the last known availability of the native embedding command. When
// the backend reports the command as missing or the configured service as
// unreachable, we stop retrying for this session and stay on the local model.
let remoteEmbeddingDisabled = false

/**
 * Embed a batch of texts, preferring the native `memory_semantic_embed_texts`
 * command (which honours the configured local/remote embedding service) and
 * falling back to the in-WebView BGE-small models when the command is absent
 * or fails. The fallback keeps older configurations usable.
 */
type EmbeddingBatch = { vectors: Float32Array[], dimension: number, model: string }

async function embedTexts(texts: string[], fallbackModel?: string): Promise<EmbeddingBatch> {
  if (texts.length === 0) return { vectors: [], dimension: 0, model: fallbackModel ?? '' }
  if (!fallbackModel && !remoteEmbeddingDisabled) {
    try {
      const result = await invoke<RemoteEmbedResult>('memory_semantic_embed_texts', { texts })
      if (!result.model || !result.embedding_space || !Number.isInteger(result.dimension) || result.dimension <= 0
        || !Array.isArray(result.vectors) || result.vectors.length !== texts.length
        || result.vectors.some((row) => !Array.isArray(row) || row.length !== result.dimension || row.some((value) => !Number.isFinite(value)))) {
        throw new Error('原生嵌入返回无效模型或维度')
      }
      return { vectors: result.vectors.map((row) => Float32Array.from(row)), dimension: result.dimension, model: `native:${result.embedding_space}:${result.dimension}` }
    } catch {
      remoteEmbeddingDisabled = true
    }
  }
  const model = fallbackModel ?? (hasChinese(texts[0]) ? ZH_MODEL : EN_MODEL)
  const extractor = await extractorFor(model === ZH_MODEL ? '中' : 'en')
  const vectors = toVectors(await extractor(texts, { pooling: 'mean', normalize: true }))
  const dimension = vectors[0]?.length ?? 0
  if (!dimension || vectors.length !== texts.length || vectors.some((vector) => vector.length !== dimension)) {
    throw new Error('BGE-small 向量数量或维度异常')
  }
  return { vectors, dimension, model }
}

/**
 * Select likely duplicate groups, preferring the configured embedding service
 * via `memory_semantic_embed_texts` and falling back to BGE-small in the
 * WebView when unavailable. The output intentionally contains only small
 * batches; only these candidates are later sent to the configured
 * consolidation LLM for a safety decision. Embedding is chunked and yields to
 * the UI thread, and `onProgress` reports how many memories have been
 * embedded so the caller can show live progress.
 */
export async function createBgeConsolidationCandidateBatches(
  memories: MemoryItem[],
  onProgress?: (processed: number, total: number) => void,
): Promise<ConsolidationCandidate[][]> {
  const local = memories.filter((memory) => memory.id.startsWith('local-l1:') && memory.memory.trim())
  if (local.length < 2) return []

  const byLanguage = new Map<boolean, MemoryItem[]>()
  for (const memory of local) {
    const key = hasChinese(memory.memory)
    byLanguage.set(key, [...(byLanguage.get(key) ?? []), memory])
  }

  const total = local.length
  let processed = 0
  const pairs: Array<{ left: MemoryItem, right: MemoryItem, score: number }> = []
  for (const [, group] of byLanguage) {
    if (group.length < 2) {
      processed += group.length
      continue
    }
    // A native failure or a model change restarts the entire language group
    // with one fixed fallback model, including chunks already embedded.
    const embedded: Float32Array[] = []
    const groupStart = processed
    let identity: Pick<EmbeddingBatch, 'model' | 'dimension'> | undefined
    let fallbackModel: string | undefined
    for (let start = 0; start < group.length;) {
      const slice = group.slice(start, start + EMBED_CHUNK_SIZE)
      const batch = await embedTexts(slice.map((memory) => memory.memory), fallbackModel)
      if (identity && (identity.model !== batch.model || identity.dimension !== batch.dimension)) {
        if (fallbackModel) throw new Error('BGE fallback 模型或维度在组内发生变化')
        fallbackModel = hasChinese(group[0].memory) ? ZH_MODEL : EN_MODEL
        embedded.length = 0
        identity = undefined
        start = 0
        await yieldToUi()
        continue
      }
      identity = batch
      // Keep every later fallback batch pinned to the first fallback model.
      if (!batch.model.startsWith('native:')) fallbackModel = batch.model
      embedded.push(...batch.vectors)
      start += slice.length
      onProgress?.(Math.min(groupStart + start, total), total)
      if (start < group.length) await yieldToUi()
    }
    processed += group.length
    const neighbours = group.map(() => [] as Array<{ index: number, score: number }>)
    let pairChecks = 0
    for (let left = 0; left < group.length; left += 1) {
      for (let right = left + 1; right < group.length; right += 1) {
        const score = cosine(embedded[left], embedded[right])
        // BGE-small has relatively high scores for same-domain Agent logs;
        // require a strong semantic signal and retain only each item's top 3.
        if (score >= 0.78) {
          neighbours[left].push({ index: right, score })
          neighbours[right].push({ index: left, score })
        }
        // The O(n^2) sweep is cheap per step but can add up on large groups;
        // yield occasionally so it never monopolises the main thread.
        pairChecks += 1
        if (pairChecks % 4096 === 0) await yieldToUi()
      }
    }
    const chosen = new Set<string>()
    neighbours.forEach((items, index) => items.sort((a, b) => b.score - a.score).slice(0, 3).forEach(({ index: other }) => {
      const [first, second] = index < other ? [index, other] : [other, index]
      chosen.add(`${first}:${second}`)
    }))
    chosen.forEach((key) => {
      const [left, right] = key.split(':').map(Number)
      pairs.push({ left: group[left], right: group[right], score: cosine(embedded[left], embedded[right]) })
    })
  }

  // At most 3 pairs per call keeps the initial request short enough for
  // reasoning models with a small visible-output budget. The native command
  // further falls back to individual pairs when a provider returns `length`.
  const batches: ConsolidationCandidate[][] = []
  const strongestPairs = pairs.sort((left, right) => right.score - left.score).slice(0, 12)
  for (let start = 0; start < strongestPairs.length; start += 3) {
    const unique = new Map<string, ConsolidationCandidate>()
    for (const pair of strongestPairs.slice(start, start + 3)) {
      for (const memory of [pair.left, pair.right]) {
        unique.set(memory.id, { id: memory.id, memory: memory.memory, memory_type: memory.memory_type })
      }
    }
    if (unique.size >= 2) batches.push([...unique.values()])
  }
  return batches
}
