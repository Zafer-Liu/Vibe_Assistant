import assert from 'node:assert/strict'
import { access, readFile } from 'node:fs/promises'
import { dirname, join, resolve } from 'node:path'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const tauriDir = join(root, 'src-tauri')
const config = JSON.parse(await readFile(join(tauriDir, 'tauri.conf.json'), 'utf8'))
const nsis = config.bundle.windows.nsis

test('NSIS installer branding is configured and every referenced file exists', async () => {
  for (const key of ['installerIcon', 'uninstallerIcon', 'headerImage', 'uninstallerHeaderImage', 'sidebarImage', 'template']) {
    assert.ok(nsis[key], `bundle.windows.nsis.${key} must be set`)
    await access(join(tauriDir, nsis[key]))
  }
})

test('icon.ico carries the standard size ladder, not a single 256px frame', async () => {
  const ico = await readFile(join(tauriDir, 'icons', 'icon.ico'))
  assert.equal(ico.readUInt16LE(2), 1, 'must be an icon (type 1) resource')
  const sizes = Array.from({ length: ico.readUInt16LE(4) }, (_, index) => ico[6 + 16 * index] || 256)
  for (const size of [16, 24, 32, 48, 256]) {
    assert.ok(sizes.includes(size), `icon.ico is missing the ${size}x${size} frame (has ${sizes.join(', ')})`)
  }
})

test('installer bitmaps are uncompressed 24-bit BMP at the NSIS recommended size', async () => {
  for (const [file, width, height] of [['header.bmp', 150, 57], ['sidebar.bmp', 164, 314]]) {
    const bmp = await readFile(join(tauriDir, 'installer', file))
    assert.equal(bmp.toString('ascii', 0, 2), 'BM', `${file} must be a BMP`)
    assert.equal(bmp.readInt32LE(18), width, `${file} width`)
    assert.equal(Math.abs(bmp.readInt32LE(22)), height, `${file} height`)
    assert.equal(bmp.readUInt16LE(28), 24, `${file} must be 24-bit`)
    assert.equal(bmp.readUInt32LE(30), 0, `${file} must be uncompressed`)
  }
})

test('custom NSIS template is pinned to the installed Tauri CLI and keeps the in-place upgrade change', async () => {
  const lock = JSON.parse(await readFile(join(root, 'package-lock.json'), 'utf8'))
  const cliVersion = lock.packages['node_modules/@tauri-apps/cli'].version
  const template = await readFile(join(tauriDir, nsis.template), 'utf8')
  const pinned = /@tauri-apps\/cli (\d+\.\d+\.\d+)/.exec(template)?.[1]
  assert.equal(
    pinned,
    cliVersion,
    `src-tauri/installer/installer.nsi is based on @tauri-apps/cli ${pinned}, but package-lock.json has ${cliVersion}. ` +
      're-copy the template for the new tag, re-apply the "Vibe Assistant:" change and update the version marker (see the file header).',
  )
  assert.match(template, /Vibe Assistant: reinstalling the same version or upgrading overwrites in place/)
  assert.match(template, /\$\{AndIf\} \$R0 >= 0/)
})
