import { invoke } from '@tauri-apps/api/core'
import type { TelemetryEvent, TelemetryLiveStatus, TelemetrySummary, TelemetryUsageAnalytics, TelemetryUsageRecord, TelemetryUsageRefresh } from '../types/telemetry'

export const telemetryApi = {
  telemetryBackfillConversations: () => invoke<number>('telemetry_backfill_conversations'),
  telemetryRefreshUsage: () => invoke<TelemetryUsageRefresh>('telemetry_refresh_usage'),
  telemetrySummary: () => invoke<TelemetrySummary>('telemetry_summary'),
  telemetryRecentEvents: (args: { limit: number }) => invoke<TelemetryEvent[]>('telemetry_recent_events', args),
  telemetryUsageRecords: (args: { limit: number }) => invoke<TelemetryUsageRecord[]>('telemetry_usage_records', args),
  telemetryLiveStatus: () => invoke<TelemetryLiveStatus>('telemetry_live_status'),
  telemetryUsageAnalytics: (args: { startAt: string | null; endAt: string | null; source: string | null; bucket: 'hour' | 'day'; recordLimit: number }) => invoke<TelemetryUsageAnalytics>('telemetry_usage_analytics', args),
}
