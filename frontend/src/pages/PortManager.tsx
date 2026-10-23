import { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { invoke } from '@tauri-apps/api/core'
import { RefreshCw, Zap, Search, AlertTriangle } from 'lucide-react'

interface PortInfo {
  port: number
  pid: number
  process_name: string
  protocol: string
  state: string
}

export function PortManager() {
  const { t } = useTranslation()
  const [ports, setPorts] = useState<PortInfo[]>([])
  const [loading, setLoading] = useState(false)
  const [search, setSearch] = useState('')
  const [killing, setKilling] = useState<number | null>(null)
  const [killResult, setKillResult] = useState<{ port: number; msg: string; ok: boolean } | null>(null)
  const [confirmKill, setConfirmKill] = useState<PortInfo | null>(null)

  async function load() {
    setLoading(true)
    try {
      const list = await invoke<PortInfo[]>('list_listening_ports')
      setPorts(list)
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => { load() }, [])

  async function doKill(port: number) {
    setKilling(port)
    setConfirmKill(null)
    try {
      const msg = await invoke<string>('kill_port', { port })
      setKillResult({ port, msg, ok: true })
      setTimeout(() => { setKillResult(null); load() }, 2000)
    } catch (e) {
      setKillResult({ port, msg: String(e), ok: false })
      setTimeout(() => setKillResult(null), 3000)
    } finally {
      setKilling(null)
    }
  }

  const filtered = ports.filter(p => {
    if (!search) return true
    const q = search.toLowerCase()
    return String(p.port).includes(q)
      || p.process_name.toLowerCase().includes(q)
      || String(p.pid).includes(q)
  })

  return (
    <div className="flex h-full flex-col bg-gray-50 dark:bg-gray-950">
      {/* Header */}
      <div className="border-b border-gray-200 bg-white px-6 py-4 dark:border-gray-700 dark:bg-gray-900">
        <div className="flex items-center justify-between">
          <div>
            <h1 className="text-base font-semibold text-gray-900 dark:text-gray-100">{t('ports.title')}</h1>
            <p className="text-xs text-gray-500 dark:text-gray-400">{t('ports.subtitle', { count: ports.length })}</p>
          </div>
          <button
            onClick={load}
            disabled={loading}
            className="flex items-center gap-1.5 rounded-lg border border-gray-200 px-3 py-1.5 text-sm text-gray-600 hover:bg-gray-50 disabled:opacity-40 dark:border-gray-700 dark:text-gray-400 dark:hover:bg-gray-800"
          >
            <RefreshCw className={`h-4 w-4 ${loading ? 'animate-spin' : ''}`} />
            {t('ports.refresh')}
          </button>
        </div>

        {/* Search */}
        <div className="relative mt-3">
          <Search className="absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-gray-500" />
          <input
            value={search}
            onChange={e => setSearch(e.target.value)}
            placeholder={t('ports.filterPlaceholder')}
            className="field-input !pl-10"
          />
        </div>
      </div>

      {/* Kill result toast */}
      {killResult && (
        <div className={`mx-6 mt-4 flex items-center gap-2 rounded-lg px-4 py-2.5 text-sm ${
          killResult.ok
            ? 'bg-green-50 text-green-700 dark:bg-green-900/20 dark:text-green-400'
            : 'bg-red-50 text-red-600 dark:bg-red-900/20 dark:text-red-400'
        }`}>
          {killResult.ok ? '✓' : '✗'} {killResult.msg}
        </div>
      )}

      {/* Table */}
      <div className="flex-1 overflow-auto p-6">
        <div className="rounded-2xl border border-gray-200 bg-white overflow-hidden dark:border-gray-700 dark:bg-gray-900">
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b border-gray-100 dark:border-gray-800 bg-gray-50 dark:bg-gray-800/50">
                <th className="px-4 py-3 text-left text-xs font-semibold uppercase tracking-wide text-gray-500">{t('ports.port')}</th>
                <th className="px-4 py-3 text-left text-xs font-semibold uppercase tracking-wide text-gray-500">{t('ports.protocol')}</th>
                <th className="px-4 py-3 text-left text-xs font-semibold uppercase tracking-wide text-gray-500">{t('ports.process')}</th>
                <th className="px-4 py-3 text-left text-xs font-semibold uppercase tracking-wide text-gray-500">{t('ports.pid')}</th>
                <th className="px-4 py-3 text-left text-xs font-semibold uppercase tracking-wide text-gray-500">{t('ports.state')}</th>
                <th className="px-4 py-3 text-right text-xs font-semibold uppercase tracking-wide text-gray-500">{t('ports.action')}</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-gray-50 dark:divide-gray-800">
              {filtered.length === 0 && (
                <tr>
                  <td colSpan={6} className="py-12 text-center text-gray-500">
                    {loading ? t('ports.loading') : t('ports.noPorts')}
                  </td>
                </tr>
              )}
              {filtered.map(p => (
                <tr key={p.port} className="hover:bg-gray-50 dark:hover:bg-gray-800/40 transition-colors">
                  <td className="px-4 py-3 font-mono font-semibold text-blue-600 dark:text-blue-400">
                    {p.port}
                  </td>
                  <td className="px-4 py-3">
                    <span className="rounded bg-gray-100 px-1.5 py-0.5 text-xs font-mono text-gray-600 dark:bg-gray-800 dark:text-gray-400">
                      {p.protocol}
                    </span>
                  </td>
                  <td className="px-4 py-3 font-mono text-gray-700 dark:text-gray-300">
                    {p.process_name}
                  </td>
                  <td className="px-4 py-3 font-mono text-gray-500 text-xs">
                    {p.pid}
                  </td>
                  <td className="px-4 py-3">
                    <span className="inline-flex items-center gap-1 rounded-md bg-green-50 px-2 py-0.5 text-xs font-medium text-green-700 dark:bg-green-900/20 dark:text-green-400">
                      <span className="h-1.5 w-1.5 rounded-full bg-green-500" />
                      {p.state}
                    </span>
                  </td>
                  <td className="px-4 py-3 text-right">
                    <button
                      onClick={() => setConfirmKill(p)}
                      disabled={killing === p.port}
                      className="flex items-center gap-1.5 rounded-lg border border-red-200 px-2.5 py-1 text-xs font-medium text-red-600 transition-colors hover:bg-red-50 disabled:opacity-40 dark:border-red-800 dark:text-red-400 dark:hover:bg-red-900/20 ml-auto"
                    >
                      <Zap className="h-3 w-3" />
                      {killing === p.port ? t('ports.killing') : t('ports.kill')}
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </div>

      {/* Kill confirm modal */}
      {confirmKill && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40">
          <div className="rounded-2xl border border-gray-200 bg-white p-6 shadow-xl dark:border-gray-700 dark:bg-gray-900 w-80">
            <div className="flex items-center gap-3 mb-3">
              <div className="flex h-10 w-10 items-center justify-center rounded-full bg-red-100 dark:bg-red-900/30">
                <AlertTriangle className="h-5 w-5 text-red-600 dark:text-red-400" />
              </div>
              <div>
                <h3 className="font-semibold text-gray-900 dark:text-gray-100">{t('ports.killConfirmTitle', { port: confirmKill.port })}</h3>
                <p className="text-xs text-gray-500">{t('ports.killConfirmProcess', { name: confirmKill.process_name, pid: confirmKill.pid })}</p>
              </div>
            </div>
            <p className="text-sm text-gray-500 dark:text-gray-400 mb-4">
              {t('ports.killConfirmWarn')}
            </p>
            <div className="flex gap-2 justify-end">
              <button onClick={() => setConfirmKill(null)} className="px-3 py-1.5 text-sm text-gray-500 hover:bg-gray-100 rounded-lg dark:hover:bg-gray-800">
                {t('common.cancel')}
              </button>
              <button
                onClick={() => doKill(confirmKill.port)}
                className="px-3 py-1.5 text-sm bg-red-600 text-white rounded-lg hover:bg-red-500"
              >
                {t('ports.killProcess')}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  )
}
