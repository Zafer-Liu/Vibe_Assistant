import type { MemoryItem } from '../../types/memory'
import { UNCATEGORIZED_ID } from './types'
import { KEYWORD_STOP_WORDS } from './keywordStopWords'

export interface KeywordStat {
  key: string
  label: string
  frequency: number
  memoryIds: Set<string>
}

export interface DomainKeywordIndex {
  memoryIds: Set<string>
  keywords: Map<string, KeywordStat>
}

export interface KeywordCorpus {
  domains: Map<string, DomainKeywordIndex>
  memoryById: Map<string, MemoryItem>
}

const segmenter = typeof Intl.Segmenter === 'function'
  ? new Intl.Segmenter('zh-CN', { granularity: 'word' })
  : null

function cleanText(text: string): string {
  return text.normalize('NFKC')
    .replace(/```[\s\S]*?```/g, ' ')
    .replace(/https?:\/\/\S+/gi, ' ')
    .replace(/!\[[^\]]*\]\([^)]*\)/g, ' ')
    .replace(/\[([^\]]+)\]\([^)]*\)/g, '$1')
    .replace(/(?:[A-Za-z]:[\\/]|[./]?[\w.-]+[\\/])[\w./\\-]+/g, ' ')
}

function tokenize(text: string): Map<string, { label: string; count: number }> {
  const tokens = new Map<string, { label: string; count: number }>()
  const add = (raw: string) => {
    const label = raw.replace(/^[._-]+|[._-]+$/g, '')
    const key = label.toLowerCase()
    if (key.length < 2 || key.length > 24 || KEYWORD_STOP_WORDS.has(key) || !/\p{L}/u.test(key)) return
    if (/^[a-z]$/i.test(key) || /^\d/.test(key)) return
    const existing = tokens.get(key)
    if (existing) existing.count++
    else tokens.set(key, { label, count: 1 })
  }

  // Keep technical identifiers intact; segment CJK separately so adjacent English is not lost.
  for (const part of cleanText(text).matchAll(/[A-Za-z][A-Za-z0-9]*(?:[._+-][A-Za-z0-9]+)*(?:\+\+|#)?|[\p{Script=Han}\p{Script=Hiragana}\p{Script=Katakana}]+/gu)) {
    const value = part[0]
    if (/^[A-Za-z]/.test(value)) {
      add(value)
    } else if (segmenter) {
      for (const word of segmenter.segment(value)) {
        if (word.isWordLike) add(word.segment)
      }
    } else {
      // Old WebViews without ICU segmentation still get bounded CJK phrase candidates.
      for (let i = 0; i < value.length - 1; i += 2) add(value.slice(i, i + 2))
    }
  }
  return tokens
}

/** Tokenize once per memory update. Domain filtering and cloud layout reuse this index. */
export function extractKeywordCorpus(memories: MemoryItem[]): KeywordCorpus {
  const domains = new Map<string, DomainKeywordIndex>()
  const memoryById = new Map<string, MemoryItem>()
  for (const memory of memories) {
    if (memoryById.has(memory.id)) continue
    memoryById.set(memory.id, memory)
    const tokens = tokenize(memory.memory)
    const domainIds = new Set(memory.domains?.length
      ? memory.domains.map((domain) => domain.id)
      : [UNCATEGORIZED_ID])
    for (const domainId of domainIds) {
      let domain = domains.get(domainId)
      if (!domain) {
        domain = { memoryIds: new Set(), keywords: new Map() }
        domains.set(domainId, domain)
      }
      domain.memoryIds.add(memory.id)
      for (const [key, token] of tokens) {
        const stat = domain.keywords.get(key)
        if (stat) {
          stat.frequency += token.count
          stat.memoryIds.add(memory.id)
        } else {
          domain.keywords.set(key, { key, label: token.label, frequency: token.count, memoryIds: new Set([memory.id]) })
        }
      }
    }
  }
  return { domains, memoryById }
}
