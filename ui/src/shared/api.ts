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

/**
 * An ECU's UDS server timing (ISO 14229-2 clause 7), in milliseconds.
 *
 * `p2ServerMaxMs` / `p2StarServerMaxMs` / `p4ServerMaxMs` are the parameters the ECU
 * advertises and is judged against; the rest are fault-injection knobs that make the ECU
 * actually slow, actually send NRC 0x78 ResponsePending, or actually never finish.
 */
export interface EcuTiming {
  p2ServerMaxMs: number
  p2StarServerMaxMs: number
  p4ServerMaxMs: number
  responseDelayMs: number
  forceResponsePending: boolean
  forcedResponsePendingCount: number
  dropFinalResponse: boolean
}

/** The result of changing an ECU's timing. */
export interface EcuTimingUpdate extends EcuTiming {
  /** ISO 14229-1 carries P2/P2* only in the DiagnosticSessionControl response. */
  advertisedAtNextSessionControl: boolean
}

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
  timing: EcuTiming
}

/** What the engine currently has loaded. */
export interface SimulationState {
  loaded: boolean
  /** False when the simulation is stopped: still loaded, still stateful, but off the bus. */
  running: boolean
  vehicleName: string | null
  protocolLoaded: boolean
  ecus: SimulationEcu[]
}

/** One message an ECU put on the wire, with both its scheduled and its measured offset. */
export interface SimulationFrame {
  atMs: number
  actualMs: number
  hex: string
  /** 'responsePending' for a NRC 0x78, 'final' for the final response. */
  kind: string
}

/** One ECU's answer to a routed request, which may be several messages over time. */
export interface SimulationResponse {
  ecuName: string
  requestCanIdHex: string
  responseCanIdHex: string
  responseHex: string
  suppressed: boolean
  session: number
  sessionName: string
  securityUnlocked: boolean
  frames: SimulationFrame[]
  finalAtMs: number | null
  responsePendingCount: number
  finalResponseDropped: boolean
  isoConformant: boolean
  conformanceWarnings: string[]
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

/** A run of request bytes copied into the response, so a wildcard override still echoes. */
export interface EchoSpan {
  requestOffset: number
  length: number
  responseOffset: number
}

/**
 * A user-defined answer to a request.
 *
 * Declaring a service supported does not implement it — the engine's UDS plugin answers seven
 * services, and an override is the only way to get a positive response out of the rest.
 */
export interface ResponseOverride {
  /** Bytes to match; a byte written `**` is a wildcard, e.g. `22 ** **`. */
  requestHex: string
  matchTrailingBytes: boolean
  /** 'substitute' or 'suppress'. */
  action: string
  responseHex?: string | null
  echoSpans: EchoSpan[]
  enabled: boolean
  respondEvenIfSuppressed: boolean
  note: string
}

/** One link in the topology view. Deliberately not called a bus — see `caveats`. */
export interface TopologyLink {
  id: string
  label: string
  kind: string
  functionalCanIdsHex: string[]
  membershipConfidence: string
}

/** One node hanging off a link. */
export interface TopologyNode {
  id: string
  label: string
  /** 'ecu' or 'tester'. */
  kind: string
  linkId: string | null
  requestCanIdHex: string | null
  responseCanIdHex: string | null
  addressingMode: string | null
  addressConfidence: string | null
  isUnreachable: boolean
}

/** The diagram, plus what it cannot know. */
export interface Topology {
  vehicleName: string | null
  links: TopologyLink[]
  nodes: TopologyNode[]
  caveats: string[]
}

/**
 * An ECU to add to the loaded vehicle. Only a name and the identifier pair are required: the
 * addressing mode follows from the identifier width, and the capability set defaults to
 * everything the engine's UDS plugin implements.
 */
export interface NewEcu {
  name: string
  requestCanIdHex: string
  responseCanIdHex: string
  addressingMode?: string
  supportedServices?: number[]
  logicalAddress?: number
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
  return sendJson<T>('POST', path, body)
}

async function putJson<T>(path: string, body: unknown): Promise<T> {
  return sendJson<T>('PUT', path, body)
}

async function deleteJson<T>(path: string): Promise<T> {
  const res = await fetch(path, { method: 'DELETE' })
  if (!res.ok) {
    throw new Error(await describeFailure(path, res))
  }
  return (await res.json()) as T
}

async function sendJson<T>(method: string, path: string, body: unknown): Promise<T> {
  const res = await fetch(path, {
    method,
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
  simulationCreateVehicle: (name: string) =>
    postJson<SimulationState>('/simulation/vehicle', { name }),
  simulationAddEcu: (ecu: NewEcu) => postJson<SimulationState>('/simulation/ecus', ecu),
  simulationRemoveEcu: (requestCanIdHex: string) =>
    deleteJson<SimulationState>(`/simulation/ecus/${requestCanIdHex}`),
  simulationRenameEcu: (requestCanIdHex: string, name: string) =>
    putJson<SimulationState>(`/simulation/ecus/${requestCanIdHex}/name`, { name }),
  simulationLoad: (logText: string) => postJson<SimulationState>('/simulation/load', { logText }),
  simulationReset: () => postJson<SimulationState>('/simulation/reset', {}),
  simulationStart: () => postJson<SimulationState>('/simulation/start', {}),
  simulationStop: () => postJson<SimulationState>('/simulation/stop', {}),
  simulationRequest: (canIdHex: string, requestHex: string) =>
    postJson<SimulationRequestResult>('/simulation/request', { canIdHex, requestHex }),
  simulationTopology: () => getJson<Topology>('/simulation/topology'),
  ecuOverrides: (requestCanIdHex: string) =>
    getJson<ResponseOverride[]>(`/simulation/ecus/${requestCanIdHex}/overrides`),
  setEcuOverrides: (requestCanIdHex: string, overrides: ResponseOverride[]) =>
    putJson<ResponseOverride[]>(`/simulation/ecus/${requestCanIdHex}/overrides`, { overrides }),
  ecuTiming: (requestCanIdHex: string) =>
    getJson<EcuTiming>(`/simulation/ecus/${requestCanIdHex}/timing`),
  setEcuTiming: (requestCanIdHex: string, timing: EcuTiming) =>
    putJson<EcuTimingUpdate>(`/simulation/ecus/${requestCanIdHex}/timing`, timing),
}
