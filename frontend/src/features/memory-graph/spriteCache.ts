import type { LobePalette } from './types'

// Cache small, flat markers instead of large transparent glow textures.
const nodeSpriteCache = new Map<string, HTMLCanvasElement>()

export function getNodeSprite(palette: LobePalette, isDark = false): HTMLCanvasElement {
  const key = `${palette.primary}:${isDark}`
  const cached = nodeSpriteCache.get(key)
  if (cached) return cached

  const canvas = document.createElement('canvas')
  canvas.width = 48
  canvas.height = 48
  const ctx = canvas.getContext('2d')
  if (!ctx) return canvas

  ctx.fillStyle = palette.primary
  ctx.strokeStyle = isDark ? '#e5e5e5' : '#555555'
  ctx.lineWidth = 2
  ctx.beginPath()
  ctx.arc(24, 24, 20, 0, Math.PI * 2)
  ctx.fill()
  ctx.stroke()
  nodeSpriteCache.set(key, canvas)
  return canvas
}

export function drawCachedNodeSprite(
  ctx: CanvasRenderingContext2D,
  palette: LobePalette,
  x: number,
  y: number,
  radius: number,
  isDark = false,
) {
  const sprite = getNodeSprite(palette, isDark)
  const drawSize = radius * (48 / 20)
  ctx.drawImage(sprite, x - drawSize / 2, y - drawSize / 2, drawSize, drawSize)
}
