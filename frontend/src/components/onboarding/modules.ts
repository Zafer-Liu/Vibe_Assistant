import { Brain, BookOpenText, Plug } from 'lucide-react'

export const ONBOARDING_MODULES = [
  { key: 'memory', page: 'memory', icon: Brain },
  { key: 'skills', page: 'skills', icon: BookOpenText },
  { key: 'mcp', page: 'mcp-library', icon: Plug },
] as const

export type OnboardingModule = typeof ONBOARDING_MODULES[number]['key']
export type OnboardingDestination = typeof ONBOARDING_MODULES[number]['page']
