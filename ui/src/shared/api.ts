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

/** Current state of the demo virtual ECU. */
export interface EcuState {
  name: string
  logicalAddress: number
  session: number
  sessionName: string
  securityUnlocked: boolean
  securityLevel: number
  supportedServices: number[]
  dids: number[]
  dtcCount: number
  protocolLoaded: boolean
}

/** Result of sending one UDS request. */
export interface RequestResult {
  requestHex: string
  responseHex: string
  suppressed: boolean
  session: number
  sessionName: string
  securityUnlocked: boolean
  error: string | null
}

async function getJson<T>(path: string): Promise<T> {
  const res = await fetch(path)
  if (!res.ok) {
    throw new Error(`${path} → HTTP ${res.status}`)
  }
  return (await res.json()) as T
}

async function postJson<T>(path: string, body: unknown): Promise<T> {
  const res = await fetch(path, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  })
  if (!res.ok) {
    throw new Error(`${path} → HTTP ${res.status}`)
  }
  return (await res.json()) as T
}

export const api = {
  health: () => getJson<Health>('/health'),
  plugins: () => getJson<PluginInfo[]>('/plugins'),
  ecuState: () => getJson<EcuState>('/ecu/state'),
  ecuReset: () => postJson<EcuState>('/ecu/reset', {}),
  ecuRequest: (requestHex: string) => postJson<RequestResult>('/ecu/request', { requestHex }),
}
