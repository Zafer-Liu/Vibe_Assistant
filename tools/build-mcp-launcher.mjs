import { execFileSync } from 'node:child_process'
import { copyFileSync, existsSync, mkdirSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = fileURLToPath(new URL('..', import.meta.url))
const tauri = join(root, 'src-tauri')
// CI passes the matrix target explicitly; a bare local run builds for the host.
// When a target is given, both crates are built with `--target` so their
// artifacts land in target/<triple>/ and the later `tauri build --target`
// reuses them instead of recompiling from scratch.
const explicitTarget = process.env.TAURI_TARGET_TRIPLE
const target = explicitTarget ?? execFileSync('rustc', ['-vV'], { encoding: 'utf8' })
  .match(/^host: (.+)$/m)?.[1]
if (!target) throw new Error('Cannot determine the Rust target triple')
const release = process.argv.includes('--release')
const suffix = target.includes('windows') ? '.exe' : ''
const args = ['build', '--manifest-path', join(tauri, 'mcp-launcher', 'Cargo.toml'), '--target', target]
if (release) args.push('--release')
execFileSync('cargo', args, { stdio: 'inherit' })
const destination = join(tauri, 'binaries')
mkdirSync(destination, { recursive: true })
const profile = release ? 'release' : 'debug'
const launcherSource = join(tauri, 'mcp-launcher', 'target', target, profile, `mcp-launcher${suffix}`)
copyFileSync(launcherSource, join(destination, `mcp-launcher-${target}${suffix}`))
if (!release) copyFileSync(launcherSource, join(destination, `mcp-launcher${suffix}`))
// The observed stdio relay is a bin of the main app package; bundle it beside
// the launcher so the GUI executable and active relays stay separate processes.
// Tauri's build script requires every externalBin file to exist before the
// app package compiles, so seed a placeholder first and replace it with the
// freshly built relay afterwards (bundling happens after compilation).
const relayBundled = join(destination, `mcp-relay-${target}${suffix}`)
if (!existsSync(relayBundled)) copyFileSync(join(destination, `mcp-launcher-${target}${suffix}`), relayBundled)
const relayArgs = ['build', '--manifest-path', join(tauri, 'Cargo.toml'), '--bin', 'mcp-relay']
if (release) relayArgs.push('--release')
// Only pin the target when CI supplied one: a local `--target <host>` build
// would land in a separate artifact directory and force a full recompile that
// `tauri dev` / `tauri build` (which build for the host) cannot reuse.
if (explicitTarget) relayArgs.push('--target', explicitTarget)
execFileSync('cargo', relayArgs, { stdio: 'inherit' })
const relaySource = explicitTarget
  ? join(tauri, 'target', explicitTarget, profile, `mcp-relay${suffix}`)
  : join(tauri, 'target', profile, `mcp-relay${suffix}`)
copyFileSync(relaySource, relayBundled)
if (!release) copyFileSync(relaySource, join(destination, `mcp-relay${suffix}`))
