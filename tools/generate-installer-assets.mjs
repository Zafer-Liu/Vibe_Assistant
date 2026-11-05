// 由 Logo/logo.png 生成 Windows 安装包使用的品牌资源，生成结果提交进仓库，CI 不需要重新生成：
//   src-tauri/icons/icon.ico          多尺寸图标（16–256）：exe、窗口、安装/卸载程序图标共用
//   src-tauri/installer/header.bmp    安装/卸载向导页眉图（150x57）
//   src-tauri/installer/sidebar.bmp   欢迎页与完成页侧栏图（164x314）
//
// 为什么需要它：原来的 icon.ico 只有一帧 256x256 的 PNG，缺少 16/32/48 等小尺寸帧，
// NSIS 与资源管理器取不到合适尺寸时会回退成默认图标；NSIS 的 BMP 图又必须是 24 位未压缩位图。
//
// 用法：node tools/generate-installer-assets.mjs
import { mkdir, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import sharp from 'sharp'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const source = path.join(root, 'Logo', 'logo.png')
const iconOut = path.join(root, 'src-tauri', 'icons', 'icon.ico')
const installerDir = path.join(root, 'src-tauri', 'installer')

const TRANSPARENT = { r: 0, g: 0, b: 0, alpha: 0 }
const ICON_SIZES = [16, 24, 32, 48, 64, 128, 256]
const ICON_MARGIN = 0.04 // 小尺寸图标里留白越少，圆形主体越清晰

// 去掉源图四周的透明/纯色留白，只保留圆形主体。
const logo = await sharp(source).ensureAlpha().trim().png().toBuffer()
const logoMeta = await sharp(logo).metadata()
console.log(`logo: ${logoMeta.width}x${logoMeta.height} (trimmed)`)

// 把 logo 等比缩放后居中放进 size x size 的透明画布。
async function squareLogo(size, margin) {
  const inner = Math.round(size * (1 - margin * 2))
  const resized = await sharp(logo)
    .resize(inner, inner, { fit: 'contain', background: TRANSPARENT, kernel: 'lanczos3' })
    .toBuffer()
  return sharp({ create: { width: size, height: size, channels: 4, background: TRANSPARENT } })
    .composite([{ input: resized, gravity: 'centre' }])
}

// ICO 中的 BMP 帧：BITMAPINFOHEADER（高度为 2 倍）+ 自下而上的 BGRA 像素 + 全 0 的 AND 掩码。
function bmpFrame(size, rgba) {
  if (rgba.length !== size * size * 4) throw new Error(`ICO frame ${size}: expected ${size * size * 4} RGBA bytes, got ${rgba.length}`)
  const xorBytes = size * size * 4
  const maskRow = Math.ceil(size / 32) * 4
  const header = Buffer.alloc(40)
  header.writeUInt32LE(40, 0)
  header.writeInt32LE(size, 4)
  header.writeInt32LE(size * 2, 8)
  header.writeUInt16LE(1, 12)
  header.writeUInt16LE(32, 14)
  header.writeUInt32LE(xorBytes + maskRow * size, 20)
  const pixels = Buffer.alloc(xorBytes)
  for (let y = 0; y < size; y += 1) {
    const sourceRow = (size - 1 - y) * size * 4
    const targetRow = y * size * 4
    for (let x = 0; x < size; x += 1) {
      const s = sourceRow + x * 4
      const d = targetRow + x * 4
      pixels[d] = rgba[s + 2]
      pixels[d + 1] = rgba[s + 1]
      pixels[d + 2] = rgba[s]
      pixels[d + 3] = rgba[s + 3]
    }
  }
  return Buffer.concat([header, pixels, Buffer.alloc(maskRow * size)])
}

function buildIco(frames) {
  const head = Buffer.alloc(6)
  head.writeUInt16LE(1, 2)
  head.writeUInt16LE(frames.length, 4)
  let offset = 6 + 16 * frames.length
  const entries = frames.map(({ size, data }) => {
    const entry = Buffer.alloc(16)
    entry[0] = size >= 256 ? 0 : size
    entry[1] = entry[0]
    entry.writeUInt16LE(1, 4)
    entry.writeUInt16LE(32, 6)
    entry.writeUInt32LE(data.length, 8)
    entry.writeUInt32LE(offset, 12)
    offset += data.length
    return entry
  })
  return Buffer.concat([head, ...entries, ...frames.map(frame => frame.data)])
}

// NSIS 只接受 24 位未压缩 BMP。rgb 为自上而下紧密排列的 RGB 数据。
function bmp24(width, height, rgb) {
  if (rgb.length !== width * height * 3) throw new Error(`BMP ${width}x${height}: expected ${width * height * 3} RGB bytes, got ${rgb.length}`)
  const rowSize = Math.ceil((width * 3) / 4) * 4
  const imageSize = rowSize * height
  const out = Buffer.alloc(54 + imageSize)
  out.write('BM', 0, 'ascii')
  out.writeUInt32LE(54 + imageSize, 2)
  out.writeUInt32LE(54, 10)
  out.writeUInt32LE(40, 14)
  out.writeInt32LE(width, 18)
  out.writeInt32LE(height, 22)
  out.writeUInt16LE(1, 26)
  out.writeUInt16LE(24, 28)
  out.writeUInt32LE(imageSize, 34)
  out.writeInt32LE(2835, 38)
  out.writeInt32LE(2835, 42)
  for (let y = 0; y < height; y += 1) {
    const sourceRow = (height - 1 - y) * width * 3
    const targetRow = 54 + y * rowSize
    for (let x = 0; x < width; x += 1) {
      out[targetRow + x * 3] = rgb[sourceRow + x * 3 + 2]
      out[targetRow + x * 3 + 1] = rgb[sourceRow + x * 3 + 1]
      out[targetRow + x * 3 + 2] = rgb[sourceRow + x * 3]
    }
  }
  return out
}

async function flattenToRgb(image) {
  return image.removeAlpha().raw().toBuffer()
}

// ── icon.ico ────────────────────────────────────────────────
const frames = []
for (const size of ICON_SIZES) {
  const canvas = await squareLogo(size, ICON_MARGIN)
  const data = size >= 256
    ? await canvas.png({ compressionLevel: 9 }).toBuffer()
    : bmpFrame(size, await canvas.raw().toBuffer())
  frames.push({ size, data })
}
await writeFile(iconOut, buildIco(frames))
console.log(`icon.ico: ${ICON_SIZES.join(', ')}`)

// ── 向导页眉图：白底居中 logo（页眉区域由 MUI 绘制标题文字，图上不放文字）────────
await mkdir(installerDir, { recursive: true })
const HEADER = { width: 150, height: 57 }
const headerLogo = await (await squareLogo(49, 0.02)).png().toBuffer()
const headerImage = sharp({
  create: { width: HEADER.width, height: HEADER.height, channels: 3, background: { r: 255, g: 255, b: 255 } },
}).composite([{ input: headerLogo, left: Math.round((HEADER.width - 49) / 2), top: 4 }])
await writeFile(path.join(installerDir, 'header.bmp'), bmp24(HEADER.width, HEADER.height, await flattenToRgb(headerImage)))
console.log('header.bmp: 150x57')

// ── 欢迎/完成页侧栏图：深蓝渐变底 + 光晕 + logo ───────────────────────────
const SIDE = { width: 164, height: 314 }
const sideLogoSize = 124
const sideLogoLeft = Math.round((SIDE.width - sideLogoSize) / 2)
const sideLogoTop = 72
const backdrop = Buffer.from(`<svg xmlns="http://www.w3.org/2000/svg" width="${SIDE.width}" height="${SIDE.height}">
  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#04122a"/>
      <stop offset="1" stop-color="#0b3a86"/>
    </linearGradient>
    <radialGradient id="glow" cx="${(SIDE.width / 2).toFixed(0)}" cy="${sideLogoTop + sideLogoSize / 2}" r="110" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="#38bdf8" stop-opacity="0.35"/>
      <stop offset="1" stop-color="#38bdf8" stop-opacity="0"/>
    </radialGradient>
  </defs>
  <rect width="100%" height="100%" fill="url(#bg)"/>
  <rect width="100%" height="100%" fill="url(#glow)"/>
</svg>`)
const sideLogo = await (await squareLogo(sideLogoSize, 0)).png().toBuffer()
const sidebarImage = sharp(backdrop).composite([{ input: sideLogo, left: sideLogoLeft, top: sideLogoTop }])
await writeFile(path.join(installerDir, 'sidebar.bmp'), bmp24(SIDE.width, SIDE.height, await flattenToRgb(sidebarImage)))
console.log('sidebar.bmp: 164x314')
