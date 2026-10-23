import type { McpScanResult } from '../../types/mcp'

import type { McpCatalogEntry } from '../../types/mcp'

/** 表单态：JSON 文本保留空参数、换行及键值中的空白。 */
export interface EntryForm {
  name: string
  description: string
  transport: string
  command: string
  argsText: string
  envText: string
  url: string
  headersText: string
}

export const EMPTY_FORM: EntryForm = {
  name: '', description: '', transport: 'stdio', command: '',
  argsText: '[]', envText: '{}', url: '', headersText: '{}',
}

export class InvalidMcpConfig extends Error {}

type CanonicalTransport = 'stdio' | 'sse' | 'http'

const REMOTE_HTTP_TYPES = new Set([
  'http', 'https', 'remote', 'remote-http', 'streamable-http', 'streamablehttp',
])

function isConfigObject(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
}

function parseConfigJson(text: string, field: string): unknown {
  try {
    return JSON.parse(text)
  } catch {
    throw new InvalidMcpConfig(`${field}: invalid JSON`)
  }
}

function parseArgs(text: string): string[] {
  const value = parseConfigJson(text, 'args')
  if (!Array.isArray(value) || !value.every((arg) => typeof arg === 'string')) {
    throw new InvalidMcpConfig('args: expected a JSON array of strings')
  }
  return value
}

function parseStringMap(text: string, field: 'env' | 'headers'): Record<string, string> {
  const value = parseConfigJson(text, field)
  if (!isConfigObject(value) || !Object.values(value).every((item) => typeof item === 'string')) {
    throw new InvalidMcpConfig(`${field}: expected a JSON object with string values`)
  }
  return value as Record<string, string>
}

function normalizeTransport(value: string, field: string): CanonicalTransport | '' {
  const normalized = value.trim().toLowerCase().replace(/[\s_]+/g, '-')
  if (!normalized) return ''
  if (normalized === 'stdio' || normalized === 'sse') return normalized
  if (REMOTE_HTTP_TYPES.has(normalized)) return 'http'
  throw new InvalidMcpConfig(`${field}: expected stdio, sse, http or streamable-http`)
}

function remoteUrlFromArgs(command: string, args: string[], requireProxy = true): string {
  const tokens = [command, ...args].map((token) => token.toLowerCase())
  const usesRemoteProxy = tokens.some((token) =>
    /(?:^|[/\\])(?:mcp-remote|mcp-proxy|supergateway)(?:@[^/\\]+)?$/.test(token),
  )
  if (requireProxy && !usesRemoteProxy) return ''
  return args.find((arg) => /^https?:\/\//i.test(arg)) ?? ''
}

export function remoteUrlFromText(text: string): string {
  return text.match(/https?:\/\/[^\s"'`,}\]]+/i)?.[0] ?? ''
}

export function remoteUrlFromForm(form: EntryForm): string {
  try {
    const args = parseArgs(form.argsText)
    return remoteUrlFromArgs(form.command, args, false)
  } catch {
    return ''
  }
}

export function formFromEntry(entry: McpCatalogEntry): EntryForm {
  return {
    name: entry.name,
    description: entry.description,
    transport: entry.transport || 'stdio',
    command: entry.command,
    argsText: JSON.stringify(entry.args, null, 2),
    envText: JSON.stringify(entry.env, null, 2),
    url: entry.url,
    headersText: JSON.stringify(entry.headers, null, 2),
  }
}

export function entryFromForm(form: EntryForm, existing?: McpCatalogEntry): McpCatalogEntry {
  if (!['stdio', 'sse', 'http'].includes(form.transport)) {
    throw new InvalidMcpConfig('transport: expected stdio, sse or http')
  }
  if (form.transport === 'stdio' ? !form.command.trim() : !form.url.trim()) {
    throw new InvalidMcpConfig(form.transport === 'stdio' ? 'command: required' : 'url: required')
  }
  const args = parseArgs(form.argsText)
  const env = parseStringMap(form.envText, 'env')
  const headers = parseStringMap(form.headersText, 'headers')
  const original = existing ? formFromEntry(existing) : undefined
  return {
    // Existing legacy names and untouched runtime fields must round-trip exactly.
    name: existing?.name ?? form.name.trim(),
    description: form.description,
    transport: form.transport,
    command: form.command,
    args: existing && form.argsText === original?.argsText ? existing.args : args,
    env: existing && form.envText === original?.envText ? existing.env : env,
    url: form.url,
    headers: existing && form.headersText === original?.headersText ? existing.headers : headers,
    assigned_agents: existing?.assigned_agents ?? [],
    created_at: existing?.created_at ?? '',
    updated_at: existing?.updated_at ?? '',
  }
}

/** 本地智能解析（不依赖 LLM）：JSON / 简单命令 / 裸 URL；不将无效 JSON 降级。 */
export function smartParse(text: string): Partial<EntryForm> {
  const trimmed = text.trim()
  if (!trimmed) return {}
  if (/^https?:\/\//.test(trimmed) && !/\s/.test(trimmed)) {
    return {
      ...EMPTY_FORM,
      transport: /\/sse(?:[/?#]|$)/i.test(trimmed) ? 'sse' : 'http',
      url: trimmed,
    }
  }
  if (/^(npx|uvx|node|python3?|uv|deno|bunx?)(\s|$)/.test(trimmed)) {
    if (/["'`|;&<>]|\\\s/.test(trimmed)) {
      throw new InvalidMcpConfig('command: use JSON for shell quoting or operators')
    }
    const tokens = trimmed.split(/\s+/)
    const command = tokens[0]
    const args = tokens.slice(1)
    const remoteUrl = remoteUrlFromArgs(command, args)
    return {
      ...EMPTY_FORM,
      transport: remoteUrl ? 'http' : 'stdio',
      command,
      argsText: JSON.stringify(args, null, 2),
      url: remoteUrl,
    }
  }

  // Accept copied object members, but never repair missing inner braces or commas.
  const jsonText = /^"(?:[^"\\]|\\.)*"\s*:/.test(trimmed) ? `{${trimmed}}` : trimmed
  const root = parseConfigJson(jsonText, 'config')
  if (!isConfigObject(root)) throw new InvalidMcpConfig('config: expected a JSON object')
  let config = root
  let serverName: string | undefined
  const serverCollectionKey = 'mcpServers' in root ? 'mcpServers' : 'servers' in root ? 'servers' : ''
  if (serverCollectionKey) {
    const collection = root[serverCollectionKey]
    if (!isConfigObject(collection)) {
      throw new InvalidMcpConfig(`${serverCollectionKey}: expected an object containing one server`)
    }
    const servers = Object.entries(collection)
    if (servers.length !== 1 || !isConfigObject(servers[0][1])) {
      throw new InvalidMcpConfig(`${serverCollectionKey}: expected exactly one server configuration`)
    }
    serverName = servers[0][0]
    config = servers[0][1]
  } else if (!['command', 'url', 'serverUrl', 'endpoint', 'transport', 'type'].some((key) => key in root)) {
    // Also accept { "server-name": { ... } }, with or without the outer braces.
    const servers = Object.entries(root)
    if (servers.length !== 1 || !isConfigObject(servers[0][1])) {
      throw new InvalidMcpConfig('config: expected exactly one server configuration')
    }
    serverName = servers[0][0]
    config = servers[0][1]
  }
  const stringField = (key: string) => {
    const value = config[key]
    if (value === undefined) return ''
    if (typeof value !== 'string') throw new InvalidMcpConfig(`${key}: expected a string`)
    return value
  }
  const transportConfig = isConfigObject(config.transport) ? config.transport : undefined
  if (config.transport !== undefined && typeof config.transport !== 'string' && !transportConfig) {
    throw new InvalidMcpConfig('transport: expected a string or object')
  }
  const nestedStringField = (key: string) => {
    const value = transportConfig?.[key]
    if (value === undefined) return ''
    if (typeof value !== 'string') throw new InvalidMcpConfig(`transport.${key}: expected a string`)
    return value
  }
  const declaredTransports = [
    typeof config.transport === 'string' ? normalizeTransport(config.transport, 'transport') : '',
    stringField('type') ? normalizeTransport(stringField('type'), 'type') : '',
    nestedStringField('type') ? normalizeTransport(nestedStringField('type'), 'transport.type') : '',
  ].filter(Boolean) as CanonicalTransport[]
  if (new Set(declaredTransports).size > 1) {
    throw new InvalidMcpConfig('transport/type: conflicting values')
  }
  const directUrls = ['url', 'serverUrl', 'endpoint']
    .map((key) => stringField(key))
    .filter(Boolean)
  const nestedUrl = nestedStringField('url')
  if (nestedUrl) directUrls.push(nestedUrl)
  if (new Set(directUrls).size > 1) {
    throw new InvalidMcpConfig('url: conflicting remote addresses')
  }
  const args = config.args === undefined
    ? []
    : Array.isArray(config.args) && config.args.every((arg) => typeof arg === 'string')
      ? config.args as string[]
      : []
  const command = stringField('command')
  const proxyUrl = remoteUrlFromArgs(command, args)
  const url = directUrls[0] ?? proxyUrl
  const transport = declaredTransports[0] ?? (url ? 'http' : 'stdio')
  if (transport !== 'stdio' && !url) {
    throw new InvalidMcpConfig('url: required for a remote transport')
  }
  if (transport === 'stdio' && !command) {
    throw new InvalidMcpConfig('command: required for stdio')
  }
  const headers = config.headers ?? transportConfig?.headers ?? {}
  if (config.headers !== undefined && transportConfig?.headers !== undefined
      && JSON.stringify(config.headers) !== JSON.stringify(transportConfig.headers)) {
    throw new InvalidMcpConfig('headers: conflicting values')
  }
  const form: EntryForm = {
    name: (serverName ?? stringField('name')).toLowerCase(),
    description: stringField('description'),
    transport,
    command,
    argsText: JSON.stringify(config.args === undefined ? [] : config.args, null, 2),
    envText: JSON.stringify(config.env === undefined ? {} : config.env, null, 2),
    url,
    headersText: JSON.stringify(headers, null, 2),
  }
  // Validate before applying anything so failed parsing leaves the previous form intact.
  entryFromForm(form)
  return form
}

export function applyScan(form: EntryForm, result: McpScanResult): EntryForm {
  return {
    ...form,
    name: result.name,
    description: result.description,
    transport: result.transport,
    command: result.command,
    argsText: JSON.stringify(result.args, null, 2),
    envText: JSON.stringify(result.env, null, 2),
    url: result.url,
    headersText: JSON.stringify(result.headers, null, 2),
  }
}
