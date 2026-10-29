import { mcpApi } from '../api/mcp'
import { create } from 'zustand'
import { useShallow } from 'zustand/react/shallow'

import type { McpAgentStatus, McpCallRefresh, McpCallStats, McpCatalogEntry, McpImportScan, McpObservationStatus, DshBridgeStatus, DshBridgeFeatures, McpProfile, MemoryMcpRuntimeStatus, MemoryMcpStatus, MemoryMcpTarget } from '../types/mcp'

import { jsonEqual } from '../lib/stateEquality'

let callStatsRequestId = 0
let callStatsScope = ''

interface McpStore {
  /** Codex / Claude connection state for the shared-memory MCP server. */
  memoryMcp: Record<MemoryMcpTarget, MemoryMcpStatus | null>
  /** Historical shared-memory MCP activity; this never claims clients are live. */
  memoryMcpRuntime: Record<MemoryMcpTarget, MemoryMcpRuntimeStatus | null>
  memoryMcpRuntimeError: string
  mcpCallStats: McpCallStats | null
  mcpCallRefreshState: 'idle' | 'loading' | 'ready' | 'error'
  mcpCallError: string
  checkMemoryMcp: () => Promise<void>
  checkMemoryMcpRuntime: () => Promise<void>
  installMemoryMcp: (agentType: MemoryMcpTarget) => Promise<MemoryMcpStatus>
  uninstallMemoryMcp: (agentType: MemoryMcpTarget) => Promise<void>
  loadMcpCallStats: (filters?: { startAt?: string; endAt?: string; source?: string; server?: string; tool?: string; recentLimit?: number }) => Promise<McpCallStats>
  refreshMcpCallObservations: () => Promise<McpCallRefresh>
  // ── MCP 库 ──
  mcpCatalog: McpCatalogEntry[]
  mcpStatuses: McpAgentStatus[]
  mcpObservationStatuses: McpObservationStatus[]
  mcpObservationError: string
  /** DeepSeek Harness 统一桥接插件（dsh-vibe-assistant-bridge）的安装状态。 */
  dshBridge: DshBridgeStatus | null
  dshBridgeError: string
  mcpStatusError: string
  mcpStatusState: 'idle' | 'loading' | 'ready' | 'error'
  mcpStatusCheckedAt: string | null
  mcpProfiles: McpProfile[]
  mcpProfilesError: string
  loadMcpCatalog: () => Promise<McpCatalogEntry[]>
  loadMcpProfiles: () => Promise<McpProfile[]>
  saveMcpProfile: (profile: McpProfile) => Promise<McpProfile>
  deleteMcpProfile: (name: string) => Promise<void>
  upsertMcpEntry: (entry: McpCatalogEntry) => Promise<McpCatalogEntry>
  importMcpEntry: (name: string, agent: string) => Promise<McpCatalogEntry>
  deleteMcpEntry: (name: string) => Promise<void>
  equipMcp: (name: string, agent: string) => Promise<McpCatalogEntry>
  unequipMcp: (name: string, agent: string) => Promise<McpCatalogEntry>
  syncMcpAgent: (agent: string) => Promise<void>
  checkMcpStatuses: () => Promise<void>
  loadMcpObservationStatuses: () => Promise<McpObservationStatus[]>
  setMcpObservation: (name: string, agent: string, enabled: boolean) => Promise<McpObservationStatus>
  loadDshBridge: () => Promise<DshBridgeStatus>
  setDshBridge: (enabled: boolean) => Promise<DshBridgeStatus>
  setDshBridgeFeatures: (features: DshBridgeFeatures) => Promise<DshBridgeStatus>
  importMcpFromAgents: () => Promise<McpImportScan>
  invalidateMcpStatuses: () => void
}

export const useMcpStore = create<McpStore>((set, get) => ({
  memoryMcp: { codex_cli: null, claude_cli: null, codex_desktop: null, claude_desktop: null, qoder: null, workbuddy: null, minimax: null, kimi: null, zcode: null, dsh: null },

  memoryMcpRuntime: { codex_cli: null, claude_cli: null, codex_desktop: null, claude_desktop: null, qoder: null, workbuddy: null, minimax: null, kimi: null, zcode: null, dsh: null },

  memoryMcpRuntimeError: '',

  mcpCallStats: null,

  mcpCallRefreshState: 'idle',

  mcpCallError: '',

  mcpCatalog: [],

  mcpStatuses: [],

  mcpObservationStatuses: [],

  mcpObservationError: '',

  dshBridge: null,

  dshBridgeError: '',

  mcpStatusError: '',

  mcpStatusState: 'idle',

  mcpStatusCheckedAt: null,

  mcpProfiles: [],

  mcpProfilesError: '',

  async checkMemoryMcp() {
    const targets: MemoryMcpTarget[] = ['codex_cli', 'claude_cli', 'codex_desktop', 'claude_desktop', 'qoder', 'workbuddy', 'minimax', 'kimi', 'zcode', 'dsh']
    const results = await Promise.allSettled(targets.map((agentType) => mcpApi.memoryMcpStatus({ agentType })))
    set((state) => {
      const next = { ...state.memoryMcp }
      results.forEach((result, index) => {
        const agentType = targets[index]
        next[agentType] = result.status === 'fulfilled' ? result.value : {
          agent_type: agentType, installed: false, executable: '', detail: String(result.reason),
        }
      })
      return jsonEqual(state.memoryMcp, next) ? state : { memoryMcp: next }
    })
  },

  async checkMemoryMcpRuntime() {
    try {
      const list = await mcpApi.memoryMcpRuntimeStatus()
      set((state) => {
        const next = { ...state.memoryMcpRuntime }
        for (const item of list) {
          next[item.agent_type as MemoryMcpTarget] = item
        }
        return jsonEqual(state.memoryMcpRuntime, next) && state.memoryMcpRuntimeError === ''
          ? state
          : { memoryMcpRuntime: next, memoryMcpRuntimeError: '' }
      })
    } catch (error) {
      set({ memoryMcpRuntimeError: String(error) })
    }
  },

  async installMemoryMcp(agentType) {
    const status = await mcpApi.memoryMcpInstall({ agentType })
    set((state) => ({ memoryMcp: { ...state.memoryMcp, [agentType]: status } }))
    return status
  },

  async uninstallMemoryMcp(agentType) {
    await mcpApi.memoryMcpUninstall({ agentType })
    await get().checkMemoryMcp()
  },

  async loadMcpCallStats(filters = {}) {
    const requestId = ++callStatsRequestId
    const args = {
      startAt: filters.startAt ?? null,
      endAt: filters.endAt ?? null,
      source: filters.source ?? null,
      server: filters.server ?? null,
      tool: filters.tool ?? null,
      recentLimit: filters.recentLimit ?? 50,
    }
    const scope = JSON.stringify(args)
    const changedScope = scope !== callStatsScope
    callStatsScope = scope
    set({ mcpCallRefreshState: 'loading', mcpCallError: '', ...(changedScope ? { mcpCallStats: null } : {}) })
    try {
      const stats = await mcpApi.telemetryMcpStats(args)
      if (requestId !== callStatsRequestId) return stats
      set((state) => jsonEqual(state.mcpCallStats, stats) ? { mcpCallRefreshState: 'ready' } : { mcpCallStats: stats, mcpCallRefreshState: 'ready' })
      return stats
    } catch (error) {
      if (requestId === callStatsRequestId) set({ mcpCallRefreshState: 'error', mcpCallError: String(error) })
      throw error
    }
  },

  async refreshMcpCallObservations() {
    // Background import must not overwrite the active call-view scope or request state.
    return mcpApi.telemetryRefreshMcpCalls()
  },

  async loadMcpCatalog() {
    const catalog = await mcpApi.mcpCatalogList()
    set((state) => (jsonEqual(state.mcpCatalog, catalog) ? state : { mcpCatalog: catalog }))
    return catalog
  },

  async loadMcpProfiles() {
    try {
      const profiles = await mcpApi.mcpProfileList()
      set((state) => (jsonEqual(state.mcpProfiles, profiles)
        ? { mcpProfilesError: '' }
        : { mcpProfiles: profiles, mcpProfilesError: '' }))
      return profiles
    } catch (error) {
      set({ mcpProfilesError: String(error) })
      throw error
    }
  },

  async saveMcpProfile(profile) {
    const saved = await mcpApi.mcpProfileSave({ profile })
    set((state) => ({
      mcpProfiles: state.mcpProfiles.some(item => item.name === saved.name)
        ? state.mcpProfiles.map(item => item.name === saved.name ? saved : item)
        : [...state.mcpProfiles, saved],
      mcpProfilesError: '',
    }))
    return saved
  },

  async deleteMcpProfile(name) {
    await mcpApi.mcpProfileDelete({ name })
    set((state) => ({ mcpProfiles: state.mcpProfiles.filter(profile => profile.name !== name) }))
  },

  async upsertMcpEntry(entry) {
    const saved = await mcpApi.mcpCatalogUpsert({ entry })
    set((state) => {
      const exists = state.mcpCatalog.some((e) => e.name === saved.name)
      const mcpCatalog = exists
        ? state.mcpCatalog.map((e) => (e.name === saved.name ? saved : e))
        : [...state.mcpCatalog, saved]
      return { mcpCatalog }
    })
    return saved
  },

  async importMcpEntry(name, agent) {
    const saved = await mcpApi.mcpCatalogImport({ name, agent })
    set((state) => ({
      mcpCatalog: state.mcpCatalog.some(entry => entry.name === saved.name)
        ? state.mcpCatalog.map(entry => entry.name === saved.name ? saved : entry)
        : [...state.mcpCatalog, saved],
    }))
    // Import succeeded. Status failures are retained separately for the UI.
    await get().checkMcpStatuses().catch(() => {})
    return saved
  },

  async deleteMcpEntry(name) {
    await mcpApi.mcpCatalogDelete({ name })
    set((state) => ({
      mcpCatalog: state.mcpCatalog.filter((e) => e.name !== name),
      mcpStatuses: state.mcpStatuses.filter((s) => s.name !== name),
      mcpObservationStatuses: state.mcpObservationStatuses.filter((s) => s.name !== name),
    }))
  },

  async equipMcp(name, agent) {
    const saved = await mcpApi.mcpEquip({ name, agent })
    set((state) => ({ mcpCatalog: state.mcpCatalog.map((e) => (e.name === saved.name ? saved : e)) }))
    // The write succeeded; status errors remain visible through mcpStatusError.
    await get().checkMcpStatuses().catch(() => {})
    await get().loadMcpObservationStatuses().catch(() => {})
    return saved
  },

  async unequipMcp(name, agent) {
    const saved = await mcpApi.mcpUnequip({ name, agent })
    set((state) => ({ mcpCatalog: state.mcpCatalog.map((e) => (e.name === saved.name ? saved : e)) }))
    // The write succeeded; status errors remain visible through mcpStatusError.
    await get().checkMcpStatuses().catch(() => {})
    await get().loadMcpObservationStatuses().catch(() => {})
    return saved
  },

  async syncMcpAgent(agent) {
    await mcpApi.mcpSyncAgent({ agent })
    await get().checkMcpStatuses().catch(() => {})
  },

  async checkMcpStatuses() {
    set({ mcpStatusState: 'loading' })
    try {
      const statuses = await mcpApi.mcpStatusAll()
      set((state) => ({
        ...(jsonEqual(state.mcpStatuses, statuses) && !state.mcpStatusError ? {} : { mcpStatuses: statuses }),
        mcpStatusError: '',
        mcpStatusState: 'ready',
        mcpStatusCheckedAt: new Date().toISOString(),
      }))
    } catch (error) {
      set({ mcpStatusError: String(error), mcpStatusState: 'error' })
      throw error
    }
  },

  async loadMcpObservationStatuses() {
    try {
      const statuses = await mcpApi.mcpObservationStatusAll()
      set((state) => ({
        ...(jsonEqual(state.mcpObservationStatuses, statuses) ? {} : { mcpObservationStatuses: statuses }),
        mcpObservationError: '',
      }))
      return statuses
    } catch (error) {
      set({ mcpObservationError: String(error) })
      throw error
    }
  },

  async setMcpObservation(name, agent, enabled) {
    const status = await mcpApi.mcpObservationSet({ name, agent, enabled })
    set((state) => ({
      mcpObservationStatuses: [
        ...state.mcpObservationStatuses.filter(item => item.name !== status.name || item.agent !== status.agent),
        status,
      ],
      mcpObservationError: '',
    }))
    return status
  },

  async loadDshBridge() {
    try {
      const status = await mcpApi.dshBridgeStatus()
      set((state) => ({
        ...(jsonEqual(state.dshBridge, status) ? {} : { dshBridge: status }),
        dshBridgeError: '',
      }))
      return status
    } catch (error) {
      set({ dshBridgeError: String(error) })
      throw error
    }
  },

  async setDshBridge(enabled) {
    const status = await mcpApi.dshBridgeSet({ enabled })
    set({ dshBridge: status, dshBridgeError: '' })
    return status
  },

  async setDshBridgeFeatures(features) {
    const status = await mcpApi.dshBridgeFeatures({ features })
    set({ dshBridge: status, dshBridgeError: '' })
    return status
  },

  async importMcpFromAgents() {
    return mcpApi.mcpImportFromAgents()
  },

  invalidateMcpStatuses() {
    set({ mcpStatuses: [], mcpStatusError: '' })
  }
}))

/** Subscribe only to fields owned by this feature. */
export function useMcpFields<const K extends keyof McpStore>(...keys: K[]): Pick<McpStore, K> {
  return useMcpStore(useShallow((state) => {
    const selected = {} as Pick<McpStore, K>
    for (const key of keys) selected[key] = state[key]
    return selected
  }))
}
