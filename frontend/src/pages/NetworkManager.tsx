import { useCallback, useEffect, useMemo, useState, type ReactNode } from 'react'
import { useTranslation } from 'react-i18next'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { RefreshCw, Download, Upload, Globe, Copy, Check, Wifi, AlertTriangle, Activity } from 'lucide-react'
import { useVisiblePolling } from '../hooks/useVisiblePolling'
import { ErrorRecovery } from '../components/ErrorRecovery'

// ── 后端 network_info.rs 的返回结构（字段为后端 snake_case 原样） ──

interface NetworkInterface {
  name: string
  mac: string | null
  mtu: number
  ipv4: string[]
  ipv6: string[]
  is_loopback: boolean
}

interface InterfaceThroughput {
  name: string
  is_loopback: boolean
  rx_bytes_per_sec: number
  tx_bytes_per_sec: number
  rx_total_bytes: number
  tx_total_bytes: number
}

interface ThroughputSnapshot {
  elapsed_ms: number
  interfaces: InterfaceThroughput[]
  total_rx_bytes_per_sec: number
  total_tx_bytes_per_sec: number
}

interface ProxyIp {
  server: string
  ipv4: string | null
  ipv6: string | null
}

interface PublicIp {
  ipv4: string | null
  ipv6: string | null
  proxy: ProxyIp | null
}

/** 单条链路（直连/代理）的测速结果。skipped 为跳过原因代码（no_proxy 等），null = 已正常测量。 */
interface SpeedTestModeResult {
  latency_ms: number | null
  download_bps: number | null
  upload_bps: number | null
  skipped: string | null
}

/** 一次测速的两条链路结果：直连 + 代理（代理不可用时 skipped 置位）。 */
interface SpeedTestResult {
  direct: SpeedTestModeResult
  proxy: SpeedTestModeResult
}

type SpeedTestPhase = 'latency' | 'download' | 'upload'

interface SpeedSample {
  rx: number
  tx: number
}

/** 速率曲线保留的采样点数：2 秒一轮 ≈ 近 2 分钟。 */
const HISTORY_LIMIT = 60

/** 速率值与单位拆开返回：大数字 + 小单位同一行展示，避免单位被挤到第二行。 */
function formatSpeedParts(bytesPerSec: number): { value: string; unit: string } {
  if (!Number.isFinite(bytesPerSec) || bytesPerSec <= 0) return { value: '0', unit: 'B/s' }
  const units = ['B/s', 'KB/s', 'MB/s', 'GB/s']
  let value = bytesPerSec
  let unit = 0
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024
    unit += 1
  }
  const text = unit === 0 || value >= 100 ? Math.round(value).toString() : value.toFixed(1)
  return { value: text, unit: units[unit] }
}

function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let value = bytes
  let unit = 0
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024
    unit += 1
  }
  const text = unit === 0 || value >= 100 ? Math.round(value).toString() : value.toFixed(1)
  return `${text} ${units[unit]}`
}

function formatMbps(bps: number | null | undefined): string {
  if (bps == null || !Number.isFinite(bps) || bps <= 0) return '—'
  return `${(bps / 1_000_000).toFixed(1)} Mbps`
}

function CopyButton({ value, label }: { value: string; label: string }) {
  const { t } = useTranslation()
  const [copied, setCopied] = useState(false)
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(value)
      setCopied(true)
      setTimeout(() => { setCopied(false) }, 2000)
    } catch {
      // 剪贴板不可用时静默失败，不打断浏览。
    }
  }
  return (
    <button
      type="button"
      onClick={copy}
      title={t('common.copy')}
      aria-label={`${t('common.copy')} ${label}`}
      className="rounded p-1 text-gray-400 transition-colors hover:bg-gray-100 hover:text-gray-600 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 dark:hover:bg-gray-800 dark:hover:text-gray-300"
    >
      {copied ? <Check className="h-3.5 w-3.5 text-emerald-500" /> : <Copy className="h-3.5 w-3.5" />}
    </button>
  )
}

function SpeedSparkline({ history }: { history: SpeedSample[] }) {
  const { t } = useTranslation()
  const width = 320
  const height = 48
  const pad = 3
  const max = Math.max(1, ...history.map(s => Math.max(s.rx, s.tx)))
  const toPoints = (pick: (s: SpeedSample) => number): Array<[number, number]> =>
    history.map((sample, index) => {
      const x = history.length <= 1
        ? 0
        : pad + (index / (history.length - 1)) * (width - pad * 2)
      const y = height - pad - (pick(sample) / max) * (height - pad * 2)
      return [x, y]
    })
  const rxPoints = toPoints(s => s.rx)
  const txPoints = toPoints(s => s.tx)
  const polyline = (points: Array<[number, number]>) =>
    points.map(([x, y]) => `${x.toFixed(1)},${y.toFixed(1)}`).join(' ')
  // 下载曲线下方铺一层淡色面积，让趋势更直观。
  const rxArea = rxPoints.length > 1
    ? `M ${rxPoints.map(([x, y]) => `${x.toFixed(1)},${y.toFixed(1)}`).join(' L ')} L ${rxPoints[rxPoints.length - 1][0].toFixed(1)},${height - pad} L ${rxPoints[0][0].toFixed(1)},${height - pad} Z`
    : ''

  return (
    <svg
      viewBox={`0 0 ${width} ${height}`}
      className="mt-3 h-12 w-full"
      role="img"
      aria-label={t('network.speedHint')}
      preserveAspectRatio="none"
    >
      <line x1={pad} y1={height - pad} x2={width - pad} y2={height - pad} className="stroke-gray-200 dark:stroke-gray-700" strokeWidth="1" />
      {rxArea && <path d={rxArea} className="fill-emerald-500/10" />}
      {rxPoints.length > 1 && <polyline points={polyline(rxPoints)} fill="none" className="stroke-emerald-500" strokeWidth="1.5" />}
      {txPoints.length > 1 && <polyline points={polyline(txPoints)} fill="none" className="stroke-sky-500" strokeWidth="1.5" />}
    </svg>
  )
}

function StatCard({ title, value, hint, icon }: { title: string; value: string; hint?: string; icon?: ReactNode }) {
  return (
    <div className="rounded-2xl border border-gray-200 bg-white p-4 dark:border-gray-700 dark:bg-gray-900">
      <div className="flex items-center gap-1.5 text-xs text-gray-500 dark:text-gray-400">
        {icon}{title}
      </div>
      <p className="mt-1.5 break-all font-mono text-xl font-semibold text-gray-900 dark:text-gray-100">{value}</p>
      {hint && <p className="mt-0.5 truncate text-xs text-gray-400 dark:text-gray-500">{hint}</p>}
    </div>
  )
}

/** 带宽数值：大数字 + 小单位固定一行，MB/s 换算放第二行小字。
 *  之前把「86.1 Mbps（10.8 MB/s）」拼成一个长串，窄列里会硬折成三行。 */
function SpeedStatValue({
  bps,
  unavailable,
  toneClass,
}: {
  bps: number | null
  unavailable: string
  toneClass: string
}) {
  if (bps == null || !Number.isFinite(bps) || bps <= 0) {
    return <p className={`mt-1 font-mono text-lg font-semibold ${toneClass}`}>{unavailable}</p>
  }
  return (
    <div>
      <p className={`mt-1 whitespace-nowrap font-mono text-lg font-semibold ${toneClass}`}>
        {(bps / 1_000_000).toFixed(1)}
        <span className="ml-1 font-sans text-[11px] font-normal text-gray-500 dark:text-gray-400">Mbps</span>
      </p>
      <p className="mt-0.5 whitespace-nowrap font-mono text-[11px] text-gray-400 dark:text-gray-500">
        {(bps / 8 / 1_000_000).toFixed(1)} MB/s
      </p>
    </div>
  )
}

/** 单条链路的测速结果卡：延迟 / 下行 / 上行三列；链路被跳过时给出一行说明。 */
function SpeedModeResultCard({ label, result }: { label: string; result: SpeedTestModeResult }) {
  const { t } = useTranslation()
  return (
    <div className="rounded-xl border border-gray-200 p-3.5 dark:border-gray-700">
      <p className="text-xs font-medium text-gray-500 dark:text-gray-400">{label}</p>
      {result.skipped ? (
        <p className="mt-2 text-sm text-gray-400 dark:text-gray-500">
          {result.skipped === 'no_proxy' ? t('network.proxySkipped') : t('network.modeSkipped')}
        </p>
      ) : (
        <div className="mt-2 grid grid-cols-3 gap-3">
          <div>
            <p className="flex items-center gap-1 text-[11px] text-gray-500 dark:text-gray-400"><Activity className="h-3 w-3" />{t('network.latency')}</p>
            <p className="mt-1 whitespace-nowrap font-mono text-lg font-semibold text-gray-900 dark:text-gray-100">
              {result.latency_ms != null
                ? (
                  <>
                    {Math.round(result.latency_ms)}
                    <span className="ml-1 font-sans text-[11px] font-normal text-gray-500 dark:text-gray-400">ms</span>
                  </>
                )
                : t('network.unavailable')}
            </p>
          </div>
          <div>
            <p className="flex items-center gap-1 text-[11px] text-gray-500 dark:text-gray-400"><Download className="h-3 w-3" />{t('network.downBps')}</p>
            <SpeedStatValue bps={result.download_bps} unavailable={t('network.unavailable')} toneClass="text-emerald-600 dark:text-emerald-400" />
          </div>
          <div>
            <p className="flex items-center gap-1 text-[11px] text-gray-500 dark:text-gray-400"><Upload className="h-3 w-3" />{t('network.upBps')}</p>
            <SpeedStatValue bps={result.upload_bps} unavailable={t('network.unavailable')} toneClass="text-sky-600 dark:text-sky-400" />
          </div>
        </div>
      )}
    </div>
  )
}

export function NetworkManager() {
  const { t } = useTranslation()
  const [snapshot, setSnapshot] = useState<ThroughputSnapshot | null>(null)
  const [interfaces, setInterfaces] = useState<NetworkInterface[]>([])
  const [history, setHistory] = useState<SpeedSample[]>([])
  const [error, setError] = useState('')
  const [publicIp, setPublicIp] = useState<PublicIp | null>(null)
  const [ipState, setIpState] = useState<'loading' | 'ready' | 'error'>('loading')
  const [ipError, setIpError] = useState('')
  const [ipNonce, setIpNonce] = useState(0)
  const [refreshing, setRefreshing] = useState(false)

  // ── 带宽测速（一次点击自动依次测直连 + 代理两条链路） ──
  const [testState, setTestState] = useState<'idle' | 'running' | 'done' | 'error'>('idle')
  const [testPhase, setTestPhase] = useState<SpeedTestPhase>('latency')
  const [activeMode, setActiveMode] = useState<'direct' | 'proxy'>('direct')
  const [liveBps, setLiveBps] = useState(0)
  const [testProgress, setTestProgress] = useState(0)
  const [testResult, setTestResult] = useState<SpeedTestResult | null>(null)
  const [testError, setTestError] = useState('')

  // 测速实时进度事件（后端每 500ms 上报当前链路与阶段速率）。
  useEffect(() => {
    let disposed = false
    let unlisten: (() => void) | undefined
    listen<{ mode: string; phase: string; current_bps: number; progress: number }>('network-speed-progress', (e) => {
      if (disposed) return
      const p = e.payload
      setTestProgress(p.progress)
      if (p.mode === 'direct' || p.mode === 'proxy') setActiveMode(p.mode)
      if (p.phase === 'latency' || p.phase === 'download' || p.phase === 'upload') {
        setTestPhase(p.phase)
        setLiveBps(p.current_bps)
      }
    }).then((fn) => {
      if (disposed) fn()
      else unlisten = fn
    })
    return () => { disposed = true; unlisten?.() }
  }, [])

  const startSpeedTest = useCallback(async () => {
    setTestState('running')
    setTestError('')
    setTestResult(null)
    setTestProgress(0)
    setLiveBps(0)
    setTestPhase('latency')
    setActiveMode('direct')
    try {
      const result = await invoke<SpeedTestResult>('network_speed_test')
      setTestResult(result)
      setTestState('done')
    } catch (cause) {
      setTestError(String(cause))
      setTestState('error')
    }
  }, [])

  const refreshThroughput = useCallback(async () => {
    try {
      const snap = await invoke<ThroughputSnapshot>('network_throughput')
      setSnapshot(snap)
      setError('')
      setHistory(prev => {
        const next = [...prev, { rx: snap.total_rx_bytes_per_sec, tx: snap.total_tx_bytes_per_sec }]
        return next.length > HISTORY_LIMIT ? next.slice(next.length - HISTORY_LIMIT) : next
      })
    } catch (cause) {
      setError(String(cause))
    }
  }, [])

  const refreshInterfaces = useCallback(async () => {
    try {
      const list = await invoke<NetworkInterface[]>('network_interfaces')
      setInterfaces(list)
      setError('')
    } catch (cause) {
      setError(String(cause))
    }
  }, [])

  const refreshAll = useCallback(async () => {
    setRefreshing(true)
    try {
      await Promise.all([refreshThroughput(), refreshInterfaces()])
    } finally {
      setRefreshing(false)
    }
  }, [refreshThroughput, refreshInterfaces])

  // 页面挂载即轮询（切换页面即卸载停止）；窗口最小化时由可见性门控暂停。
  useVisiblePolling(refreshThroughput, 2000)
  useVisiblePolling(refreshInterfaces, 10000)

  // 公网 IP 按需获取：进入页面一次 + 手动刷新，避免持续探测外网服务。
  useEffect(() => {
    let disposed = false
    setIpState('loading')
    setIpError('')
    invoke<PublicIp>('network_public_ip')
      .then(result => {
        if (disposed) return
        setPublicIp(result)
        setIpState('ready')
      })
      .catch(cause => {
        if (disposed) return
        setIpError(String(cause))
        setIpState('error')
      })
    return () => { disposed = true }
  }, [ipNonce])

  const throughputByName = useMemo(() => {
    const map = new Map<string, InterfaceThroughput>()
    for (const item of snapshot?.interfaces ?? []) map.set(item.name, item)
    return map
  }, [snapshot])

  // 本机主 IP：优先取第一个带 IPv4 的物理（非回环）接口。
  const primaryIp = useMemo(() => {
    const withIpv4 = interfaces.find(i => !i.is_loopback && i.ipv4.length > 0)
    if (withIpv4) return { ip: withIpv4.ipv4[0].split('/')[0], name: withIpv4.name }
    const withAny = interfaces.find(i => !i.is_loopback && i.ipv4.length + i.ipv6.length > 0)
    if (withAny) {
      const cidr = withAny.ipv6[0] ?? ''
      return { ip: cidr.split('/')[0], name: withAny.name }
    }
    return null
  }, [interfaces])

  const loading = snapshot === null && !error

  return (
    <div className="flex h-full flex-col bg-gray-50 dark:bg-gray-950">
      {/* Header */}
      <div className="border-b border-gray-200 bg-white px-6 py-4 dark:border-gray-700 dark:bg-gray-900">
        <div className="flex items-center justify-between">
          <div>
            <h2 className="text-base font-semibold text-gray-900 dark:text-gray-100">{t('network.title')}</h2>
            <p className="text-xs text-gray-500 dark:text-gray-400">{t('network.subtitle', { count: interfaces.length })}</p>
          </div>
          <button
            onClick={() => { void refreshAll() }}
            disabled={refreshing}
            className="flex items-center gap-1.5 rounded-lg border border-gray-200 px-3 py-1.5 text-sm text-gray-600 hover:bg-gray-50 disabled:opacity-40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 dark:border-gray-700 dark:text-gray-400 dark:hover:bg-gray-800"
          >
            <RefreshCw className={`h-4 w-4 ${refreshing ? 'animate-spin' : ''}`} />
            {t('network.refresh')}
          </button>
        </div>
      </div>

      <div className="flex-1 overflow-auto p-6">
        {error && (
          <div role="alert" className="mb-4 rounded-xl border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800 dark:border-amber-900/60 dark:bg-amber-950/30 dark:text-amber-300">
            <div className="flex items-center gap-1.5 font-medium">
              <AlertTriangle className="h-4 w-4" />{t('network.loadFailed')}
            </div>
            <p className="mt-1 max-h-24 overflow-auto break-words text-xs">{error}</p>
            <ErrorRecovery error={error} onRetry={() => { void refreshAll() }} disabled={refreshing} />
          </div>
        )}

        {/* 概览：实时网速 + 本机 IP + 公网 IP */}
        <div className="grid gap-4 lg:grid-cols-3">
          <div className="rounded-2xl border border-gray-200 bg-white p-4 dark:border-gray-700 dark:bg-gray-900">
            <div className="flex items-baseline justify-between">
              <p className="flex items-center gap-1.5 text-xs text-gray-500 dark:text-gray-400">
                <Wifi className="h-3.5 w-3.5" />{t('network.speedTitle')}
              </p>
              <p className="text-[11px] text-gray-400 dark:text-gray-500">
                <span className="mr-2 inline-flex items-center gap-1"><span className="inline-block h-0.5 w-3 rounded bg-emerald-500" />{t('network.download')}</span>
                <span className="inline-flex items-center gap-1"><span className="inline-block h-0.5 w-3 rounded bg-sky-500" />{t('network.upload')}</span>
              </p>
            </div>
            <div className="mt-2 flex flex-wrap items-baseline gap-x-6 gap-y-1">
              {loading ? (
                <p className="font-mono text-2xl font-semibold text-gray-400">—</p>
              ) : (
                <>
                  <p className="flex items-baseline gap-1 whitespace-nowrap font-mono text-2xl font-semibold text-emerald-600 dark:text-emerald-400">
                    <Download className="mr-0.5 inline h-4 w-4 self-center" />
                    {formatSpeedParts(snapshot?.total_rx_bytes_per_sec ?? 0).value}
                    <span className="font-sans text-xs font-normal text-gray-500 dark:text-gray-400">{formatSpeedParts(snapshot?.total_rx_bytes_per_sec ?? 0).unit}</span>
                  </p>
                  <p className="flex items-baseline gap-1 whitespace-nowrap font-mono text-2xl font-semibold text-sky-600 dark:text-sky-400">
                    <Upload className="mr-0.5 inline h-4 w-4 self-center" />
                    {formatSpeedParts(snapshot?.total_tx_bytes_per_sec ?? 0).value}
                    <span className="font-sans text-xs font-normal text-gray-500 dark:text-gray-400">{formatSpeedParts(snapshot?.total_tx_bytes_per_sec ?? 0).unit}</span>
                  </p>
                </>
              )}
            </div>
            <SpeedSparkline history={history} />
            <p className="mt-1 text-[11px] text-gray-400 dark:text-gray-500">{t('network.speedHint')}</p>
          </div>

          <StatCard
            title={t('network.localIp')}
            value={primaryIp ? primaryIp.ip : '—'}
            hint={primaryIp ? primaryIp.name : undefined}
            icon={<Globe className="h-3.5 w-3.5" />}
          />

          <div className="rounded-2xl border border-gray-200 bg-white p-4 dark:border-gray-700 dark:bg-gray-900">
            <div className="flex items-center justify-between">
              <p className="flex items-center gap-1.5 text-xs text-gray-500 dark:text-gray-400">
                <Globe className="h-3.5 w-3.5" />{t('network.publicIp')}
              </p>
              <button
                type="button"
                onClick={() => { setIpNonce(n => n + 1) }}
                disabled={ipState === 'loading'}
                title={t('network.refresh')}
                aria-label={t('network.refresh')}
                className="rounded p-1 text-gray-400 hover:bg-gray-100 hover:text-gray-600 disabled:opacity-40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 dark:hover:bg-gray-800 dark:hover:text-gray-300"
              >
                <RefreshCw className={`h-3.5 w-3.5 ${ipState === 'loading' ? 'animate-spin' : ''}`} />
              </button>
            </div>
            {ipState === 'loading' && <p className="mt-1.5 text-sm text-gray-400 dark:text-gray-500">{t('network.ipFetching')}</p>}
            {ipState === 'error' && (
              <div className="mt-1.5">
                <p className="text-sm text-amber-600 dark:text-amber-400">{t('network.ipFailed')}</p>
                <p className="mt-0.5 break-words text-xs text-gray-400 dark:text-gray-500">{ipError}</p>
              </div>
            )}
            {ipState === 'ready' && (
              <div className="mt-1.5 space-y-1.5">
                <div className="flex items-center justify-between gap-2">
                  <p className="break-all font-mono text-lg font-semibold text-gray-900 dark:text-gray-100">
                    <span className="mr-1.5 rounded bg-blue-50 px-1 py-0.5 font-sans text-[10px] font-medium text-blue-600 dark:bg-blue-900/30 dark:text-blue-400">{t('network.directIp')}·IPv4</span>
                    {publicIp?.ipv4 ?? <span className="text-sm font-normal text-gray-400">{t('network.ipUnavailable')}</span>}
                  </p>
                  {publicIp?.ipv4 && <CopyButton value={publicIp.ipv4} label={t('network.publicIpv4')} />}
                </div>
                <div className="flex items-center justify-between gap-2 border-t border-gray-100 pt-1.5 dark:border-gray-800">
                  <p className="break-all font-mono text-sm text-gray-600 dark:text-gray-300">
                    <span className="mr-1.5 rounded bg-purple-50 px-1 py-0.5 font-sans text-[10px] font-medium text-purple-600 dark:bg-purple-900/30 dark:text-purple-400">{t('network.directIp')}·IPv6</span>
                    {publicIp?.ipv6 ?? <span className="text-gray-400">{t('network.ipUnavailable')}</span>}
                  </p>
                  {publicIp?.ipv6 && <CopyButton value={publicIp.ipv6} label={t('network.publicIpv6')} />}
                </div>
                {publicIp?.proxy && (
                  <div className="flex items-center justify-between gap-2 border-t border-gray-100 pt-1.5 dark:border-gray-800">
                    <p className="break-all font-mono text-sm text-gray-600 dark:text-gray-300">
                      <span className="mr-1.5 rounded bg-blue-50 px-1 py-0.5 font-sans text-[10px] font-medium text-blue-600 dark:bg-blue-900/30 dark:text-blue-400">{t('network.proxyEgress')}·IPv4</span>
                      {publicIp.proxy.ipv4
                        ?? <span className="font-sans text-xs text-amber-500 dark:text-amber-400">{t('network.proxyUnreachable')}</span>}
                      <span className="ml-1.5 font-sans text-[11px] text-gray-400">({publicIp.proxy.server})</span>
                      {publicIp.proxy.ipv6 && (
                        <span className="ml-1.5 inline-flex items-center gap-1">
                          <span className="rounded bg-purple-50 px-1 py-0.5 font-sans text-[10px] font-medium text-purple-600 dark:bg-purple-900/30 dark:text-purple-400">IPv6</span>
                          <span>{publicIp.proxy.ipv6}</span>
                        </span>
                      )}
                    </p>
                    {publicIp.proxy.ipv4 && <CopyButton value={publicIp.proxy.ipv4} label={t('network.proxyEgress')} />}
                  </div>
                )}
                {!publicIp?.proxy && (
                  <p className="border-t border-gray-100 pt-1.5 text-[11px] text-gray-400 dark:border-gray-800 dark:text-gray-500">{t('network.noProxy')}</p>
                )}
                {!publicIp?.ipv4 && (
                  <p className="border-t border-gray-100 pt-1.5 text-[11px] text-amber-500 dark:border-gray-800 dark:text-amber-400">{t('network.directFailedHint')}</p>
                )}
              </div>
            )}
          </div>
        </div>

        {/* 网速测试：带宽测速（延迟/下行/上行），与上面的实时流量是两回事 */}
        <div className="mt-4 overflow-hidden rounded-2xl border border-gray-200 bg-white dark:border-gray-700 dark:bg-gray-900">
          <div className="flex items-center justify-between border-b border-gray-100 px-4 py-3 dark:border-gray-800">
            <div>
              <h3 className="text-sm font-semibold text-gray-900 dark:text-gray-100">{t('network.speedTest')}</h3>
              <p className="mt-0.5 text-xs text-gray-500 dark:text-gray-400">{t('network.speedTestDesc')}</p>
            </div>
            <div className="flex shrink-0 items-center gap-2">
              {/* 一次点击自动依次测直连与代理两条链路（测速仅手动触发，从不自动运行） */}
              <button
                type="button"
                onClick={() => { void startSpeedTest() }}
                disabled={testState === 'running'}
                className="flex shrink-0 items-center gap-1.5 rounded-lg bg-blue-600 px-3 py-1.5 text-sm font-medium text-white transition-colors hover:bg-blue-700 disabled:opacity-40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 dark:bg-blue-500 dark:hover:bg-blue-600"
              >
                {testState === 'running' && <RefreshCw className="h-4 w-4 animate-spin" />}
                {testState === 'running' ? t('network.testing') : t('network.startTest')}
              </button>
            </div>
          </div>

          <div className="px-4 py-4">
            {testState === 'idle' && (
              <p className="text-sm text-gray-500 dark:text-gray-400">{t('network.speedTestHint')}</p>
            )}

            {testState === 'running' && (
              <div>
                <p className="flex flex-wrap items-center gap-2 text-sm font-medium text-gray-700 dark:text-gray-200">
                  <span className="rounded bg-blue-50 px-1.5 py-0.5 text-[11px] font-medium text-blue-600 dark:bg-blue-900/30 dark:text-blue-400">
                    {activeMode === 'direct' ? t('network.speedTestModeDirect') : t('network.speedTestModeProxy')}
                  </span>
                  {testPhase === 'latency' && t('network.phaseLatency')}
                  {testPhase === 'download' && t('network.phaseDownload')}
                  {testPhase === 'upload' && t('network.phaseUpload')}
                </p>
                <p className="mt-1 font-mono text-2xl font-semibold text-gray-900 dark:text-gray-100">
                  {testPhase === 'latency' ? '…' : formatMbps(liveBps)}
                </p>
                <div className="mt-3 h-1.5 overflow-hidden rounded-full bg-gray-100 dark:bg-gray-800" role="progressbar" aria-valuenow={Math.round(testProgress * 100)} aria-valuemin={0} aria-valuemax={100}>
                  <div className="h-full rounded-full bg-blue-500 transition-all duration-300" style={{ width: `${Math.round(testProgress * 100)}%` }} />
                </div>
              </div>
            )}

            {testState === 'error' && (
              <div role="alert">
                <p className="text-sm font-medium text-red-600 dark:text-red-400">{t('network.testFailed')}</p>
                <p className="mt-1 max-h-24 overflow-auto break-words text-xs text-gray-500 dark:text-gray-400">{testError}</p>
                <div className="mt-2"><ErrorRecovery error={testError} onRetry={() => { void startSpeedTest() }} /></div>
              </div>
            )}

            {testState === 'done' && testResult && (
              <div className="grid gap-4 md:grid-cols-2">
                <SpeedModeResultCard label={t('network.speedTestModeDirect')} result={testResult.direct} />
                <SpeedModeResultCard label={t('network.speedTestModeProxy')} result={testResult.proxy} />
              </div>
            )}
          </div>
        </div>

        {/* 网卡接口列表 */}
        <div className="mt-4 overflow-hidden rounded-2xl border border-gray-200 bg-white dark:border-gray-700 dark:bg-gray-900">
          <div className="border-b border-gray-100 px-4 py-3 dark:border-gray-800">
            <h3 className="text-sm font-semibold text-gray-900 dark:text-gray-100">{t('network.interfaces')}</h3>
          </div>
          {interfaces.length === 0 ? (
            <p className="px-4 py-8 text-center text-sm text-gray-500 dark:text-gray-400">{t('network.noInterfaces')}</p>
          ) : (
            <ul className="divide-y divide-gray-100 dark:divide-gray-800">
              {interfaces.map(item => {
                const speed = throughputByName.get(item.name)
                const muted = item.is_loopback
                return (
                  <li key={item.name} className={`flex flex-wrap items-center gap-x-6 gap-y-2 px-4 py-3 ${muted ? 'opacity-60' : ''}`}>
                    <div className="min-w-0 flex-1">
                      <div className="flex flex-wrap items-center gap-2">
                        <Wifi className="h-3.5 w-3.5 shrink-0 text-gray-400" />
                        <span className="text-sm font-medium text-gray-900 dark:text-gray-100">{item.name}</span>
                        {item.is_loopback && (
                          <span className="rounded bg-gray-100 px-1.5 py-0.5 text-[10px] text-gray-500 dark:bg-gray-800 dark:text-gray-400">{t('network.loopback')}</span>
                        )}
                        {item.mac && <span className="font-mono text-[11px] text-gray-400 dark:text-gray-500">{item.mac}</span>}
                        {item.mtu > 0 && <span className="text-[11px] text-gray-400 dark:text-gray-500">MTU {item.mtu}</span>}
                      </div>
                      <div className="mt-1 flex flex-wrap gap-1.5">
                        {[...item.ipv4, ...item.ipv6].length === 0 ? (
                          <span className="text-xs text-gray-400 dark:text-gray-500">{t('network.noIp')}</span>
                        ) : (
                          <>
                            {item.ipv4.map(cidr => (
                              <span key={cidr} className="inline-flex items-center gap-1 rounded bg-blue-50 px-1.5 py-0.5 text-[10px] dark:bg-blue-900/30">
                                <span className="font-sans font-medium text-blue-600 dark:text-blue-400">IPv4</span>
                                <span className="font-mono text-blue-700 dark:text-blue-300">{cidr}</span>
                              </span>
                            ))}
                            {item.ipv6.map(cidr => (
                              <span key={cidr} className="inline-flex items-center gap-1 rounded bg-purple-50 px-1.5 py-0.5 text-[10px] dark:bg-purple-900/30">
                                <span className="font-sans font-medium text-purple-600 dark:text-purple-400">IPv6</span>
                                <span className="font-mono text-purple-700 dark:text-purple-300">{cidr}</span>
                              </span>
                            ))}
                          </>
                        )}
                      </div>
                    </div>
                    {/* 实时速率集中在「实时流量」卡展示，这里只保留各接口累计流量 */}
                    {speed && (
                      <p className="shrink-0 text-right text-[11px] text-gray-400 dark:text-gray-500">
                        {t('network.totalRx')} {formatBytes(speed.rx_total_bytes)} · {t('network.totalTx')} {formatBytes(speed.tx_total_bytes)}
                      </p>
                    )}
                  </li>
                )
              })}
            </ul>
          )}
        </div>
      </div>
    </div>
  )
}
