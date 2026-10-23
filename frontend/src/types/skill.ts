export interface SkillItem {
  source: string
  name: string
  description: string
  path: string
  hash: string
  version: number
  status: 'draft' | 'published'
  assigned_agents: string[]
  /** Skill 目录内打包发布的全部文件（相对路径，含 SKILL.md） */
  files?: string[]
  /** 技能性质：builtin（厂商内置）/ plugin（插件市场安装）/ local（本地
   *  自建或 Vibe Assistant 部署）/ marketplace（技能市场导入）。旧数据缺省
   *  时按 local 处理。 */
  origin?: 'builtin' | 'plugin' | 'local' | 'marketplace'
}

export interface SkillSyncPreview {
  target: string
  create: SkillItem[]
  update: SkillItem[]
  unchanged: SkillItem[]
  conflict: SkillItem[]
  /** 已发布、应装备到该 target，但 target 本地无副本的 Skill */
  missing: SkillItem[]
}

export interface SkillDocument {
  item: SkillItem
  content: string
}

/** 一份漂移对比文件：None 表示该侧不存在此文件或非 UTF-8 文本（二进制）。 */
export interface SkillDriftFile {
  path: string
  shared_text: string | null
  local_text: string | null
}

/** 已装备 Agent 在某个已发布 Skill 上的本地同步状态。 */
export interface SkillDriftAgent {
  agent: string
  state: 'in_sync' | 'modified' | 'missing'
  changed_files: number
}

/** 一个已发布 Skill 在全部已装备 Agent 上的漂移总览（「已发布」页仲裁数据）。 */
export interface SkillPublishedDrift {
  source: string
  name: string
  agents: SkillDriftAgent[]
}

/** 采纳 Agent 本地版本后，向其他 Agent 同步的结果。 */
export interface SkillAdoptResult {
  item: SkillItem
  adopted_agent: string
  /** 成功用新共享版本覆盖的 Agent（本地此前无修改）。 */
  synced: string[]
  /** 因自身本地副本也有修改而被跳过、未覆盖的 Agent。 */
  skipped: string[]
}

export interface MarketplaceSkill {
  id: string
  source: 'openai' | 'anthropic'
  source_label: string
  name: string
  description: string
  repository_url: string
  skill_url: string
  revision: string
  files: string[]
}

export interface MarketplaceCatalog {
  items: MarketplaceSkill[]
  fetched_at: string
  from_cache: boolean
  warning: string | null
}

export interface MarketplaceSkillPreview {
  item: MarketplaceSkill
  content: string
}
