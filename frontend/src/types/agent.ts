export type AgentStatus = 'running' | 'stopped' | 'error' | 'starting'

export interface AgentConfig {
  id: string
  name: string
  description: string
  command: string
  args: string[]
  working_dir: string
  env: Record<string, string>
  port?: number
  ui_token?: string
  /** Git worktree 隔离：源仓库绝对路径；设置后 Agent 在独立工作树中运行 */
  worktree_repo?: string
  /** worktree 检出分支（缺省分离 HEAD） */
  worktree_branch?: string
  auto_restart: boolean
  created_at: string
  updated_at: string
}

export interface AgentState {
  config: AgentConfig
  status: AgentStatus
  pid?: number
  started_at?: string
  port_open: boolean
  /** 自动重启累计次数 */
  restart_count?: number
  /** 最近一次进程退出码 */
  last_exit_code?: number | null
}

export interface LogEntry {
  timestamp: string
  level: 'info' | 'warn' | 'error' | 'debug'
  message: string
}
