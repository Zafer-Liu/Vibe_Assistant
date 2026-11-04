import { useEffect, useRef, useState } from 'react'
import type { MemoryItem } from '../../types/memory'
import type { KeywordCorpus } from './extractKeywords'

export function useKeywordCorpus(memories: MemoryItem[], active: boolean, revision: number) {
  const [corpus, setCorpus] = useState<KeywordCorpus>(() => ({ domains: new Map(), memoryById: new Map() }))
  const [loading, setLoading] = useState(false)
  const [failed, setFailed] = useState(false)
  const processedRef = useRef<{ memories: MemoryItem[]; revision: number } | null>(null)

  useEffect(() => {
    if (!active || (processedRef.current?.memories === memories && processedRef.current.revision === revision)) return
    let cancelled = false
    let worker: Worker | undefined
    setLoading(true)
    setFailed(false)
    const fail = () => {
      if (cancelled) return
      setLoading(false)
      setFailed(true)
      worker?.terminate()
    }
    try {
      worker = new Worker(new URL('./keywordWorker.ts', import.meta.url), { type: 'module' })
      worker.onmessage = (event: MessageEvent<KeywordCorpus>) => {
        if (cancelled) return
        processedRef.current = { memories, revision }
        setCorpus(event.data)
        setLoading(false)
        worker?.terminate()
      }
      worker.onerror = fail
      worker.onmessageerror = fail
      worker.postMessage(memories)
    } catch {
      fail()
    }
    return () => {
      cancelled = true
      worker?.terminate()
    }
  }, [memories, active, revision])

  return { corpus, loading, failed }
}
