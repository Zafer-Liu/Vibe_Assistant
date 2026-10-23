import { execFileSync } from 'node:child_process'
import { copyFileSync, existsSync, mkdirSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = fileURLToPath(new URL('..', import.meta.url))
const tauri = join(root, 'src-tauri')
const target = process.env.TAURI_TARGET_TRIPLE ?? execFileSync('rustc', ['-vV'], { encoding: 'utf8' })
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
execFileSync('cargo', ['build', '--manifest-path', join(tauri, 'Cargo.toml'), '--bin', 'mcp-relay', ...release ? ['--release'] : []], { stdio: 'inherit' })
const relaySource = join(tauri, 'target', profile, `mcp-relay${suffix}`)
copyFileSync(relaySource, relayBundled)
if (!release) copyFileSync(relaySource, join(destination, `mcp-relay${suffix}`))
