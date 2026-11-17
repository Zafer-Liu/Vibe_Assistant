import { useEffect, useState } from 'react'
import { createRoot } from 'react-dom/client'
import { createPortal } from 'react-dom'
import { useTranslation } from 'react-i18next'
import { MemoryRecallInspector } from '../src/components/MemoryRecallInspector'
import { CurrentMemoryEvidence } from '../src/components/MemoryEvidencePanel'
import i18n from '../src/i18n'
import '../src/index.css'
import { CURRENT_TRACE, fixture, initialAccess, installFixture, unsavedResponse } from './fixture'

installFixture()
// This page does not persist language/theme or import the production app/store.
void i18n.changeLanguage('zh')
document.documentElement.classList.remove('dark')

function VerificationHarness() {
  const { i18n: language } = useTranslation()
  const unsavedTrace = new URLSearchParams(location.search).get('scenario') === 'trace-unavailable'
  const [open, setOpen] = useState(!unsavedTrace)
  const [dark, setDark] = useState(false)
  const [slow, setSlow] = useState(false)
  const [dialog, setDialog] = useState<HTMLDialogElement | null>(null)
  const [rerunId, setRerunId] = useState<string | null>(null)
  const english = language.language === 'en'

  useEffect(() => {
    // Put the test toolbar inside the native modal's top layer so its controls
    // remain keyboard/mouse accessible while the real inspector is open.
    const update = () => setDialog(document.querySelector('dialog'))
    const observer = new MutationObserver(update)
    observer.observe(document.getElementById('root')!, { childList: true, subtree: true })
    update()
    return () => observer.disconnect()
  }, [])

  function toggleLanguage() {
    const next = english ? 'zh' : 'en'
    void language.changeLanguage(next)
    document.documentElement.lang = next === 'zh' ? 'zh-CN' : 'en'
  }

  function toggleTheme() {
    document.documentElement.classList.toggle('dark', !dark)
    setDark(!dark)
  }

  const toolbar = <header className="verification-toolbar" aria-label="Synthetic verification controls">
    <div>
      <h1>隔离 UI 验证｜合成 IPC 数据｜不连接用户记忆库</h1>
      <p>{english ? 'Synthetic UI fixture only · No model execution · No native backend verification · Dialog file actions return null' : '仅合成 UI fixture · 不执行模型 · 不代表原生后端验证 · 导入/导出对话框返回 null'}</p>
      <p data-testid="initial-test-label">{english ? 'Initial: current trace + ready access · old history requires rerun' : '初始：当前 trace + ready Access｜旧历史需重跑'}</p>
    </div>
    <div className="verification-controls">
      <button type="button" onClick={() => setOpen(value => !value)}>{open ? (english ? 'Close inspector' : '关闭诊断面板') : (english ? 'Open inspector' : '打开诊断面板')}</button>
      <button type="button" onClick={toggleLanguage}>{english ? '切换中文' : 'Switch to English'}</button>
      <button type="button" onClick={toggleTheme}>{dark ? (english ? 'Light theme' : '切换浅色') : (english ? 'Dark theme' : '切换深色')}</button>
      <button type="button" aria-pressed={slow} onClick={() => { fixture.evidenceDelayMs = slow ? 1800 : 6000; setSlow(!slow) }}>{english ? `Read delay: ${slow ? '6' : '1.8'}s` : `读取延迟：${slow ? '6' : '1.8'} 秒`}</button>
    </div>
  </header>

  return <>
    {dialog && open ? createPortal(toolbar, dialog) : toolbar}
    <main className="verification-main">
      <h2 className="text-xl font-semibold">{english ? 'Initial test: current trace + ready evidence access' : '初始测试：当前 trace + ready 来源访问凭据'}</h2>
      <p>{english ? 'Two synthetic history entries. Only the current trace has initial access; rerun the old trace with its stored arguments to obtain a new trace/access and compare added results, ranking and fingerprints.' : '初始历史两条。当前记录具有 ready 访问凭据；旧历史无访问权限。请重跑旧历史原参数，获得新 trace/Access 并比较新增结果、排名与指纹。'}</p>
      <p>{english ? 'Source reads expose ORBIT_KEY, Unicode code-point ranges and SHA-256 fingerprints. Continue changes the excerpt window and spends the shared character allowance. Cancel or close during the delay to test late-response guards.' : '来源读取展示可见 ORBIT_KEY 合成片段、Unicode 码点范围与 SHA-256 指纹。继续读取会改变窗口并扣减共享额度。可在延迟期间取消或关闭，检查迟到响应保护。'}</p>
      <p>{english ? 'Evaluation requires preview and explicit confirmation. Status polling drives preparing → running → completed; cancellation preserves a partial report. Reports include success/failure/not_run; null metrics mean unmeasured.' : '评估需先预览、再显式确认。status 查询推动 preparing → running → completed；取消会保存部分报告。报告包含成功/失败/not_run，null 指标显示未测量。'}</p>
      <p>{english ? 'Designed for 1440 × 1000 and 900 × 600. Read-only inspection: window.__memoryFixture. Reload resets all synthetic state.' : '设计检查尺寸：1440 × 1000、900 × 600。只读检查：window.__memoryFixture。刷新页面会重置全部合成状态。'}</p>
      {unsavedTrace && <section className="space-y-3 rounded-[8px] border border-[var(--app-stroke)] bg-[var(--app-surface)] p-4">
        <h2 className="text-base font-semibold">{english ? 'Regression: unsaved trace with valid source access' : '回归：轨迹未保存，但来源访问有效'}</h2>
        <p>{english ? 'Trace status: unavailable. Open source excerpts and read; no trace/history lookup is needed.' : 'trace_status = unavailable。展开来源片段即可读取，无需查询轨迹或历史。'}</p>
        <CurrentMemoryEvidence access={unsavedResponse.evidence_access ?? null} requestId={unsavedResponse.retrieval?.request_id}
          query={unsavedResponse.query ?? ''} memories={unsavedResponse.memories.map(memory => ({ id: memory.id, memory: memory.content }))} />
      </section>}
      {rerunId && <p role="status">Synthetic rerun: {rerunId}</p>}
    </main>
    {open && <MemoryRecallInspector initialRequestId={CURRENT_TRACE} evidenceAccess={initialAccess} onClose={() => setOpen(false)} onRerun={response => setRerunId(response.retrieval?.request_id ?? null)} />}
  </>
}

createRoot(document.getElementById('root')!).render(<VerificationHarness />)
