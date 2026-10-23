import type { McpImportCandidate } from '../../types/mcp'

export function groupImportCandidates(candidates: McpImportCandidate[]) {
  const groups = new Map<string, McpImportCandidate[]>()
  for (const candidate of candidates) {
    const group = groups.get(candidate.entry.name) ?? []
    group.push(candidate)
    groups.set(candidate.entry.name, group)
  }
  return groups
}

/** Select only unambiguous new services. Conflicts require an explicit source. */
export function defaultImportSelection(candidates: McpImportCandidate[], scanComplete = true) {
  const selected = new Map<string, string>()
  if (!scanComplete) return selected
  for (const [name, group] of groupImportCandidates(candidates)) {
    if (!group[0].already_in_catalog && group[0].variant_count === 1) {
      selected.set(name, group[0].agent)
    }
  }
  return selected
}

export function selectedImports(candidates: McpImportCandidate[], picked: Map<string, string>) {
  // One write per server name, regardless of how many sources have that name.
  return [...groupImportCandidates(candidates)].flatMap(([name, group]) => {
    const selected = group.find(candidate => candidate.agent === picked.get(name))
    return selected ? [selected] : []
  })
}
