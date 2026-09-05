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

// ---------------------------------------------------------------------------------------
// Simulation: a vehicle reconstructed from a CAN log, driven by CAN address.
// ---------------------------------------------------------------------------------------

/** One ECU running inside the loaded simulation. */
export interface SimulationEcu {
  name: string
  logicalAddress: number
  requestCanIdHex: string
  responseCanIdHex: string
  /** The broadcast identifier this ECU also listens on, if any. */
  functionalCanIdHex: string | null
  addressingMode: string
  /** How the identifier pair was established: Observed, Inferred, … */
  addressConfidence: string
  session: number
  sessionName: string
  securityUnlocked: boolean
  securityLevel: number
  supportedServices: number[]
  dids: number[]
  dtcCount: number
}

/** What the engine currently has loaded. */
export interface SimulationState {
  loaded: boolean
  vehicleName: string | null
  protocolLoaded: boolean
  ecus: SimulationEcu[]
}

/** One ECU's answer to a routed request. */
export interface SimulationResponse {
  ecuName: string
  responseCanIdHex: string
  responseHex: string
  suppressed: boolean
  session: number
  sessionName: string
  securityUnlocked: boolean
}

/**
 * The outcome of one routed request. `responses` holds one entry per ECU that answered: at
 * most one for a physically addressed request, one per listening ECU for a broadcast. It can
 * be empty while `routed` is true — every listener was required to stay silent.
 */
export interface SimulationRequestResult {
  canIdHex: string
  requestHex: string
  addressing: string
  routed: boolean
  responses: SimulationResponse[]
}

/** The engine returns a JSON body with an `error` field for a 4xx. */
interface ApiErrorBody {
  error?: string
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
    throw new Error(await describeFailure(path, res))
  }
  return (await res.json()) as T
}

/**
 * Turn a failed response into a message worth showing. The engine explains rejections in an
 * `error` field ("no CAN frames found in log…"), which is far more useful than the status
 * code alone; fall back to the code when the body is not the shape we expect.
 */
async function describeFailure(path: string, res: Response): Promise<string> {
  try {
    const body = (await res.json()) as ApiErrorBody
    if (typeof body.error === 'string' && body.error.length > 0) {
      return body.error
    }
  } catch {
    // Not a JSON body (a proxy error page, an empty response): fall through to the status.
  }
  return `${path} → HTTP ${res.status}`
}

export const api = {
  health: () => getJson<Health>('/health'),
  plugins: () => getJson<PluginInfo[]>('/plugins'),
  ecuState: () => getJson<EcuState>('/ecu/state'),
  ecuReset: () => postJson<EcuState>('/ecu/reset', {}),
  ecuRequest: (requestHex: string) => postJson<RequestResult>('/ecu/request', { requestHex }),

  simulationState: () => getJson<SimulationState>('/simulation/state'),
  simulationLoad: (logText: string) => postJson<SimulationState>('/simulation/load', { logText }),
  simulationReset: () => postJson<SimulationState>('/simulation/reset', {}),
  simulationRequest: (canIdHex: string, requestHex: string) =>
    postJson<SimulationRequestResult>('/simulation/request', { canIdHex, requestHex }),
}
