import { skillApi } from '../api/skill'
import { create } from 'zustand'
import { useShallow } from 'zustand/react/shallow'

import type { SkillAdoptResult, SkillDocument, SkillDriftFile, SkillItem, SkillPublishedDrift, SkillSyncPreview } from '../types/skill'

interface SkillStore {
  skills: SkillItem[]
  skillCacheReady: boolean
  skillRevision: number
  scanSkills: () => Promise<SkillItem[]>
  loadSkills: (force?: boolean) => Promise<SkillItem[]>
  readSkill: (source: string, name: string) => Promise<SkillDocument>
  skillDriftDetail: (target: string, source: string, name: string) => Promise<SkillDriftFile[]>
  publishedDrift: (signal?: AbortSignal) => Promise<SkillPublishedDrift[]>
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

export const useSkillStore = create<SkillStore>((set, get) => {
  let revision = 0
  let cachedRevision = -1
  let pendingMutations = 0
  let mutationsSettled = Promise.resolve()
  let finishMutations: (() => void) | undefined
  let listRequest: Promise<SkillItem[]> | undefined
  let driftRequest: { revision: number; promise: Promise<SkillPublishedDrift[]> } | undefined

  // Reads spanning a write are retried after all writes settle. Per-item responses
  // still reach the visible catalog immediately, before a full-list refresh.
  async function mutate<T>(run: () => Promise<T>, apply?: (result: T) => void): Promise<T> {
    if (pendingMutations === 0) mutationsSettled = new Promise<void>((resolve) => { finishMutations = resolve })
    pendingMutations += 1
    revision += 1
    try {
      const result = await run()
      apply?.(result)
      return result
    } finally {
      revision += 1
      pendingMutations -= 1
      set({ skillRevision: revision })
      if (pendingMutations === 0) {
        finishMutations?.()
        finishMutations = undefined
      }
    }
  }

  function updateItems(items: SkillItem[]) {
    const updated = new Map(items.map((item) => [`${item.source}:${item.name}`, item]))
    set({ skills: get().skills.map((item) => updated.get(`${item.source}:${item.name}`) ?? item) })
  }

  function checkAborted(signal?: AbortSignal) {
    if (signal?.aborted) throw new DOMException('Skill read cancelled', 'AbortError')
  }

  async function readCurrent<T>(read: () => Promise<T>, signal?: AbortSignal, apply?: (result: T) => void): Promise<T> {
    for (;;) {
      await mutationsSettled
      checkAborted(signal)
      if (pendingMutations > 0) continue
      const startedAt = revision
      try {
        const result = await read()
        checkAborted(signal)
        if (pendingMutations === 0 && startedAt === revision) {
          apply?.(result)
          return result
        }
      } catch (error) {
        checkAborted(signal)
        if (pendingMutations === 0 && startedAt === revision) throw error
      }
    }
  }

  return {
    skills: [],
    skillCacheReady: false,
    skillRevision: 0,

    async scanSkills() {
      const imported = await mutate(() => skillApi.skillScan())
      // 扫描返回值仅用于提示数量；完整目录仍以共享库重新读取为准。
      await get().loadSkills(true)
      return imported
    },

    async loadSkills(force = false) {
      // Force bypasses the settled cache, but joins any live full-list read.
      // Pages force one revalidation per activation; no TTL masks external edits.
      if (listRequest) return listRequest
      if (!force && pendingMutations === 0 && cachedRevision === revision && get().skillCacheReady) return get().skills
      const request = readCurrent(() => skillApi.skillList(), undefined, (skills) => {
        cachedRevision = revision
        set({ skills, skillCacheReady: true })
      })
      listRequest = request
      const clear = () => { if (listRequest === request) listRequest = undefined }
      void request.then(clear, clear)
      return request
    },

    async readSkill(source, name) {
      return skillApi.skillRead({ source, name })
    },

    async skillDriftDetail(target, source, name) {
      return skillApi.skillDriftDetail({ target, source, name })
    },

    async publishedDrift(signal) {
      // Only concurrent reads share results. An aborted hidden-page consumer
      // cannot start another IPC read after a mutation invalidates its response.
      return readCurrent(() => {
        if (driftRequest?.revision === revision) return driftRequest.promise
        const request = { revision, promise: skillApi.skillPublishedDrift() }
        driftRequest = request
        const clear = () => { if (driftRequest === request) driftRequest = undefined }
        void request.promise.then(clear, clear)
        return request.promise
      }, signal)
    },

    async adoptLocalSkill(target, source, name) {
      return mutate(() => skillApi.skillAdoptLocal({ target, source, name }), (result) => updateItems([result.item]))
    },

    async applySkillSyncOne(target, source, name) {
      return mutate(() => skillApi.skillSyncApplyOne({ target, source, name }), (item) => updateItems([item]))
    },

    async previewSkillSync(target) {
      return skillApi.skillSyncPreview({ target })
    },

    async applySkillSync(target, overwrite) {
      return mutate(() => skillApi.skillSyncApply({ target, overwrite }))
    },

    async setSkillStatus(source, name, status) {
      return mutate(() => skillApi.skillSetStatus({ source, name, status }), (item) => updateItems([item]))
    },

    async setSkillAssignment(source, name, target, equipped) {
      return mutate(() => skillApi.skillSetAssignment({ source, name, target, equipped }), (item) => updateItems([item]))
    },

    async setSkillStatusBulk(items, status) {
      const updated = await mutate(() => skillApi.skillSetStatusBulk({ items, status }), updateItems)
      await get().loadSkills(true)
      return updated
    },

    async setSkillAssignmentBulk(items, target, equipped) {
      const updated = await mutate(() => skillApi.skillSetAssignmentBulk({ items, target, equipped }), updateItems)
      await get().loadSkills(true)
      return updated
    },

    async publishSkill(source, name, assignedAgents) {
      return mutate(() => skillApi.skillPublish({ source, name, assignedAgents }), (item) => updateItems([item]))
    },

    async rollbackSkillLatest(source, name) {
      return mutate(() => skillApi.skillRollbackLatest({ source, name }), (item) => updateItems([item]))
    },

    async deleteSkill(source, name) {
      await mutate(() => skillApi.skillDelete({ source, name }), () => {
        set({ skills: get().skills.filter((item) => !(item.source === source && item.name === name)) })
      })
    },
  }
})

/** Subscribe only to fields owned by this feature. */
export function useSkillFields<const K extends keyof SkillStore>(...keys: K[]): Pick<SkillStore, K> {
  return useSkillStore(useShallow((state) => {
    const selected = {} as Pick<SkillStore, K>
    for (const key of keys) selected[key] = state[key]
    return selected
  }))
}
