import { skillApi } from '../api/skill'
import { create } from 'zustand'
import { useShallow } from 'zustand/react/shallow'

import type { SkillAdoptResult, SkillDocument, SkillDriftFile, SkillItem, SkillPublishedDrift, SkillSyncPreview } from '../types/skill'

interface SkillStore {
  skills: SkillItem[]
  skillCacheReady: boolean
  scanSkills: () => Promise<SkillItem[]>
  loadSkills: (force?: boolean) => Promise<SkillItem[]>
  readSkill: (source: string, name: string) => Promise<SkillDocument>
  skillDriftDetail: (target: string, source: string, name: string) => Promise<SkillDriftFile[]>
  publishedDrift: () => Promise<SkillPublishedDrift[]>
  adoptLocalSkill: (target: string, source: string, name: string) => Promise<SkillAdoptResult>
  applySkillSyncOne: (target: string, source: string, name: string) => Promise<SkillItem>
  previewSkillSync: (target: string) => Promise<SkillSyncPreview>
  applySkillSync: (target: string, overwrite: boolean) => Promise<SkillSyncPreview>
  setSkillStatus: (source: string, name: string, status: 'draft' | 'published') => Promise<SkillItem>
  setSkillAssignment: (source: string, name: string, target: string, equipped: boolean) => Promise<SkillItem>
  setSkillStatusBulk: (items: { source: string; name: string }[], status: 'draft' | 'published') => Promise<SkillItem[]>
  setSkillAssignmentBulk: (items: { source: string; name: string }[], target: string, equipped: boolean) => Promise<SkillItem[]>
  publishSkill: (source: string, name: string, assignedAgents: string[]) => Promise<SkillItem>
  rollbackSkillLatest: (source: string, name: string) => Promise<SkillItem>
  deleteSkill: (source: string, name: string) => Promise<void>
}

export const useSkillStore = create<SkillStore>((set, get) => ({
  skills: [],

  skillCacheReady: false,

  async scanSkills() {
    const imported = await skillApi.skillScan()
    // 扫描返回值仅用于提示数量；列表数据以共享库缓存为准重新加载，
    // 与其他变更操作保持一致，避免把中间态直接当成目录状态。
    await get().loadSkills(true)
    return imported
  },

  async loadSkills(force = false) {
    if (!force && get().skillCacheReady) return get().skills
    const skills = await skillApi.skillList()
    set({ skills, skillCacheReady: true })
    return skills
  },

  async readSkill(source, name) {
    return skillApi.skillRead({ source, name })
  },

  async skillDriftDetail(target, source, name) {
    return skillApi.skillDriftDetail({ target, source, name })
  },

  async publishedDrift() {
    return skillApi.skillPublishedDrift()
  },

  async adoptLocalSkill(target, source, name) {
    const result = await skillApi.skillAdoptLocal({ target, source, name })
    set({ skills: get().skills.map(s => s.source === result.item.source && s.name === result.item.name ? result.item : s) })
    return result
  },

  async applySkillSyncOne(target, source, name) {
    const item = await skillApi.skillSyncApplyOne({ target, source, name })
    set({ skills: get().skills.map(s => s.source === item.source && s.name === item.name ? item : s) })
    return item
  },

  async previewSkillSync(target) {
    return skillApi.skillSyncPreview({ target })
  },

  async applySkillSync(target, overwrite) {
    const result = await skillApi.skillSyncApply({ target, overwrite })
    return result
  },

  async setSkillStatus(source, name, status) {
   const item = await skillApi.skillSetStatus({ source, name, status })
   set({ skills: get().skills.map(s => s.source === item.source && s.name === item.name ? item : s) })
   return item
 },

  async setSkillAssignment(source, name, target, equipped) {
   const item = await skillApi.skillSetAssignment({ source, name, target, equipped })
   set({ skills: get().skills.map(s => s.source === item.source && s.name === item.name ? item : s) })
   return item
 },

  async setSkillStatusBulk(items, status) {
   const updated = await skillApi.skillSetStatusBulk({ items, status })
   await get().loadSkills(true)
   return updated
 },

  async setSkillAssignmentBulk(items, target, equipped) {
   const updated = await skillApi.skillSetAssignmentBulk({ items, target, equipped })
   await get().loadSkills(true)
   return updated
 },

  async publishSkill(source, name, assignedAgents) {
   const item = await skillApi.skillPublish({ source, name, assignedAgents })
   set({ skills: get().skills.map(s => s.source === item.source && s.name === item.name ? item : s) })
   return item
 },

  async rollbackSkillLatest(source, name) {
   const item = await skillApi.skillRollbackLatest({ source, name })
   set({ skills: get().skills.map(s => s.source === item.source && s.name === item.name ? item : s) })
   return item
 },

  async deleteSkill(source, name) {
   await skillApi.skillDelete({ source, name })
   set({ skills: get().skills.filter(s => !(s.source === source && s.name === name)) })
 }
}))

/** Subscribe only to fields owned by this feature. */
export function useSkillFields<const K extends keyof SkillStore>(...keys: K[]): Pick<SkillStore, K> {
  return useSkillStore(useShallow((state) => {
    const selected = {} as Pick<SkillStore, K>
    for (const key of keys) selected[key] = state[key]
    return selected
  }))
}
