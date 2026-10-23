import assert from 'node:assert/strict'
import { execFileSync, spawn, spawnSync } from 'node:child_process'
import { once } from 'node:events'
import { copyFile, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { test } from 'node:test'
import { prepareDevelopmentTarget, stopDevelopmentApp } from './prepare-dev.mjs'

test('development build output can write normal data after inheriting Low integrity', {
  skip: process.platform !== 'win32',
  timeout: 60_000,
}, async (t) => {
  const directory = await mkdtemp(join(tmpdir(), 'agent-manager-integrity-'))
  t.after(() => rm(directory, { recursive: true, force: true }))
  const checkout = join(directory, 'low checkout')
  const target = join(checkout, 'custom target')
  const existingExe = join(target, 'debug', 'write-probe.exe')
  const data = join(directory, 'normal-data.txt')
  await writeFile(data, 'original')
  await mkdir(join(existingExe, '..'), { recursive: true })
  await copyFile(process.execPath, existingExe)
  execFileSync('icacls.exe', [checkout, '/setintegritylevel', '(OI)(CI)L'], { windowsHide: true })
  const probe = (executable) => spawnSync(executable, [
    '-e', 'require("node:fs").writeFileSync(process.argv[1], "updated")', data,
  ], { encoding: 'utf8', windowsHide: true })
  const before = probe(existingExe)
  assert.notEqual(before.status, 0, 'Low integrity executable must reproduce the write failure')
  assert.match(before.stderr, /EACCES|EPERM/)
  assert.equal(await readFile(data, 'utf8'), 'original')

  prepareDevelopmentTarget(target)
  const after = probe(existingExe)
  assert.equal(after.status, 0, after.stderr)
  assert.equal(await readFile(data, 'utf8'), 'updated')
  prepareDevelopmentTarget(target)
  const freshTarget = join(checkout, 'fresh target')
  prepareDevelopmentTarget(freshTarget)
  const freshExe = join(freshTarget, 'new-build.exe')
  await copyFile(process.execPath, freshExe)
  const fresh = probe(freshExe)
  assert.equal(fresh.status, 0, fresh.stderr)
})

test('development cleanup stops only this build and preserves MCP connections', {
  skip: process.platform !== 'win32',
  timeout: 60_000,
}, async (t) => {
  const directory = await mkdtemp(join(tmpdir(), 'agent-manager-dev-'))
  const children = []
  t.after(async () => {
    await Promise.all(children.map(async (child) => {
      if (child.exitCode !== null || child.signalCode !== null) return
      const exited = once(child, 'exit')
      child.kill()
      await exited
    }))
    await rm(directory, { recursive: true, force: true })
  })
  const ownExe = join(directory, 'own build', 'agent-manager.exe')
  const otherExe = join(directory, 'other build', 'agent-manager.exe')
  for (const executable of [ownExe, otherExe]) {
    await mkdir(join(executable, '..'), { recursive: true })
    await copyFile(process.execPath, executable)
  }
  async function start(executable, ...args) {
    const child = spawn(executable, [
      '-e', 'console.log("ready"); setInterval(() => {}, 1000)', '--', ...args,
    ], { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] })
    children.push(child)
    await once(child.stdout, 'data')
    return child
  }

  const own = await start(ownExe, '--page=memory')
  const other = await start(otherExe)
  stopDevelopmentApp(ownExe)
  await once(own, 'exit')
  assert.doesNotThrow(() => process.kill(other.pid, 0), 'other checkouts must remain running')
  assert.doesNotThrow(() => stopDevelopmentApp(ownExe), 'cleanup is safe with no old app')

  for (const mode of ['--mcp-memory', '--mcp-relay']) {
    const mcp = await start(ownExe, mode)
    const gui = await start(ownExe, '--page=memory')
    assert.throws(() => stopDevelopmentApp(ownExe), 'an active MCP connection blocks cleanup')
    assert.doesNotThrow(() => process.kill(mcp.pid, 0), 'MCP connections must remain running')
    assert.doesNotThrow(() => process.kill(gui.pid, 0), 'detect blockers before stopping any app')
    const exits = [once(mcp, 'exit'), once(gui, 'exit')]
    mcp.kill()
    gui.kill()
    await Promise.all(exits)
  }
})
