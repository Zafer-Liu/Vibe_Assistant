interface DomainSurface {
  light: string
  dark: string
  strokeLight: string
  strokeDark: string
  marker: string
}

// Quiet category surfaces; labels, rather than color alone, identify every domain.
const DOMAIN_SURFACES: readonly DomainSurface[] = [
  { light: '#edf4fc', dark: '#263344', strokeLight: '#c6d8ed', strokeDark: '#425875', marker: '#547ca8' },
  { light: '#f3effa', dark: '#342d40', strokeLight: '#d8ccec', strokeDark: '#594969', marker: '#8a6ca8' },
  { light: '#edf5f3', dark: '#283b37', strokeLight: '#c7ddd6', strokeDark: '#455e56', marker: '#587f74' },
  { light: '#f8f2e9', dark: '#3d3529', strokeLight: '#e5d6c0', strokeDark: '#64563e', marker: '#9b8058' },
  { light: '#f8eff1', dark: '#3e2d33', strokeLight: '#e8cdd5', strokeDark: '#674853', marker: '#a16b7d' },
  { light: '#eef3f5', dark: '#2b373c', strokeLight: '#cbdbe1', strokeDark: '#465b65', marker: '#668692' },
]

export function getDomainSurface(domainId: string): DomainSurface {
  let hash = 2166136261
  for (const character of domainId) hash = Math.imul(hash ^ character.codePointAt(0)!, 16777619)
  return DOMAIN_SURFACES[(hash >>> 0) % DOMAIN_SURFACES.length]
}
