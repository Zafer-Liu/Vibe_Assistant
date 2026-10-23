import { create } from 'zustand'
import { invoke } from '@tauri-apps/api/core'

export type CloseBehavior = 'ask' | 'background' | 'exit'
export interface DesktopPreferences {
  close_behavior: CloseBehavior
  tray_available: boolean
  close_pending: boolean
}
interface DesktopStore {
  preferences: DesktopPreferences | null
  pending: boolean
  working: boolean
  error: string | null
  accept: (preferences: DesktopPreferences) => void
  refresh: (language: string) => Promise<void>
  save: (closeBehavior: CloseBehavior) => Promise<void>
  resolve: (action: Exclude<CloseBehavior, 'ask'>) => Promise<void>
  cancel: () => Promise<void>
}
const message = (error: unknown) => String(error)
export const useDesktopStore = create<DesktopStore>((set, get) => ({
  preferences: null, pending: false, working: false, error: null,
  accept: preferences => set({ preferences, pending: preferences.close_pending, error: null }),
  refresh: async language => {
    try { get().accept(await invoke<DesktopPreferences>('desktop_preferences_get', { language })) }
    catch (error) { set({ error: message(error) }) }
  },
  save: async closeBehavior => {
    if (get().working) return
    set({ working: true, error: null })
    try { get().accept(await invoke<DesktopPreferences>('desktop_preferences_set', { closeBehavior })) }
    catch (error) { set({ error: message(error) }) }
    finally { set({ working: false }) }
  },
  resolve: async action => {
    if (get().working) return
    set({ working: true, error: null })
    try { get().accept(await invoke<DesktopPreferences>('desktop_close_resolve', { action })) }
    catch (error) { set({ error: message(error) }) }
    finally { set({ working: false }) }
  },
  cancel: async () => {
    if (get().working) return
    set({ working: true, error: null })
    try { await invoke('desktop_close_cancel'); set({ pending: false }) }
    catch (error) { set({ error: message(error) }) }
    finally { set({ working: false }) }
  },
}))
