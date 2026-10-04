import { useEffect, useState, useCallback } from 'react'
import { useTranslation } from 'react-i18next'
import { invoke } from '@tauri-apps/api/core'
import {
  Globe, Plus, Trash2, Eye, EyeOff, RefreshCw,
  CheckCircle, XCircle, Play, Square, Copy, ChevronDown,
  ChevronUp, Shield, Server, Users, AlertTriangle,
  Link, Wifi, WifiOff, Loader2, ExternalLink,
} from 'lucide-react'
import type { AgentState } from '../types/agent'
import { useVisiblePolling } from '../hooks/useVisiblePolling'
import { ErrorRecovery } from '../components/ErrorRecovery'

// ── Caddy 类型 ───────────────────────────────────────────────

interface ProxyUser { username: string; password_hash: string }
interface ProxyRule {
  id: string; domain: string; target_port: number
  agent_name: string; https: boolean; allowed_users: string[]
}
interface ProxyConfig {
  users: ProxyUser[]; rules: ProxyRule[]
  caddy_path: string | null; admin_port: number | null
}

// ── 隧道状态 ─────────────────────────────────────────────────

type TunnelStatus = 'idle' | 'connecting' | 'active' | 'error'

interface TunnelState {
  status: TunnelStatus
  url: string
  error: string
}

// ── 工具 ─────────────────────────────────────────────────────

function genId() { return Math.random().toString(36).slice(2, 10) }

function copyText(text: string) {
  navigator.clipboard.writeText(text).catch(() => {
    const el = document.createElement('textarea')
    el.value = text
    document.body.appendChild(el)
    el.select()
    document.execCommand('copy')
    document.body.removeChild(el)
  })
}

// ════════════════════════════════════════════════════════════
// 临时分享 Section（Cloudflare Tunnel）
// ════════════════════════════════════════════════════════════

function TunnelSection({ agents }: { agents: AgentState[] }) {
  const { t } = useTranslation()
  const [cloudflaredPath, setCloudflaredPath] = useState<string | null>(null)
  const [tunnels, setTunnels] = useState<Record<string, TunnelState>>({})
  const [copied, setCopied] = useState<string | null>(null)
  useEffect(() => {
    let disposed = false
    void invoke<string | null>('tunnel_check_cloudflared').then(path => {
      if (!disposed) setCloudflaredPath(path)
    }).catch(() => {})
    void invoke<Record<string, string>>('tunnel_list').then(existing => {
      if (disposed) return
      const states: Record<string, TunnelState> = {}
      for (const [id, url] of Object.entries(existing)) {
        states[id] = { status: 'active', url, error: '' }
      }
      // A delayed initial snapshot must not overwrite a user start/stop action.
      setTunnels(prev => ({ ...states, ...prev }))
    }).catch(() => {})
    return () => { disposed = true }
  }, [])

  // Only observe active tunnels; tunnel_start owns connecting/error/timeout state.
  const refreshTunnelStatus = useCallback(async () => {
    const active = Object.entries(tunnels).filter(([, tunnel]) => tunnel.status === 'active')
    await Promise.allSettled(active.map(async ([id, tunnel]) => {
      const alive = await invoke<boolean>('tunnel_alive', { agentId: id })
      if (!alive) {
        setTunnels(prev => {
          // Ignore a result for a tunnel that has since been stopped or restarted.
          if (prev[id] !== tunnel) return prev
          return { ...prev, [id]: { status: 'idle', url: '', error: t('proxy.tunnelDisconnected') } }
        })
      }
    }))
  }, [t, tunnels])
  useVisiblePolling(refreshTunnelStatus, 8000)

  const activeAgents = agents.filter(a => a.config.port)
  const canStart = !!cloudflaredPath

  async function startTunnel(agentId: string) {
    const port = agents.find(a => a.config.id === agentId)?.config.port
    if (!port) return
    setTunnels(prev => ({ ...prev, [agentId]: { status: 'connecting', url: '', error: '' } }))
    try {
      const url = await invoke<string>('tunnel_start', { agentId, port })
      setTunnels(prev => ({ ...prev, [agentId]: { status: 'active', url, error: '' } }))
    } catch (e: unknown) {
      setTunnels(prev => ({ ...prev, [agentId]: { status: 'error', url: '', error: String(e) } }))
    }
  }

  async function stopTunnel(agentId: string) {
    await invoke('tunnel_stop', { agentId })
    setTunnels(prev => ({ ...prev, [agentId]: { status: 'idle', url: '', error: '' } }))
  }

  function handleCopy(url: string, agentId: string) {
    copyText(url); setCopied(agentId); setTimeout(() => setCopied(null), 2000)
  }

  return (
    <section>
      <div className="flex items-center gap-2 mb-3">
        <Link className="h-4 w-4 text-blue-500" />
        <h2 className="text-sm font-semibold text-gray-700 dark:text-gray-300">{t('proxy.tunnelTitle')}</h2>
        <span className="rounded-full bg-blue-100 px-2 py-0.5 text-[10px] font-medium text-blue-600 dark:bg-blue-900/30 dark:text-blue-400">
          {t('proxy.recommended')}
        </span>
      </div>

      {/* 方案说明 */}
      <div className="mb-3 space-y-1.5">
        <p className="text-xs text-gray-400 dark:text-gray-500 leading-relaxed">
          {t('proxy.tunnelDesc')}
        </p>
        <p className="text-xs text-amber-500 dark:text-amber-400 leading-relaxed">
          {t('proxy.dnsHint')}
        </p>
      </div>

      {/* 未安装 cloudflared 提示 */}
      {!canStart && (
        <div className="mb-4 rounded-xl border border-amber-200 bg-amber-50 p-4 dark:border-amber-800/50 dark:bg-amber-900/20">
          <div className="flex items-start gap-3">
            <AlertTriangle className="mt-0.5 h-5 w-5 shrink-0 text-amber-500" />
            <div className="text-sm text-amber-800 dark:text-amber-300">
              <p className="font-medium mb-1">{t('proxy.noCloudflared')}</p>
              <div className="space-y-1 font-mono text-xs">
                <div><span className="text-amber-600 mr-2">Scoop:</span>
                  <code className="rounded bg-amber-100 px-2 py-0.5 dark:bg-amber-900/40 select-all">scoop install cloudflared</code>
                </div>
                <div><span className="text-amber-600 mr-2">winget:</span>
                  <code className="rounded bg-amber-100 px-2 py-0.5 dark:bg-amber-900/40 select-all">winget install Cloudflare.cloudflared</code>
                </div>
              </div>
              <button onClick={() => invoke<string | null>('tunnel_check_cloudflared').then(setCloudflaredPath)}
                className="mt-3 flex items-center gap-1 text-xs text-amber-600 hover:text-amber-700 dark:text-amber-400">
                <RefreshCw className="h-3 w-3" /> {t('proxy.recheckBtn')}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* 已安装 — 显示路径 */}
      {canStart && (
        <div className="mb-4 flex items-center gap-2 text-xs text-gray-400 dark:text-gray-500">
          <CheckCircle className="h-3.5 w-3.5 text-green-500 shrink-0" />
          <span className="font-mono truncate">{cloudflaredPath}</span>
        </div>
      )}

      {/* Agent 列表 */}
      {activeAgents.length === 0 ? (
        <div className="rounded-xl border border-gray-200 bg-white py-8 text-center text-sm text-gray-400 dark:border-gray-800 dark:bg-gray-900 dark:text-gray-600">
          {t('proxy.noPortAgents')}
        </div>
      ) : (
        <div className="rounded-xl border border-gray-200 bg-white dark:border-gray-800 dark:bg-gray-900 overflow-hidden">
          <div className="divide-y divide-gray-100 dark:divide-gray-800">
            {activeAgents.map(agent => {
              const tunnel = tunnels[agent.config.id] ?? { status: 'idle', url: '', error: '' }
              const isConnecting = tunnel.status === 'connecting'
              const isActive = tunnel.status === 'active'
              const isError = tunnel.status === 'error'

              return (
                <div key={agent.config.id} className="p-4">
                  <div className="flex items-center gap-3">
                    {/* 状态图标 */}
                    <div className={`flex h-8 w-8 shrink-0 items-center justify-center rounded-lg ${
                      isActive ? 'bg-green-100 dark:bg-green-900/30'
                      : isConnecting ? 'bg-blue-100 dark:bg-blue-900/30'
                      : isError ? 'bg-red-100 dark:bg-red-900/30'
                      : 'bg-gray-100 dark:bg-gray-800'
                    }`}>
                      {isConnecting
                        ? <Loader2 className="h-4 w-4 text-blue-500 animate-spin" />
                        : isActive ? <Wifi className="h-4 w-4 text-green-500" />
                        : isError ? <WifiOff className="h-4 w-4 text-red-500" />
                        : <Globe className="h-4 w-4 text-gray-400" />}
                    </div>

                    {/* Agent 信息 */}
                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-2">
                        <span className="text-sm font-medium text-gray-800 dark:text-gray-200 truncate">{agent.config.name}</span>
                        <span className="shrink-0 text-xs text-gray-400">:{agent.config.port}</span>
                        {agent.status === 'running'
                          ? <span className="shrink-0 rounded-full bg-green-100 px-1.5 py-0.5 text-[10px] font-medium text-green-600 dark:bg-green-900/30 dark:text-green-400">{t('proxy.statusRunning')}</span>
                          : <span className="shrink-0 rounded-full bg-gray-100 px-1.5 py-0.5 text-[10px] text-gray-400 dark:bg-gray-800 dark:text-gray-500">{t('proxy.statusStopped')}</span>}
                      </div>

                      {isActive && (
                        <div className="mt-1 flex items-center gap-2">
                          <span className="font-mono text-xs text-blue-600 dark:text-blue-400 truncate">{tunnel.url}</span>
                          <button onClick={() => handleCopy(tunnel.url, agent.config.id)}
                            className="shrink-0 flex items-center gap-1 rounded px-1.5 py-0.5 text-[10px] text-gray-400 hover:bg-gray-100 hover:text-gray-600 dark:hover:bg-gray-800 dark:hover:text-gray-300 transition-colors">
                            {copied === agent.config.id
                              ? <><CheckCircle className="h-3 w-3 text-green-500" /> {t('common.copied')}</>
                              : <><Copy className="h-3 w-3" /> {t('common.copy')}</>}
                          </button>
                          <a href={tunnel.url} target="_blank" rel="noreferrer"
                            className="shrink-0 flex items-center gap-1 rounded px-1.5 py-0.5 text-[10px] text-gray-400 hover:bg-gray-100 hover:text-gray-600 dark:hover:bg-gray-800 dark:hover:text-gray-300">
                            <ExternalLink className="h-3 w-3" /> {t('common.open')}
                          </a>
                        </div>
                      )}

                      {isConnecting && (
                        <p className="mt-1 text-xs text-blue-500 dark:text-blue-400 animate-pulse">
                          {t('proxy.generating')}
                        </p>
                      )}

                      {isError && (
                        <div role="alert" className="mt-2 rounded-lg bg-red-50 px-3 py-2 text-[11px] leading-relaxed text-red-600 dark:bg-red-900/30 dark:text-red-400">
                          <pre className="max-h-48 overflow-auto whitespace-pre-wrap">{tunnel.error}</pre>
                          <ErrorRecovery error={tunnel.error} onRetry={() => { void startTunnel(agent.config.id) }} disabled={isConnecting} />
                        </div>
                      )}
                    </div>

                    {/* 操作按钮 */}
                    <div className="shrink-0">
                      {isActive ? (
                        <button onClick={() => stopTunnel(agent.config.id)}
                          className="flex items-center gap-1.5 rounded-lg border border-red-200 px-3 py-1.5 text-xs font-medium text-red-600 hover:bg-red-50 dark:border-red-800 dark:text-red-400 dark:hover:bg-red-900/20 transition-colors">
                          <Square className="h-3.5 w-3.5" /> {t('common.close')}
                        </button>
                      ) : (
                        <button onClick={() => startTunnel(agent.config.id)}
                          disabled={isConnecting || !canStart}
                          className="flex items-center gap-1.5 rounded-lg bg-blue-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-blue-500 disabled:opacity-40 transition-colors">
                          {isConnecting
                            ? <><Loader2 className="h-3.5 w-3.5 animate-spin" /> {t('proxy.connecting')}</>
                            : <><Play className="h-3.5 w-3.5" /> {t('proxy.generateLink')}</>}
                        </button>
                      )}
                    </div>
                  </div>
                </div>
              )
            })}
          </div>
        </div>
      )}

      {/* 使用说明 */}
      <div className="mt-3 rounded-lg bg-gray-50 p-3 dark:bg-gray-900/50 text-xs text-gray-400 dark:text-gray-500 leading-relaxed">
        <span className="font-medium text-gray-500 dark:text-gray-400">{t('proxy.tunnelNoteLabel')}</span>
        {t('proxy.tunnelNote')}
      </div>
    </section>
  )
}

// ════════════════════════════════════════════════════════════
// 主组件 ProxyManager
// ════════════════════════════════════════════════════════════

interface Props { agents: AgentState[] }

export function ProxyManager({ agents }: Props) {
  const { t } = useTranslation()
  const [config, setConfig] = useState<ProxyConfig>({
    users: [], rules: [], caddy_path: null, admin_port: null,
  })
  const [caddyPath, setCaddyPath] = useState<string | null>(null)
  const [isRunning, setIsRunning] = useState(false)
  const [busy, setBusy] = useState(false)
  const [msg, setMsg] = useState<{ text: string; ok: boolean } | null>(null)
  const [caddyfilePreview, setCaddyfilePreview] = useState('')
  const [showPreview, setShowPreview] = useState(false)
  const [showCaddy, setShowCaddy] = useState(false)   // Caddy section 折叠

  // 用户表单
  const [newUsername, setNewUsername] = useState('')
  const [newPassword, setNewPassword] = useState('')
  const [showPwd, setShowPwd] = useState(false)
  const [addingUser, setAddingUser] = useState(false)

  // 规则表单
  const [showRuleForm, setShowRuleForm] = useState(false)
  const [ruleForm, setRuleForm] = useState<Partial<ProxyRule>>({ https: false, allowed_users: [] })

  const refresh = useCallback(async () => {
    const [cfg, caddy, running] = await Promise.all([
      invoke<ProxyConfig>('proxy_get_config'),
      invoke<string | null>('proxy_check_caddy'),
      invoke<boolean>('proxy_status'),
    ])
    setConfig(cfg); setCaddyPath(caddy); setIsRunning(running)
  }, [])

  // Load configuration once; polling must not overwrite in-progress form edits.
  // proxy_status is fetched by the hook, avoiding a duplicate initial status read.
  useEffect(() => {
    let disposed = false
    void Promise.all([
      invoke<ProxyConfig>('proxy_get_config'),
      invoke<string | null>('proxy_check_caddy'),
    ]).then(([cfg, caddy]) => {
      if (!disposed) { setConfig(cfg); setCaddyPath(caddy) }
    }).catch(() => {})
    return () => { disposed = true }
  }, [])

  const refreshProxyStatus = useCallback(async () => {
    setIsRunning(await invoke<boolean>('proxy_status'))
  }, [])
  useVisiblePolling(refreshProxyStatus, 5000)

  async function addUser() {
    if (!newUsername.trim() || !newPassword.trim()) return
    if (!caddyPath) { flash(t('proxy.installCaddyFirst'), false); return }
    setAddingUser(true)
    try {
      const hash = await invoke<string>('proxy_hash_password', { caddyPath, plaintext: newPassword })
      const user: ProxyUser = { username: newUsername.trim(), password_hash: hash }
      const updated: ProxyConfig = { ...config, users: [...config.users.filter(u => u.username !== user.username), user] }
      await invoke('proxy_save_config', { config: updated })
      setConfig(updated); setNewUsername(''); setNewPassword('')
      flash(t('proxy.userAdded', { name: user.username }), true)
    } catch (e: unknown) { flash(String(e), false) }
    finally { setAddingUser(false) }
  }

  async function removeUser(username: string) {
    const updated: ProxyConfig = {
      ...config,
      users: config.users.filter(u => u.username !== username),
      rules: config.rules.map(r => ({ ...r, allowed_users: r.allowed_users.filter(u => u !== username) })),
    }
    await invoke('proxy_save_config', { config: updated }); setConfig(updated)
  }

  async function addRule() {
    if (!ruleForm.domain || !ruleForm.target_port) return
    const rule: ProxyRule = {
      id: genId(), domain: ruleForm.domain, target_port: ruleForm.target_port,
      agent_name: ruleForm.agent_name ?? '', https: ruleForm.https ?? false,
      allowed_users: ruleForm.allowed_users ?? [],
    }
    const updated: ProxyConfig = { ...config, rules: [...config.rules, rule] }
    await invoke('proxy_save_config', { config: updated }); setConfig(updated)
    setShowRuleForm(false); setRuleForm({ https: false, allowed_users: [] })
  }

  async function removeRule(id: string) {
    const updated: ProxyConfig = { ...config, rules: config.rules.filter(r => r.id !== id) }
    await invoke('proxy_save_config', { config: updated }); setConfig(updated)
  }

  async function toggleUserInRule(ruleId: string, username: string) {
    const updated: ProxyConfig = {
      ...config,
      rules: config.rules.map(r => {
        if (r.id !== ruleId) return r
        const has = r.allowed_users.includes(username)
        return { ...r, allowed_users: has ? r.allowed_users.filter(u => u !== username) : [...r.allowed_users, username] }
      }),
    }
    await invoke('proxy_save_config', { config: updated }); setConfig(updated)
  }

  async function applyProxy() {
    if (!caddyPath) { flash(t('proxy.caddyNotFound'), false); return }
    setBusy(true)
    try { flash(await invoke<string>('proxy_apply', { config }), true); setIsRunning(true) }
    catch (e: unknown) { flash(String(e), false) }
    finally { setBusy(false) }
  }

  async function stopProxy() {
    setBusy(true)
    try { flash(await invoke<string>('proxy_stop'), true); setIsRunning(false) }
    catch (e: unknown) { flash(String(e), false) }
    finally { setBusy(false) }
  }

  async function loadPreview() {
    setCaddyfilePreview(await invoke<string>('proxy_preview_caddyfile', { config }))
    setShowPreview(true)
  }

  function flash(text: string, ok: boolean) {
    setMsg({ text, ok }); setTimeout(() => setMsg(null), 4000)
  }

  return (
    <div className="flex h-full flex-col overflow-hidden bg-gray-50 dark:bg-gray-950">

      {/* 顶部标题栏 */}
      <div className="flex items-center gap-3 border-b border-gray-200 bg-white px-6 py-4 dark:border-gray-800 dark:bg-gray-900">
        <div className="flex h-8 w-8 items-center justify-center rounded-lg bg-blue-100 dark:bg-blue-900/40">
          <Globe className="h-4 w-4 text-blue-600 dark:text-blue-400" />
        </div>
        <div>
          <h1 className="text-sm font-semibold text-gray-900 dark:text-gray-100">{t('proxy.title')}</h1>
          <p className="text-xs text-gray-400 dark:text-gray-500">{t('proxy.subtitle')}</p>
        </div>
      </div>

      {/* 消息提示 */}
      {msg && (
        <div role={msg.ok ? 'status' : 'alert'} className={`mx-6 mt-4 flex items-start gap-2 rounded-lg px-4 py-2.5 text-sm ${
          msg.ok ? 'bg-green-50 text-green-700 dark:bg-green-900/30 dark:text-green-300'
                 : 'bg-red-50 text-red-700 dark:bg-red-900/30 dark:text-red-300'
        }`}>
          {msg.ok ? <CheckCircle className="mt-0.5 h-4 w-4 shrink-0" /> : <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />}
          <div className="min-w-0 flex-1">
            <p className="break-words">{msg.text}</p>
            {!msg.ok && <ErrorRecovery error={msg.text} />}
          </div>
        </div>
      )}

      <div className="flex-1 overflow-y-auto p-6 space-y-6">

        {/* ── 临时分享（主功能）── */}
        <TunnelSection agents={agents} />

        {/* ── 分隔线 ── */}
        <div className="flex items-center gap-3">
          <div className="flex-1 border-t border-gray-200 dark:border-gray-800" />
          <span className="text-xs text-gray-400">{t('proxy.advanced')}</span>
          <div className="flex-1 border-t border-gray-200 dark:border-gray-800" />
        </div>

        {/* ── Caddy 反向代理（折叠）── */}
        <section>
          <button
            onClick={() => setShowCaddy(v => !v)}
            className="flex w-full items-center gap-2 rounded-xl border border-gray-200 bg-white px-4 py-3 text-sm text-gray-600 hover:bg-gray-50 dark:border-gray-800 dark:bg-gray-900 dark:text-gray-400 dark:hover:bg-gray-800/50"
          >
            <Shield className="h-4 w-4 text-gray-400" />
            <span className="flex-1 text-left font-medium">{t('proxy.caddyTitle')}</span>
            <span className="text-xs text-gray-400">{t('proxy.caddySubtitle')}</span>
            {showCaddy ? <ChevronUp className="h-4 w-4" /> : <ChevronDown className="h-4 w-4" />}
          </button>

          {showCaddy && (
            <div className="mt-3 space-y-4">

              {/* Caddy 状态 */}
              <div className="flex items-center justify-between">
                <div className={`flex items-center gap-1.5 rounded-full px-3 py-1 text-xs font-medium ${
                  !caddyPath ? 'bg-red-50 text-red-600 dark:bg-red-900/30 dark:text-red-400'
                  : isRunning ? 'bg-green-50 text-green-600 dark:bg-green-900/30 dark:text-green-400'
                  : 'bg-gray-100 text-gray-500 dark:bg-gray-800 dark:text-gray-400'
                }`}>
                  {!caddyPath ? <><XCircle className="h-3 w-3" /> {t('proxy.caddyNotInstalled')}</>
                  : isRunning ? <><CheckCircle className="h-3 w-3" /> {t('proxy.statusRunning')}</>
                  : <><Square className="h-3 w-3" /> {t('proxy.statusStopped')}</>}
                </div>
                <button onClick={refresh} className="rounded p-1.5 text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-800">
                  <RefreshCw className="h-4 w-4" />
                </button>
              </div>

              {/* 未安装提示 */}
              {!caddyPath && (
                <div className="rounded-xl border border-amber-200 bg-amber-50 p-4 dark:border-amber-800/50 dark:bg-amber-900/20">
                  <div className="flex items-start gap-3">
                    <AlertTriangle className="mt-0.5 h-5 w-5 shrink-0 text-amber-500" />
                    <div className="text-xs text-amber-800 dark:text-amber-300 space-y-1">
                      <p className="font-medium">{t('proxy.caddyNotInstalled')}</p>
                      <div className="font-mono space-y-1">
                        <div><span className="text-amber-600">Windows: </span><code className="bg-amber-100 rounded px-1 dark:bg-amber-900/40">scoop install caddy</code></div>
                        <div><span className="text-amber-600">Mac: </span><code className="bg-amber-100 rounded px-1 dark:bg-amber-900/40">brew install caddy</code></div>
                        <div><span className="text-amber-600">Linux: </span><code className="bg-amber-100 rounded px-1 dark:bg-amber-900/40">apt install caddy</code></div>
                      </div>
                    </div>
                  </div>
                </div>
              )}

              {/* 用户管理 */}
              <div>
                <div className="flex items-center gap-2 mb-2">
                  <Users className="h-3.5 w-3.5 text-gray-400" />
                  <h3 className="text-xs font-semibold text-gray-600 dark:text-gray-400">{t('proxy.accessUsers')}</h3>
                </div>
                <div className="rounded-xl border border-gray-200 bg-white dark:border-gray-800 dark:bg-gray-900 overflow-hidden">
                  {config.users.map(user => (
                    <div key={user.username} className="flex items-center gap-3 px-4 py-2.5 border-b border-gray-100 dark:border-gray-800 last:border-0">
                      <div className="flex h-6 w-6 items-center justify-center rounded-full bg-blue-100 text-[10px] font-medium text-blue-600 dark:bg-blue-900/40 dark:text-blue-400 shrink-0">
                        {user.username[0]?.toUpperCase()}
                      </div>
                      <span className="flex-1 text-sm text-gray-700 dark:text-gray-300">{user.username}</span>
                      <button onClick={() => removeUser(user.username)} className="rounded p-1 text-gray-300 hover:text-red-500 dark:text-gray-700 dark:hover:text-red-400">
                        <Trash2 className="h-3.5 w-3.5" />
                      </button>
                    </div>
                  ))}
                  <div className="flex items-center gap-2 p-3 bg-gray-50 dark:bg-gray-900/50">
                    <input value={newUsername} onChange={e => setNewUsername(e.target.value)}
                      placeholder={t('proxy.username')} onKeyDown={e => e.key === 'Enter' && addUser()}
                      className="flex-1 rounded-lg border border-gray-200 bg-white px-3 py-1.5 text-sm outline-none focus:border-blue-400 dark:border-gray-700 dark:bg-gray-800 dark:text-gray-100" />
                    <div className="relative">
                      <input type={showPwd ? 'text' : 'password'} value={newPassword}
                        onChange={e => setNewPassword(e.target.value)} placeholder={t('proxy.password')}
                        onKeyDown={e => e.key === 'Enter' && addUser()}
                        className="w-32 rounded-lg border border-gray-200 bg-white px-3 py-1.5 pr-8 text-sm outline-none focus:border-blue-400 dark:border-gray-700 dark:bg-gray-800 dark:text-gray-100" />
                      <button onClick={() => setShowPwd(v => !v)} className="absolute right-2 top-1/2 -translate-y-1/2 text-gray-400">
                        {showPwd ? <EyeOff className="h-3.5 w-3.5" /> : <Eye className="h-3.5 w-3.5" />}
                      </button>
                    </div>
                    <button onClick={addUser} disabled={addingUser || !newUsername.trim() || !newPassword.trim() || !caddyPath}
                      className="flex items-center gap-1 rounded-lg bg-blue-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-blue-500 disabled:opacity-40">
                      {addingUser ? <RefreshCw className="h-3.5 w-3.5 animate-spin" /> : <Plus className="h-3.5 w-3.5" />} {t('common.add')}
                    </button>
                  </div>
                  {config.users.length === 0 && (
                    <p className="py-4 text-center text-xs text-gray-400 dark:text-gray-600">{t('proxy.noUsers')}</p>
                  )}
                </div>
              </div>

              {/* 代理规则 */}
              <div>
                <div className="flex items-center gap-2 mb-2">
                  <Server className="h-3.5 w-3.5 text-gray-400" />
                  <h3 className="text-xs font-semibold text-gray-600 dark:text-gray-400">{t('proxy.proxyRules')}</h3>
                  <button onClick={() => setShowRuleForm(v => !v)}
                    className="ml-auto flex items-center gap-1 rounded-lg border border-gray-200 px-2 py-0.5 text-xs text-gray-500 hover:bg-gray-50 dark:border-gray-700 dark:hover:bg-gray-800">
                    <Plus className="h-3 w-3" /> {t('common.add')}
                  </button>
                </div>

                {showRuleForm && (
                  <div className="mb-2 rounded-xl border border-blue-200 bg-blue-50 p-3 dark:border-blue-800/50 dark:bg-blue-900/20 space-y-2">
                    <div className="grid grid-cols-2 gap-2">
                      <div>
                        <label className="block text-[10px] text-gray-500 mb-1">{t('proxy.domain')}</label>
                        <input value={ruleForm.domain ?? ''} onChange={e => setRuleForm(f => ({ ...f, domain: e.target.value }))}
                          placeholder={t('proxy.domainPlaceholder')}
                          className="w-full rounded-lg border border-gray-200 bg-white px-2 py-1.5 text-xs outline-none focus:border-blue-400 dark:border-gray-700 dark:bg-gray-800 dark:text-gray-100" />
                      </div>
                      <div>
                        <label className="block text-[10px] text-gray-500 mb-1">{t('proxy.forwardTo')}</label>
                        <select value={ruleForm.target_port ?? ''}
                          onChange={e => {
                            const agent = agents.find(a => String(a.config.port) === e.target.value)
                            setRuleForm(f => ({ ...f, target_port: Number(e.target.value), agent_name: agent?.config.name ?? '' }))
                          }}
                          className="w-full rounded-lg border border-gray-200 bg-white px-2 py-1.5 text-xs outline-none focus:border-blue-400 dark:border-gray-700 dark:bg-gray-800 dark:text-gray-100">
                          <option value="">{t('proxy.selectAgent')}</option>
                          {agents.filter(a => a.config.port).map(a => (
                            <option key={a.config.id} value={a.config.port!}>{a.config.name} (:{a.config.port})</option>
                          ))}
                        </select>
                      </div>
                    </div>
                    <label className="flex items-center gap-2 text-xs text-gray-600 dark:text-gray-400 cursor-pointer">
                      <input type="checkbox" checked={ruleForm.https ?? false}
                        onChange={e => setRuleForm(f => ({ ...f, https: e.target.checked }))} className="accent-blue-500" />
                      {t('proxy.enableHttps')}
                    </label>
                    <div className="flex justify-end gap-2">
                      <button onClick={() => { setShowRuleForm(false); setRuleForm({ https: false, allowed_users: [] }) }}
                        className="rounded px-3 py-1 text-xs text-gray-500 hover:bg-gray-100 dark:hover:bg-gray-800">{t('common.cancel')}</button>
                      <button onClick={addRule} disabled={!ruleForm.domain || !ruleForm.target_port}
                        className="rounded-lg bg-blue-600 px-3 py-1 text-xs font-medium text-white hover:bg-blue-500 disabled:opacity-40">{t('common.save')}</button>
                    </div>
                  </div>
                )}

                <div className="rounded-xl border border-gray-200 bg-white dark:border-gray-800 dark:bg-gray-900 overflow-hidden">
                  {config.rules.length === 0 ? (
                    <p className="py-6 text-center text-xs text-gray-400 dark:text-gray-600">{t('proxy.noRules')}</p>
                  ) : config.rules.map(rule => (
                    <div key={rule.id} className="flex items-center gap-3 px-4 py-3 border-b border-gray-100 dark:border-gray-800 last:border-0">
                      <Globe className="h-4 w-4 text-green-500 shrink-0" />
                      <div className="flex-1 min-w-0">
                        <span className="font-mono text-xs text-gray-700 dark:text-gray-300">
                          {rule.https ? 'https://' : 'http://'}{rule.domain}
                        </span>
                        <span className="ml-2 text-xs text-gray-400">→ :{rule.target_port} {rule.agent_name && `(${rule.agent_name})`}</span>
                      </div>
                      {/* 用户权限切换 */}
                      {config.users.length > 0 && (
                        <div className="flex gap-1">
                          {config.users.map(u => (
                            <button key={u.username} onClick={() => toggleUserInRule(rule.id, u.username)}
                              className={`rounded-full px-2 py-0.5 text-[10px] font-medium transition-colors ${
                                rule.allowed_users.length === 0 || rule.allowed_users.includes(u.username)
                                  ? 'bg-blue-100 text-blue-600 dark:bg-blue-900/30 dark:text-blue-400'
                                  : 'bg-gray-100 text-gray-400 dark:bg-gray-800 dark:text-gray-600'
                              }`}>
                              {u.username}
                            </button>
                          ))}
                        </div>
                      )}
                      <button onClick={() => removeRule(rule.id)} className="rounded p-1 text-gray-300 hover:text-red-500 dark:text-gray-700 dark:hover:text-red-400">
                        <Trash2 className="h-3.5 w-3.5" />
                      </button>
                    </div>
                  ))}
                </div>
              </div>

              {/* Caddyfile 预览 */}
              <button onClick={() => showPreview ? setShowPreview(false) : loadPreview()}
                className="flex items-center gap-1.5 text-xs text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
                <Shield className="h-3.5 w-3.5" />
                {showPreview ? t('proxy.hideCaddyfile') : t('proxy.previewCaddyfile')}
                {showPreview ? <ChevronUp className="h-3.5 w-3.5" /> : <ChevronDown className="h-3.5 w-3.5" />}
              </button>
              {showPreview && (
                <div className="rounded-xl border border-gray-200 bg-gray-900 dark:border-gray-700 overflow-hidden">
                  <div className="flex items-center justify-between px-4 py-2 border-b border-gray-700">
                    <span className="text-xs text-gray-400 font-mono">Caddyfile</span>
                    <button onClick={() => copyText(caddyfilePreview)} className="flex items-center gap-1 text-xs text-gray-400 hover:text-gray-200">
                      <Copy className="h-3 w-3" /> {t('common.copy')}
                    </button>
                  </div>
                  <pre className="p-4 text-xs text-green-400 font-mono overflow-x-auto whitespace-pre">
                    {caddyfilePreview || t('proxy.caddyfileEmpty')}
                  </pre>
                </div>
              )}

              {/* Caddy 操作按钮 */}
              <div className="flex justify-end gap-2">
                {isRunning && (
                  <button onClick={stopProxy} disabled={busy}
                    className="flex items-center gap-1.5 rounded-lg border border-red-200 px-4 py-2 text-sm font-medium text-red-600 hover:bg-red-50 disabled:opacity-40 dark:border-red-800 dark:text-red-400 dark:hover:bg-red-900/20">
                    <Square className="h-4 w-4" /> {t('proxy.stopProxy')}
                  </button>
                )}
                <button onClick={applyProxy} disabled={busy || !caddyPath || config.rules.length === 0}
                  className="flex items-center gap-1.5 rounded-lg bg-blue-600 px-5 py-2 text-sm font-medium text-white hover:bg-blue-500 disabled:opacity-40">
                  {busy ? <RefreshCw className="h-4 w-4 animate-spin" /> : <Play className="h-4 w-4" />}
                  {isRunning ? t('proxy.reapply') : t('proxy.applyProxy')}
                </button>
              </div>

            </div>
          )}
        </section>

      </div>
    </div>
  )
}
