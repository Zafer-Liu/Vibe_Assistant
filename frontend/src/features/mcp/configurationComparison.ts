import type { McpCatalogEntry } from '../../types/mcp'

export interface McpComparisonRow {
  key: string
  left: string | undefined
  right: string | undefined
  changed: boolean
  sensitive: boolean
}

function connectionFields(entry: McpCatalogEntry | null) {
  const fields = new Map<string, string>()
  if (!entry) return fields
  const transport = entry.transport || 'stdio'
  fields.set('transport', transport)
  if (transport === 'stdio') {
    fields.set('command', entry.command)
    fields.set('args', JSON.stringify(entry.args, null, 2))
    for (const key of Object.keys(entry.env).sort()) fields.set(`env.${key}`, entry.env[key])
  } else {
    fields.set('url', entry.url)
    for (const key of Object.keys(entry.headers).sort()) fields.set(`headers.${key}`, entry.headers[key])
  }
  return fields
}

export function buildMcpComparisonRows(left: McpCatalogEntry | null, right: McpCatalogEntry | null): McpComparisonRow[] {
  const a = connectionFields(left)
  const b = connectionFields(right)
  return [...new Set([...a.keys(), ...b.keys()])].map(key => ({
    key, left: a.get(key), right: b.get(key), changed: a.get(key) !== b.get(key),
    sensitive: key.startsWith('env.') || key.startsWith('headers.') || key === 'url',
  }))
}
