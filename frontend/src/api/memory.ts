import { invoke } from '@tauri-apps/api/core'

interface MemoryApiError extends Error {
  status?: number
}

export async function requestMemory<T>(path: string, body: unknown): Promise<T> {
  try {
    const data = await invoke<T & { code?: string, message?: string }>('memory_backend_request', { path, body })
    if (data.code && data.code !== 'ok' && data.code !== 'queued') {
      const err = new Error(data.message || 'memory API request failed') as MemoryApiError
      throw err
    }
    return data
  } catch (cause) {
    const err = cause instanceof Error ? cause as MemoryApiError : new Error(String(cause)) as MemoryApiError
    throw err
  }
}
