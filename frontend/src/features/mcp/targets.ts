export const TARGETS = [
  { id: 'claude_cli', label: 'Claude Code', color: 'bg-amber-500', remote: true },
  { id: 'claude_desktop', label: 'Claude Desktop', color: 'bg-orange-500', remote: true },
  { id: 'codex_cli', label: 'Codex CLI', color: 'bg-emerald-500', remote: false },
  { id: 'codex_desktop', label: 'Codex Desktop', color: 'bg-teal-500', remote: false },
  { id: 'qoder', label: 'Qoder', color: 'bg-sky-500', remote: false },
  { id: 'workbuddy', label: 'WorkBuddy', color: 'bg-rose-500', remote: false },
  { id: 'minimax', label: 'MiniMax Code', color: 'bg-indigo-500', remote: false },
  { id: 'kimi', label: 'Kimi', color: 'bg-cyan-500', remote: false },
  { id: 'zcode', label: 'ZCode', color: 'bg-fuchsia-500', remote: false },
  { id: 'dsh', label: 'DeepSeek Harness', color: 'bg-teal-600', remote: true },
] as const

export function targetOf(id: string) {
  return TARGETS.find((t) => t.id === id)
}
