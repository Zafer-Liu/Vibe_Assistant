import { invoke } from '@tauri-apps/api/core'
import type { SkillAdoptResult, SkillDocument, SkillDriftFile, SkillItem, SkillPublishedDrift, SkillSyncPreview } from '../types/skill'

export const skillApi = {
  skillScan: () => invoke<SkillItem[]>('skill_scan'),
  skillList: () => invoke<SkillItem[]>('skill_list'),
  skillRead: (args: { source: string; name: string }) => invoke<SkillDocument>('skill_read', args),
  skillDriftDetail: (args: { target: string; source: string; name: string }) => invoke<SkillDriftFile[]>('skill_drift_detail', args),
  skillPublishedDrift: () => invoke<SkillPublishedDrift[]>('skill_published_drift'),
  skillAdoptLocal: (args: { target: string; source: string; name: string }) => invoke<SkillAdoptResult>('skill_adopt_local', args),
  skillSyncApplyOne: (args: { target: string; source: string; name: string }) => invoke<SkillItem>('skill_sync_apply_one', args),
  skillSyncPreview: (args: { target: string }) => invoke<SkillSyncPreview>('skill_sync_preview', args),
  skillSyncApply: (args: { target: string; overwrite: boolean }) => invoke<SkillSyncPreview>('skill_sync_apply', args),
  skillSetStatus: (args: { source: string; name: string; status: SkillItem['status'] }) => invoke<SkillItem>('skill_set_status', args),
  skillSetAssignment: (args: { source: string; name: string; target: string; equipped: boolean }) => invoke<SkillItem>('skill_set_assignment', args),
  skillSetStatusBulk: (args: { items: { source: string; name: string }[]; status: SkillItem['status'] }) => invoke<SkillItem[]>('skill_set_status_bulk', args),
  skillSetAssignmentBulk: (args: { items: { source: string; name: string }[]; target: string; equipped: boolean }) => invoke<SkillItem[]>('skill_set_assignment_bulk', args),
  skillPublish: (args: { source: string; name: string; assignedAgents: string[] }) => invoke<SkillItem>('skill_publish', args),
  skillRollbackLatest: (args: { source: string; name: string }) => invoke<SkillItem>('skill_rollback_latest', args),
  skillDelete: (args: { source: string; name: string }) => invoke<void>('skill_delete', args),
}
