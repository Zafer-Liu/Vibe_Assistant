/** One Hook-initiated memory injection or skip; `detail` holds the written payload. */
export interface MemoryInjectionLog {
  id: number
  occurred_at: string
  client_name: string
  tool_name: string
  summary: string
  detail?: string | null
  success: boolean
}

/** Per-agent injection-cost rollup over the audit window. `prompt_skips` counts
 *  turns whose rendered context fingerprint was unchanged and were therefore
 *  not re-injected — the hit rate of the per-turn injection gate. */
export interface MemoryInjectionStats {
  client_name: string
  session_injections: number
  prompt_injections: number
  prompt_skips: number
  injected_chars: number
}
