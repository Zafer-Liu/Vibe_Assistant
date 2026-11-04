import type { MemoryItem } from '../../types/memory'
import { extractKeywordCorpus } from './extractKeywords'

addEventListener('message', (event: MessageEvent<MemoryItem[]>) => {
  postMessage(extractKeywordCorpus(event.data))
})
