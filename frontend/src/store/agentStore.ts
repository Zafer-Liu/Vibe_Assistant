import { invoke } from '@tauri-apps/api/core'
import type { AgentConfig, AgentState, LogEntry } from '../types/agent'
import { create } from 'zustand'

const ORDER_KEY = 'agent-order'

function loadOrder(): string[] {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(ORDER_KEY) ?? '[]')
    if (!Array.isArray(value)) return []
    return [...new Set(value.filter((id): id is string => typeof id === 'string' && id.length > 0))]
  } catch {
    return []
  }
}
function saveOrder(ids: string[]) {
  localStorage.setItem(ORDER_KEY, JSON.stringify(ids))
}

function reconcileOrder(
  agents: AgentState[],
  savedOrder: string[],
  currentAgents: AgentState[],
): { agents: AgentState[]; order: string[] } {
  const map = new Map(agents.map(a => [a.config.id, a]))
  const order: string[] = []
  const seen = new Set<string>()

  const appendExisting = (ids: string[]) => {
    for (const id of ids) {
      if (map.has(id) && !seen.has(id)) {
        seen.add(id)
        order.push(id)
      }
    }
  }

  // Persisted drag order wins. The current visible order keeps the list stable
  // during overlapping refreshes. Brand-new agents are appended deterministically.
  appendExisting(savedOrder)
  appendExisting(currentAgents.map(agent => agent.config.id))

  const newIds = agents
    .filter(agent => !seen.has(agent.config.id))
    .sort((a, b) => {
      const created = a.config.created_at.localeCompare(b.config.created_at)
      return created || a.config.id.localeCompare(b.config.id)
    })
    .map(agent => agent.config.id)
  appendExisting(newIds)

  return {
    agents: order.map(id => map.get(id)!),
    order,
  }
}

interface AgentStore {
  agents: AgentState[]
  order: string[]
  selectedId: string | null
  logs: Record<string, LogEntry[]>
  loading: boolean
  fetchAgents: () => Promise<void>
  selectAgent: (id: string) => void
  reorderAgents: (newOrder: string[]) => void
  startAgent: (id: string) => Promise<void>
  stopAgent: (id: string) => Promise<void>
  saveAgent: (config: Partial<AgentConfig>) => Promise<void>
  deleteAgent: (id: string) => Promise<void>
  fetchLogs: (id: string) => Promise<void>
}

export const useAgentStore = create<AgentStore>((set, get) => ({
  agents: [],
  order: loadOrder(),
  selectedId: null,
  logs: {},
  loading: false,

  fetchAgents: async () => {
    set({ loading: true })
    try {
      const raw = await invoke<AgentState[]>('list_agents')
      const current = get()
      // Compare the reconciled list, including configuration and IDs. Otherwise
      // saving a corrected command/worktree can leave the UI on its old config.
      const reconciled = reconcileOrder(raw, current.order, current.agents)
      if (JSON.stringify(current.agents) === JSON.stringify(reconciled.agents)) {
        set({ loading: false })
        return
      }
      saveOrder(reconciled.order)
      set({ agents: reconciled.agents, order: reconciled.order, loading: false })
    } catch {
      set({ loading: false })
    }
  },

  selectAgent: (id) => {
    set({ selectedId: id })
    void get().fetchLogs(id).catch(() => {})
  },

  reorderAgents: (newOrder) => {
    saveOrder(newOrder)
    set(s => ({
      order: newOrder,
      agents: newOrder
        .map(id => s.agents.find(agent => agent.config.id === id))
        .filter((agent): agent is AgentState => agent !== undefined),
    }))
  },

  startAgent: async (id) => {
    await invoke('start_agent', { id })
    await get().fetchAgents()
  },

  stopAgent: async (id) => {
    await invoke('stop_agent', { id })
    await get().fetchAgents()
  },

  saveAgent: async (config) => {
    await invoke('save_agent_config', { config })
    await get().fetchAgents()
  },

  deleteAgent: async (id) => {
    await invoke('delete_agent', { id })
    set(s => ({
      agents: s.agents.filter(a => a.config.id !== id),
      selectedId: s.selectedId === id ? null : s.selectedId,
      order: s.order.filter(oid => oid !== id),
    }))
    saveOrder(get().order)
  },

  fetchLogs: async (id) => {
    const logs = await invoke<LogEntry[]>('get_agent_logs', { id })
    const current = get().logs[id]
    // 如果日志条数没变且最后一条相同，跳过更新
    if (current && current.length === logs.length && current.length > 0) {
      if (current[current.length - 1].timestamp === logs[logs.length - 1].timestamp) {
        return
      }
    }
    set(s => ({ logs: { ...s.logs, [id]: logs } }))
  },
}))
