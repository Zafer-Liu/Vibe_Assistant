export type SourceFilter = 'all' | 'published' | 'codex' | 'claude' | 'qoder' | 'workbuddy' | 'minimax' | 'kimi' | 'copilot' | 'zcode' | 'dsh'

export const SOURCES: { id: Exclude<SourceFilter, 'all'>; label: string; color: string }[] = [
  { id: 'codex',    label: 'Codex',       color: 'bg-emerald-500' },
  { id: 'claude',   label: 'Claude Code', color: 'bg-amber-500' },
  { id: 'qoder',    label: 'Qoder',       color: 'bg-sky-500' },
  { id: 'workbuddy', label: 'WorkBuddy',  color: 'bg-rose-500' },
  { id: 'minimax',  label: 'MiniMax Code', color: 'bg-indigo-500' },
  { id: 'kimi',     label: 'Kimi',        color: 'bg-cyan-500' },
  { id: 'copilot',  label: 'GitHub Copilot', color: 'bg-slate-500' },
  { id: 'zcode',    label: 'ZCode',       color: 'bg-fuchsia-500' },
  { id: 'dsh',      label: 'DeepSeek Harness', color: 'bg-teal-500' },
]

export const sourceColor = (source: string) => SOURCES.find(s => s.id === source)?.color ?? 'bg-gray-400'
