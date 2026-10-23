import type { McpAccessLog, McpAgentStatus, McpCatalogEntry, McpProfile } from '../types/mcp'

export type McpStatusState = 'idle' | 'loading' | 'ready' | 'error'

export interface McpConnectionSummary {
  services: number
  assignments: number
  profiles: number
  installed: number | null
  missing: number | null
  differs: number | null
  unavailable: number | null
}

export interface McpProfileResolution {
  profile: McpProfile
  entries: McpCatalogEntry[]
  missing: string[]
}

export interface McpActivityRow {
  id: number
  occurredAt: string
  clientName: string
  toolName: string
  summary: string
  detail: string | null
  success: boolean
}

export function buildMcpConnectionSummary(
  catalog: McpCatalogEntry[],
  statuses: McpAgentStatus[],
  profiles: McpProfile[],
  statusState: McpStatusState,
): McpConnectionSummary {
  // Configuration counts cover all inspected sources; deployment counts below
  // remain limited to explicit assignments, including when a source is missing.
  const inspected = statuses.filter(status => catalog.some(entry => entry.name === status.name))
  return {
    services: catalog.length,
    assignments: catalog.reduce((total, entry) => total + entry.assigned_agents.length, 0),
    profiles: profiles.length,
    installed: statusState === 'ready' ? inspected.filter(status => status.state === 'installed').length : null,
    missing: statusState === 'ready' ? inspected.filter(status => status.state === 'missing').length : null,
    differs: statusState === 'ready' ? inspected.filter(status => status.state === 'differs').length : null,
    unavailable: statusState === 'ready' ? inspected.filter(status => status.state === 'unavailable').length : null,
  }
}

export function resolveMcpProfile(profile: McpProfile, catalog: McpCatalogEntry[]): McpProfileResolution {
  const byName = new Map(catalog.map(entry => [entry.name, entry]))
  const entries: McpCatalogEntry[] = []
  const missing: string[] = []
  for (const name of profile.servers) {
    const entry = byName.get(name)
    if (entry) entries.push(entry)
    else missing.push(name)
  }
  return { profile, entries, missing }
}

export function normalizeMcpActivity(log: McpAccessLog): McpActivityRow {
  return {
    id: log.id,
    occurredAt: log.occurred_at,
    clientName: log.client_name,
    toolName: log.tool_name,
    summary: log.summary,
    detail: log.detail ?? null,
    success: log.success,
  }
}
