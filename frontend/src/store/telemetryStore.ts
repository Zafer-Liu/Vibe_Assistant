import { telemetryApi } from '../api/telemetry'
import { create } from 'zustand'
import { useShallow } from 'zustand/react/shallow'

import type { TelemetryEvent, TelemetryLiveStatus, TelemetrySummary, TelemetryUsageAnalytics, TelemetryUsageBucket, TelemetryUsageRecord, TelemetryUsageRefresh } from '../types/telemetry'
import { jsonEqual } from '../lib/stateEquality'

interface TelemetryStore {
  telemetrySummary: TelemetrySummary | null
  telemetryLiveStatus: TelemetryLiveStatus | null
  telemetryEvents: TelemetryEvent[]
  telemetryUsageRecords: TelemetryUsageRecord[]
  /** 常规刷新只读取账本；仅在用户主动刷新时回扫本机转录。 */
  checkTelemetry: (options?: { backfill?: boolean; refreshUsage?: boolean; limit?: number; events?: boolean; details?: boolean }) => Promise<TelemetryUsageRefresh | null>
  loadUsageAnalytics: (filters: { startAt?: string; endAt?: string; source?: string; bucket: 'hour' | 'day' }) => Promise<TelemetryUsageAnalytics>
  loadUsageCalendar: (filters: { startAt?: string; endAt?: string; source?: string }) => Promise<TelemetryUsageBucket[]>
}

const summaryRequests = new Map<string, Promise<TelemetrySummary>>()
const liveRequests = new Map<string, Promise<TelemetryLiveStatus>>()
const eventRequests = new Map<string, Promise<TelemetryEvent[]>>()
const recordRequests = new Map<string, Promise<TelemetryUsageRecord[]>>()
const usageRequests = new Map<string, Promise<TelemetryUsageAnalytics>>()
const calendarRequests = new Map<string, Promise<TelemetryUsageBucket[]>>()

// Polls, sync events and manual refreshes can ask for the same data together.
// Share only pending work; completed responses are never retained as stale data.
async function coalescedRequest<T>(requests: Map<string, Promise<T>>, key: string, load: () => Promise<T>): Promise<T> {
  const pending = requests.get(key)
  if (pending) return pending
  const request = load()
  requests.set(key, request)
  try {
    return await request
  } finally {
    if (requests.get(key) === request) requests.delete(key)
  }
}

export const useTelemetryStore = create<TelemetryStore>((set) => ({
  telemetrySummary: null,

  telemetryLiveStatus: null,

  telemetryEvents: [],

  telemetryUsageRecords: [],

  async checkTelemetry(options = {}) {
    try {
      if (options.backfill) await telemetryApi.telemetryBackfillConversations()
      const usageRefresh = options.refreshUsage
        ? await telemetryApi.telemetryRefreshUsage()
        : null
      if (options.backfill || options.refreshUsage) {
        // A post-write refresh must not join a read started before that write.
        summaryRequests.clear()
        liveRequests.clear()
        eventRequests.clear()
        recordRequests.clear()
      }
      // Overview polling does not transfer conversation bodies or unused request
      // details. Capture/detail pages explicitly opt in when they need them.
      const limit = options.limit ?? 20
      const [summaryResult, eventsResult, usageRecordsResult, liveResult] = await Promise.allSettled([
        coalescedRequest(summaryRequests, 'summary', () => telemetryApi.telemetrySummary()),
        options.events ? coalescedRequest(eventRequests, String(limit), () => telemetryApi.telemetryRecentEvents({ limit })) : Promise.resolve(null),
        options.details ? coalescedRequest(recordRequests, String(limit), () => telemetryApi.telemetryUsageRecords({ limit: Math.max(limit, 200) })) : Promise.resolve(null),
        coalescedRequest(liveRequests, 'live', () => telemetryApi.telemetryLiveStatus()),
      ])
      set((state) => {
        const next = {
          telemetrySummary: summaryResult.status === 'fulfilled' ? summaryResult.value : state.telemetrySummary,
          telemetryEvents: eventsResult.status === 'fulfilled' && eventsResult.value !== null ? eventsResult.value : state.telemetryEvents,
          telemetryUsageRecords: usageRecordsResult.status === 'fulfilled' && usageRecordsResult.value !== null ? usageRecordsResult.value : state.telemetryUsageRecords,
          telemetryLiveStatus: liveResult.status === 'fulfilled' ? liveResult.value : state.telemetryLiveStatus,
        }
        return jsonEqual(state.telemetrySummary, next.telemetrySummary) && jsonEqual(state.telemetryEvents, next.telemetryEvents) && jsonEqual(state.telemetryUsageRecords, next.telemetryUsageRecords) && jsonEqual(state.telemetryLiveStatus, next.telemetryLiveStatus)
          ? state
          : next
      })
      return usageRefresh
    } catch {
      /* 浏览器预览下无 Tauri 后端，忽略 */
      return null
    }
  },

  async loadUsageAnalytics(filters) {
    const args = {
      startAt: filters.startAt ?? null,
      endAt: filters.endAt ?? null,
      source: filters.source && filters.source !== 'all' ? filters.source : null,
      bucket: filters.bucket,
      recordLimit: 500,
    }
    return coalescedRequest(usageRequests, JSON.stringify(args), () => telemetryApi.telemetryUsageAnalytics(args))
  },

  async loadUsageCalendar(filters) {
    const args = {
      startAt: filters.startAt ?? null,
      endAt: filters.endAt ?? null,
      source: filters.source && filters.source !== 'all' ? filters.source : null,
    }
    return coalescedRequest(calendarRequests, JSON.stringify(args), () => telemetryApi.telemetryUsageCalendar(args))
  }
}))

/** Subscribe only to fields owned by this feature. */
export function useTelemetryFields<const K extends keyof TelemetryStore>(...keys: K[]): Pick<TelemetryStore, K> {
  return useTelemetryStore(useShallow((state) => {
    const selected = {} as Pick<TelemetryStore, K>
    for (const key of keys) selected[key] = state[key]
    return selected
  }))
}
