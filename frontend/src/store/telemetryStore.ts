import { telemetryApi } from '../api/telemetry'
import { create } from 'zustand'
import { useShallow } from 'zustand/react/shallow'

import type { TelemetryEvent, TelemetryLiveStatus, TelemetrySummary, TelemetryUsageAnalytics, TelemetryUsageRecord, TelemetryUsageRefresh } from '../types/telemetry'
import { jsonEqual } from '../lib/stateEquality'

interface TelemetryStore {
  telemetrySummary: TelemetrySummary | null
  telemetryLiveStatus: TelemetryLiveStatus | null
  telemetryEvents: TelemetryEvent[]
  telemetryUsageRecords: TelemetryUsageRecord[]
  /** 常规刷新只读取账本；仅在用户主动刷新时回扫本机转录。 */
  checkTelemetry: (options?: { backfill?: boolean; refreshUsage?: boolean; limit?: number }) => Promise<TelemetryUsageRefresh | null>
  loadUsageAnalytics: (filters: { startAt?: string; endAt?: string; source?: string; bucket: 'hour' | 'day' }) => Promise<TelemetryUsageAnalytics>
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
      // The summary is the primary UI. Live status is a supplementary query,
      // so a migration/older backend must never hide the Token panel merely
      // because that supplementary query is temporarily unavailable.
      const [summaryResult, eventsResult, usageRecordsResult, liveResult] = await Promise.allSettled([
        telemetryApi.telemetrySummary(),
        telemetryApi.telemetryRecentEvents({ limit: options.limit ?? 20 }),
        telemetryApi.telemetryUsageRecords({ limit: Math.max(options.limit ?? 20, 200) }),
        telemetryApi.telemetryLiveStatus(),
      ])
      set((state) => {
        const next = {
          telemetrySummary: summaryResult.status === 'fulfilled' ? summaryResult.value : state.telemetrySummary,
          telemetryEvents: eventsResult.status === 'fulfilled' ? eventsResult.value : state.telemetryEvents,
          telemetryUsageRecords: usageRecordsResult.status === 'fulfilled' ? usageRecordsResult.value : state.telemetryUsageRecords,
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
    return telemetryApi.telemetryUsageAnalytics({
      startAt: filters.startAt ?? null,
      endAt: filters.endAt ?? null,
      source: filters.source && filters.source !== 'all' ? filters.source : null,
      bucket: filters.bucket,
      recordLimit: 500,
    })
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
