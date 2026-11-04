import type { AgentSourceInfo } from '../types/agentSources'
import type { HookStatus } from '../types/memory'

/** 采集 Hook 监听的事件；命中任意一个即视为该来源正在采集。Gemini CLI 的
 * BeforeAgent 与 Antigravity 的 PreInvocation 是 UserPromptSubmit 的等价事件。 */
const CAPTURE_HOOK_EVENTS = ['UserPromptSubmit', 'PostToolUse', 'Stop', 'BeforeAgent', 'PreInvocation', 'AfterAgent']
/** 各 Agent 的“每轮提问”注入事件；与会话启动事件齐备才会在提问时注入记忆。 */
const PER_TURN_INJECTION_EVENT: Record<string, string> = {
  gemini: 'BeforeAgent',
  antigravity: 'PreInvocation',
}

/**
 * 某个 Agent 来源是否已经具备采集能力：装了采集 Hook，或开启了本地转录
 * 扫描且目录真实存在。记忆中心概览与「记忆采集」页共用同一判定，避免两处
 * 数字各算一套。
 */
export function isCaptureReady(source: AgentSourceInfo, hook: HookStatus | null): boolean {
  const hookCapturing = source.supports_hooks
    && (hook?.events ?? []).some((event) => CAPTURE_HOOK_EVENTS.includes(event))
  const scanning = source.scan_enabled && source.transcript_roots.some((root) => root.exists)
  return hookCapturing || scanning
}

/** 某个 Agent 是否已经开启记忆注入：Hook 事件齐备，或（DeepSeek Harness）
 * 注入插件在位——pre-step 插件同时覆盖会话启动与每轮提问两类注入。
 * Gemini CLI / Antigravity 的每轮提问事件名不同，按 agent_type 映射。 */
export function isInjectionReady(hook: HookStatus | null): boolean {
  const events = hook?.events ?? []
  if (events.includes('agent/pre-step')) return hook?.installed === true
  const perTurn = PER_TURN_INJECTION_EVENT[hook?.agent_type ?? ''] ?? 'UserPromptSubmit'
  return events.includes('SessionStart') && events.includes(perTurn)
}
