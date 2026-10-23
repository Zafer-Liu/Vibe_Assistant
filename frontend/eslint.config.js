import js from '@eslint/js'
import globals from 'globals'
import reactHooks from 'eslint-plugin-react-hooks'
import reactRefresh from 'eslint-plugin-react-refresh'
import tseslint from 'typescript-eslint'
import { defineConfig, globalIgnores } from 'eslint/config'

export default defineConfig([
  globalIgnores(['dist']),
  {
    files: ['**/*.{ts,tsx}'],
    extends: [
      js.configs.recommended,
      tseslint.configs.recommended,
      reactHooks.configs.flat.recommended,
      reactRefresh.configs.vite,
    ],
    languageOptions: {
      globals: globals.browser,
    },
    rules: {
      // Data-loading effects are intentional in this desktop client. The
      // exhaustive-deps rule still reports dependency mistakes as warnings.
      'react-hooks/set-state-in-effect': 'off',
      // theme.tsx exports both the provider and its hook by design.
      'react-refresh/only-export-components': 'off',
    },
  },
  // Feature state and views use typed clients instead of importing transport or other features.
  {
    files: ['src/store/skillStore.ts', 'src/features/skills/**/*.{ts,tsx}', 'src/pages/SkillLibrary.tsx', 'src/pages/PublishedSkills.tsx'],
    rules: {
      'no-restricted-imports': ['error', {
        paths: [{ name: '@tauri-apps/api/core', message: 'Use the typed skill API client.' }],
        patterns: [{ group: ['**/pages/**', '**/store/memoryStore', '**/store/mcpStore', '**/store/telemetryStore', '**/types/memory'], message: 'Keep skill dependencies inside the skill feature and shared utilities.' }],
      }],
    },
  },
  {
    files: ['src/store/mcpStore.ts', 'src/features/mcp/**/*.{ts,tsx}', 'src/pages/McpLibrary.tsx'],
    rules: {
      'no-restricted-imports': ['error', {
        paths: [{ name: '@tauri-apps/api/core', message: 'Use the typed MCP API client.' }],
        patterns: [{ group: ['**/pages/**', '**/store/memoryStore', '**/store/skillStore', '**/store/telemetryStore', '**/types/memory'], message: 'Keep MCP dependencies inside the MCP feature and shared utilities.' }],
      }],
    },
  },
  {
    files: ['src/store/telemetryStore.ts'],
    rules: {
      'no-restricted-imports': ['error', {
        paths: [{ name: '@tauri-apps/api/core', message: 'Use the typed telemetry API client.' }],
        patterns: [{ group: ['**/pages/**', '**/store/*', '**/types/memory'], message: 'Telemetry state must not depend on UI or other stores.' }],
      }],
    },
  },
  {
    files: ['src/api/**/*.{ts,tsx}'],
    rules: {
      'no-restricted-imports': ['error', {
        patterns: [{ group: ['**/pages/**', '**/store/*'], message: 'API clients own IPC and must not depend on views or state stores.' }],
      }],
    },
  },
])
