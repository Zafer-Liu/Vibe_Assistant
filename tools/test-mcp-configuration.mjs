// Run with Node 22.18+ (native TypeScript type stripping).
import assert from 'node:assert/strict'
import { test } from 'node:test'
import { defaultImportSelection, groupImportCandidates, selectedImports } from '../frontend/src/features/mcp/importSelection.ts'
import { buildMcpComparisonRows } from '../frontend/src/features/mcp/configurationComparison.ts'
import { buildMcpConnectionSummary } from '../frontend/src/lib/mcpConnectionCenter.ts'

const candidate = (name, agent, versions = 1, catalog = 'new') => ({
  agent, entry: { name }, variant: 1, variant_count: versions,
  already_in_catalog: catalog !== 'new', catalog_state: catalog,
  differing_fields: [], peer_differing_fields: [],
})

test('same-name sources stay grouped; conflicts and existing entries require a source choice', () => {
  const candidates = [
    candidate('same', 'qoder'), candidate('same', 'kimi'),
    candidate('conflict', 'qoder', 2), candidate('conflict', 'kimi', 2),
    candidate('existing', 'kimi', 1, 'matches'),
  ]
  assert.equal(groupImportCandidates(candidates).get('same').length, 2)
  assert.deepEqual([...defaultImportSelection(candidates)], [['same', 'qoder']])
})

test('a selected version produces exactly one import for that service name', () => {
  const candidates = [candidate('demo', 'claude_cli', 2), candidate('demo', 'qoder', 2)]
  const picked = new Map([['demo', 'qoder']])
  assert.deepEqual(selectedImports(candidates, picked).map(item => item.agent), ['qoder'])
  picked.delete('demo')
  assert.deepEqual(selectedImports(candidates, picked), [])
  assert.deepEqual(selectedImports(candidates, new Map([['demo', 'removed-source']])), [])
})

test('incomplete scans require an explicit choice even for apparently unique new services', () => {
  const candidates = [candidate('apparently-unique', 'qoder')]
  assert.equal(defaultImportSelection(candidates, false).size, 0)
  assert.equal(defaultImportSelection(candidates, true).size, 1)
})

test('summary includes discovered differences without inflating managed deployments', () => {
  const catalog = [{ name: 'demo', assigned_agents: ['claude_cli', 'kimi'] }]
  const statuses = [
    { name: 'demo', agent: 'claude_cli', state: 'installed' },
    { name: 'demo', agent: 'kimi', state: 'unavailable' },
    { name: 'demo', agent: 'qoder', state: 'differs' },
    { name: 'demo', agent: 'codex_cli', state: 'installed' },
  ]
  const summary = buildMcpConnectionSummary(catalog, statuses, [], 'ready')
  assert.deepEqual(summary, { services: 1, assignments: 2, profiles: 0, installed: 2, differs: 1, missing: 0, unavailable: 1 })
  const loading = buildMcpConnectionSummary(catalog, statuses, [], 'loading')
  assert.equal(loading.installed, null)
  assert.equal(loading.unavailable, null)
})


test('comparison exposes changed environment keys even while values are masked in the view', () => {
  const base = { transport: 'stdio', command: 'node', args: ['a', 'b'], env: { TOKEN: 'one' }, url: '', headers: {} }
  const changed = { ...base, env: { TOKEN: 'two', EXTRA: 'value' }, args: ['b', 'a'] }
  const rows = buildMcpComparisonRows(base, changed)
  assert.deepEqual(rows.filter(row => row.changed).map(row => row.key), ['args', 'env.TOKEN', 'env.EXTRA'])
  assert.equal(rows.find(row => row.key === 'env.TOKEN').sensitive, true)
  assert.equal(rows.find(row => row.key === 'env.EXTRA').left, undefined)
})

test('comparison ignores key order and distinguishes a missing value from an empty one', () => {
  const base = { transport: '', command: 'node', args: [], env: { B: '2', A: '' }, url: '', headers: {} }
  const same = { ...base, transport: 'stdio', env: { A: '', B: '2' } }
  assert.equal(buildMcpComparisonRows(base, same).some(row => row.changed), false)
  const rows = buildMcpComparisonRows({ ...same, env: {} }, same)
  assert.equal(rows.find(row => row.key === 'env.A').changed, true)
})
