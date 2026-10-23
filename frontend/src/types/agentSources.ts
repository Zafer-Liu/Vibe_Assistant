/** Agent 数据源（转录目录 / MCP 配置）的当前生效路径，支持按设备覆盖。 */
export interface AgentSourcePathInfo {
  path: string
  exists: boolean
  is_override: boolean
}

export interface AgentSourceInfo {
  id: string
  label: string
  supports_hooks: boolean
  supports_transcript_scan: boolean
  scan_enabled: boolean
  transcript_roots: AgentSourcePathInfo[]
  default_transcript_roots: string[]
  mcp_config_path?: string | null
}
