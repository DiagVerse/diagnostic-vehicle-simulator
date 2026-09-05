// Thin typed client for the engine API. Uses same-origin relative paths; in dev the Vite
// proxy (see vite.config.ts) forwards them to the engine.

export interface Health {
  status: string
  engine_version: string
  plugin_count: number
}

export interface PluginInfo {
  name: string
  kind: string
  version: string
  description: string
  path: string
}

async function getJson<T>(path: string): Promise<T> {
  const res = await fetch(path)
  if (!res.ok) {
    throw new Error(`${path} → HTTP ${res.status}`)
  }
  return (await res.json()) as T
}

export const api = {
  health: () => getJson<Health>('/health'),
  plugins: () => getJson<PluginInfo[]>('/plugins'),
}
