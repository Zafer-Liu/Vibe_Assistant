import { execFileSync } from 'node:child_process'
import { mkdirSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

// A sandbox can label the checkout Low. Executables inherit that label and
// Windows then launches them at Low integrity, denying writes to normal AppData
// even when the database ACL grants full access. Override inheritance only for
// Cargo's disposable build output; keep the checkout and user data permissions.
export function prepareDevelopmentTarget(directory) {
  if (process.platform !== 'win32') return
  const target = resolve(directory)
  mkdirSync(target, { recursive: true })
  try {
    execFileSync('icacls.exe', [target, '/setintegritylevel', '(OI)(CI)M'], {
      encoding: 'utf8', windowsHide: true,
    })
  } catch (cause) {
    throw new Error(`Cannot set normal Windows integrity on Cargo build output ${target}; development executables may be unable to write AppData.`, { cause })
  }
}

// Windows keeps running executables locked, including a GUI hidden in the tray.
// Match the full build path so installed apps and other checkouts are untouched.
export function stopDevelopmentApp(executable) {
  if (process.platform !== 'win32') return
  const script = `
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
try {
$expected = [IO.Path]::GetFullPath($env:AGENT_MANAGER_DEV_EXE)
$instances = @(Get-CimInstance Win32_Process -Filter "Name = 'agent-manager.exe'" |
  Where-Object { $_.ExecutablePath -and [IO.Path]::GetFullPath($_.ExecutablePath) -eq $expected })
if ($instances | Where-Object { $_.CommandLine -match '(?:^|[\\s"])(?:--mcp-memory|--mcp-relay)(?:[\\s"]|$)' }) {
  throw 'The development executable is serving an MCP connection. Stop that connection before rebuilding; use the standalone MCP launcher for persistent connections.'
}
foreach ($instance in $instances) {
  $appProcess = Get-Process -Id $instance.ProcessId -ErrorAction SilentlyContinue
  if ($appProcess -and $appProcess.Path -eq $expected) {
    Write-Output "[dev] Stopping previous development app (PID $($instance.ProcessId))"
    Stop-Process -InputObject $appProcess -Force
    if (-not $appProcess.WaitForExit(10000)) {
      throw 'The previous development app did not exit; its executable is still locked.'
    }
  }
}
} catch {
  [Console]::Error.WriteLine($_.Exception.Message)
  exit 1
}
`
  execFileSync('powershell.exe', [
    '-NoProfile', '-NonInteractive', '-OutputFormat', 'Text',
    '-EncodedCommand', Buffer.from(script, 'utf16le').toString('base64'),
  ], {
    env: { ...process.env, AGENT_MANAGER_DEV_EXE: resolve(executable) },
    stdio: 'inherit',
    windowsHide: true,
  })
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href && process.platform === 'win32') {
  const root = fileURLToPath(new URL('..', import.meta.url))
  // Cargo metadata respects CARGO_TARGET_DIR and the project's Cargo configuration.
  const metadata = JSON.parse(execFileSync('cargo', [
    'metadata', '--manifest-path', join(root, 'src-tauri', 'Cargo.toml'), '--no-deps', '--format-version', '1',
  ], { cwd: root, encoding: 'utf8', windowsHide: true }))
  stopDevelopmentApp(join(metadata.target_directory, 'debug', 'agent-manager.exe'))
  prepareDevelopmentTarget(metadata.target_directory)
}
