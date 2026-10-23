import { getLanguage, intlLocale } from '../i18n'

interface ScaleUnit {
  /** 进入该单位的最小数值。 */
  base: number
  /** 中文带空格分隔（「251.7 亿」），英文紧贴后缀（「25.17B」）。 */
  suffix: string
}

const ZH_SCALE: ScaleUnit[] = [
  { base: 1e8, suffix: ' 亿' },
  { base: 1e4, suffix: ' 万' },
]
const EN_SCALE: ScaleUnit[] = [
  { base: 1e9, suffix: 'B' },
  { base: 1e6, suffix: 'M' },
  { base: 1e3, suffix: 'K' },
]

/**
 * 大数字的紧凑写法：中文按万/亿，英文按 K/M/B。记忆中心的 Token 总量动辄
 * 上百亿，逐位展开会把概览卡撑破。小数位随量级收紧，进位跨过上一级基数时
 * 改用上一级单位，避免出现「10000 万」这种比原数字更难读的结果。
 */
export function compactTokenCount(value: number): string {
  if (!Number.isFinite(value)) return '—'
  // Token 计数本身是整数，但入参可能是估算值；先取整，避免 9999.6 四舍五入
  // 成 10,000 后仍然逐位展开。
  const absolute = Math.round(Math.abs(value))
  const sign = value < 0 ? '-' : ''
  const scale = getLanguage() === 'en' ? EN_SCALE : ZH_SCALE
  const index = scale.findIndex((unit) => absolute >= unit.base)
  if (index === -1) return `${sign}${new Intl.NumberFormat(intlLocale()).format(absolute)}`
  const unit = scale[index]
  const upper = index > 0 ? scale[index - 1] : null
  // 先用本位格式化再判断是否进位：9999.9999 万 → 1 亿，999999 → 100 万。
  const chosen = upper && Number(formatScaled(absolute / unit.base)) >= upper.base / unit.base ? upper : unit
  return `${sign}${formatScaled(absolute / chosen.base)}${chosen.suffix}`
}

/** 小数点后位数随量级收紧，并去掉多余的 0。 */
function formatScaled(scaled: number): string {
  return scaled.toFixed(scaled >= 1000 ? 1 : 2).replace(/\.?0+$/, '')
}
