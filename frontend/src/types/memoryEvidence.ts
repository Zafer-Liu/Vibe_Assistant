export interface EvidenceAccess {
  request_id: string
  status: string
  context_token: string | null
  expires_at: string | null
  max_chars: number
}

export interface EvidenceRequest {
  context_token: string
  memory_ids: string[]
  query?: string | null
  max_chars?: number | null
  cursor?: string | null
}

export interface EvidenceExcerpt {
  text: string
  char_start: number
  char_end: number
  total_chars: number
  match_method: string
  excerpt_fingerprint: string
}

export interface EvidenceSource {
  source_event_key: string
  source_agent: string
  session_id: string
  occurred_at: string | null
  source_fingerprint: string | null
  status: string
  excerpt: EvidenceExcerpt | null
}

export interface EvidenceItem {
  memory_id: string
  status: string
  sources: EvidenceSource[]
}

export interface EvidenceResponse {
  request_id: string
  context_request_id: string | null
  status: string
  expires_at: string | null
  returned_chars: number
  remaining_chars: number
  partial: boolean
  items: EvidenceItem[]
  next_cursor: string | null
  instruction: string
}
