// Thin typed client for the engine API. Uses same-origin relative paths; in dev the Vite
// proxy (see vite.config.ts) forwards them to the engine.

export interface Health {
  status: string
  engine_version: string
  /**
   * The commit the running engine was built from, with a `+` when the tree was dirty.
   *
   * Shown in the header because a fix that is not in the running binary looks exactly like a
   * fix that does not work, and there was no way to tell those apart from the browser.
   */
  build_commit: string
  /** When that binary was built, as seconds since the epoch. */
  built_at_secs: number
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
 * advertises and is judged against; the fault-injection knobs make the ECU actually slow,
 * actually send NRC 0x78 ResponsePending, or actually never finish.
 *
 * `isoTpBlockSize` / `isoTpSeparationTimeMin` are different in kind: they belong to
 * ISO 15765-2 rather than ISO 14229, and they govern the *request* direction — how fast this
 * ECU lets a tester send it a multi-frame message.
 */
export interface EcuTiming {
  p2ServerMaxMs: number
  p2StarServerMaxMs: number
  p4ServerMaxMs: number
  responseDelayMs: number
  forceResponsePending: boolean
  forcedResponsePendingCount: number
  dropFinalResponse: boolean
  /** ConsecutiveFrames accepted per FlowControl; 0 means "send them all without pausing". */
  isoTpBlockSize: number
  /** Raw STmin byte: 0x00-0x7F milliseconds, 0xF1-0xF9 hundreds of microseconds. */
  isoTpSeparationTimeMin: number
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
  /** How this ECU is named in a URL: its CAN identifier, or `doip-1234`. */
  handle: string
  /** The identifier a tester addresses it on. Null for an ECU reachable only over DoIP. */
  requestCanIdHex: string | null
  /** The identifier it answers on. Null for an ECU reachable only over DoIP. */
  responseCanIdHex: string | null
  /** Its DoIP logical address in hex, when that address is a routable one. */
  logicalAddressHex: string | null
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
  /** Whether the ECU is switched on. Off keeps its configuration and answers nothing. */
  isEnabled: boolean
}

/** What the engine currently has loaded. */
export interface SimulationState {
  loaded: boolean
  /** False when the simulation is stopped: still loaded, still stateful, but off the bus. */
  running: boolean
  vehicleName: string | null
  protocolLoaded: boolean
  ecus: SimulationEcu[]
  /**
   * True when the ECUs are not enforcing their own gates — session restrictions, security
   * locks, services absent from the supported list — so a configured response is always
   * reachable. It never fabricates an answer.
   */
  permissiveMode: boolean
  /**
   * True when the vehicle has been edited since it was last written to a file. A freshly
   * loaded one is not unsaved — it came from somewhere you still have.
   */
  unsavedChanges: boolean
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
  /** True when one of your response overrides produced this answer, not the UDS plugin. */
  overridden: boolean
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
  /** The ECU that would have answered had it been switched on. Only for 'silenced'. */
  silencedEcuName: string | null
  /** Why nothing answered. Only for 'silenced'. */
  silencedReason: string | null
}

/** Whether the simulation is reachable over Ethernet. */
export interface DoIpStatus {
  running: boolean
  /** The address actually bound, which differs from the request when port 0 was asked for. */
  boundAddress: string | null
  entityAddressHex: string | null
  /** Every logical address the loaded vehicle answers on, so you know what to target. */
  logicalAddressesHex: string[]
}

/** What a vehicle tells a DoIP tester about itself (ISO 13400-2 Table 5). */
export interface VehicleIdentity {
  /** 17 characters. Null means not programmed, announced as such rather than as a wrong VIN. */
  vin: string | null
  /** Six bytes of entity identification, in hex. */
  eidHex: string | null
  /** Six bytes of group identification, in hex. */
  gidHex: string | null
  /** Table 6: 0x00 no further action, 0x10 central security required. */
  furtherActionRequired: number
  /** Table 7: 0x00 synchronized, 0x10 not — which tells a tester to wait and ask again. */
  vinGidSyncStatus: number
}

/** What the DoIP entity says about itself, and what it has been told to say instead. */
export interface DoIpSettings {
  /** 0x00 not ready, 0x01 ready, 0x02 not supported. */
  powerMode: number
  /** 0x00 gateway, 0x01 node. */
  nodeType: number
  /** Sockets reported, excluding the reserve the standard requires. */
  maxSockets: number
  /** Reported and enforced — the two must agree. */
  maxDataSize: number
  /** Answer nothing to a vehicle identification request. */
  suppressIdentificationResponse: boolean
  /** Force this routing activation response code. Null to decide normally. */
  forcedRoutingActivationCode: number | null
  /** NACK every diagnostic message with this code. Null to route normally. */
  forcedDiagnosticNack: number | null
  /** Refuse every message with this header NACK code. Null to read them normally. */
  forcedHeaderNack: number | null
  /** True when any of the above makes the entity behave as a healthy one would not. */
  isInjectingFaults: boolean
}

/** A serial port this machine offers. */
export interface SerialPort {
  name: string
  description: string
}

/** What `GET /hw/ports` answers. */
export interface SerialPorts {
  /** False when the build has no serial support: an empty list then means that, not "nothing plugged in". */
  serialSupported: boolean
  ports: SerialPort[]
}

/** Whether the simulation is on a wire, and how much has crossed it. */
export interface HardwareStatus {
  running: boolean
  port: string | null
  bitrateBps: number | null
  /**
   * Host-to-adapter line speed, NOT the CAN bitrate. Determined when the link is opened, by
   * asking the adapter what it runs at; `serialBaudSource` says whether that number was
   * measured, chosen by the operator, or merely assumed.
   */
  serialBaudBps: number | null
  serialBaudSource: string | null
  adapterVersion: string | null
  framesReceived: number
  framesSent: number
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
/** One second of bus activity. */
export interface BusLoadSample {
  atSec: number
  frames: number
  /** Share of the bus occupied, 0–1, with bit stuffing excluded. */
  loadNominal: number
  /** The same with every frame stuffed as heavily as the standard allows. */
  loadWorstCase: number
}

/**
 * How busy the bus has been.
 *
 * `loadNominal` is a floor, not a measurement: bit stuffing depends on the payload's bit
 * pattern and on a CRC a decoded frame does not carry, so the real figure lies between the
 * nominal and the worst case. Showing one number would be showing a number that is subtly
 * wrong; showing the range is honestly right.
 */
export interface BusLoad {
  bitrateBps: number
  samples: BusLoadSample[]
  /** Incomplete by definition — the second still being filled. */
  current: BusLoadSample | null
  peak: BusLoadSample | null
  isNominalAFloor: boolean
}

/** An ECU's CAN error counters and the bus state they put it in (ISO 11898-1 §12.1). */
export interface BusState {
  state: 'errorActive' | 'errorPassive' | 'busOff'
  transmitErrorCount: number
  receiveErrorCount: number
  /** False once the node is off the wire: requests to it time out rather than being refused. */
  canTransmit: boolean
}

export interface SetBusState {
  state?: 'errorActive' | 'errorPassive' | 'busOff'
  transmitErrorCount?: number
  receiveErrorCount?: number
}

/** A vehicle written out as a simulation file. */
export interface SimFileExport {
  fileName: string
  content: string
}

/**
 * One SecurityAccess level.
 *
 * `keyPolicy` says what the ECU does with the key a tester sends:
 * - `compare`   — check it against `expectedKeyHex`; NRC 0x35 on a mismatch. For a level whose
 *                 key is genuinely known.
 * - `acceptAny` — unlock whatever arrives. The honest choice for a level taken from a capture:
 *                 the seed was observed, the key never usefully was, and a fresh session issues
 *                 a fresh seed that any recorded key is wrong for.
 * - `refuse`    — never unlock; answer every key with `refusalNrcHex`. Fault injection.
 *
 * A key that arrives with no preceding requestSeed is refused with NRC 0x24 under every policy:
 * ISO 14229-1 makes that a sequence rule, not a key rule.
 */
export interface SecurityLevel {
  /** requestSeed sub-function in hex, e.g. "01". Odd; the even value above it is sendKey. */
  requestSeedHex: string
  seedHex: string
  /** Only meaningful under `compare`. */
  expectedKeyHex: string
  keyPolicy: 'compare' | 'acceptAny' | 'refuse'
  /** The code `refuse` answers with, in hex. Defaults to 35 (invalidKey). */
  refusalNrcHex: string | null
}

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
  /** True for a link a tester attaches to directly; everything else hangs off a gateway. */
  isEntryPoint: boolean
  /** Gateways crossed to reach it. 0 for an entry point, null when nothing reaches it. */
  depth: number | null
  /** The node id of the ECU that forwards onto this link. */
  reachedViaNodeId: string | null
}

/** One node hanging off a link. */
export interface TopologyNode {
  id: string
  label: string
  /** How this ECU is named in a URL. Null for the tester node, which is drawn, not driven. */
  handle: string | null
  /** 'ecu' or 'tester'. */
  kind: string
  linkId: string | null
  requestCanIdHex: string | null
  responseCanIdHex: string | null
  addressingMode: string | null
  addressConfidence: string | null
  isUnreachable: boolean
  /** Why, in plain words, when `isUnreachable` is set. */
  unreachableReason: string | null
  /** Its DoIP logical address in hex, when it has one. */
  logicalAddressHex: string | null
  /** Which transports address it: 'CAN', 'DoIP', or both. */
  transports: string[]
  /** The links it forwards onto, making it a gateway. */
  gatewayForLinkIds: string[]
  /** The gateways a tester crosses to reach it, nearest the tester first. */
  reachedViaEcuNames: string[]
  hopCount: number
  /** False when the ECU is declared but the engine cannot drive it on the wire yet. */
  isSimulated: boolean
  /** Whether the ECU is switched on. A switched-off ECU answers nothing at all. */
  isEnabled: boolean
  /** The gateway between it and the tester that is switched off, when one is. */
  blockedByEcuName: string | null
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
  /** The id of the network it sits on. Left out means nobody has said. */
  networkId?: string | null
  /** The networks it forwards diagnostics onto, making it a gateway. */
  gatewayForNetworkIds?: string[]
}

/** A bus to declare on the loaded vehicle. */
export interface NewNetwork {
  id: string
  name: string
  /** 'CAN', 'CAN-FD' or 'Ethernet'. */
  kind: string
  bitrateBps?: number | null
  dataBitrateBps?: number | null
  /** True for the link a tester attaches to directly. */
  entryPoint?: boolean
}

/** Where one ECU sits, and what it gateways onto. */
export interface EcuPlacement {
  networkId: string | null
  gatewayForNetworkIds: string[]
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

/**
 * Send raw bytes.
 *
 * A capture is binary and can be tens of megabytes. Base64 would cost a third more on the wire
 * and force the whole file through a JavaScript string on the way — so it goes as it is.
 */
async function postBinary<T>(path: string, arrBody: ArrayBuffer): Promise<T> {
  const res = await fetch(path, {
    method: 'POST',
    headers: { 'content-type': 'application/octet-stream' },
    body: arrBody,
  })
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
  simulationRemoveEcu: (handle: string) =>
    deleteJson<SimulationState>(`/simulation/ecus/${handle}`),
  simulationRenameEcu: (handle: string, name: string) =>
    putJson<SimulationState>(`/simulation/ecus/${handle}/name`, { name }),
  simulationLoad: (logText: string) => postJson<SimulationState>('/simulation/load', { logText }),
  simulationLoadSimFile: (logText: string) =>
    postJson<SimulationState>('/simulation/simfile', { logText }),
  simulationLoadCapture: (arrCapture: ArrayBuffer) =>
    postBinary<SimulationState>('/simulation/pcap', arrCapture),
  simulationReset: () => postJson<SimulationState>('/simulation/reset', {}),
  simulationStart: () => postJson<SimulationState>('/simulation/start', {}),
  simulationStop: () => postJson<SimulationState>('/simulation/stop', {}),
  simulationRequest: (canIdHex: string, requestHex: string) =>
    postJson<SimulationRequestResult>('/simulation/request', { canIdHex, requestHex }),
  simulationTopology: () => getJson<Topology>('/simulation/topology'),
  simulationDeclareNetwork: (network: NewNetwork) =>
    postJson<Topology>('/simulation/networks', network),
  simulationRemoveNetwork: (networkId: string) =>
    deleteJson<Topology>(`/simulation/networks/${encodeURIComponent(networkId)}`),
  simulationSetEcuPlacement: (handle: string, placement: EcuPlacement) =>
    putJson<Topology>(`/simulation/ecus/${handle}/placement`, placement),
  simulationSetEcuEnabled: (handle: string, enabled: boolean) =>
    putJson<SimulationState>(`/simulation/ecus/${handle}/enabled`, { enabled }),
  doipStatus: () => getJson<DoIpStatus>('/doip/status'),
  doipStart: (bind: string) => postJson<DoIpStatus>('/doip/start', { bind }),
  doipStop: () => postJson<DoIpStatus>('/doip/stop', {}),
  vehicleIdentity: () => getJson<VehicleIdentity>('/simulation/identity'),
  setVehicleIdentity: (identity: VehicleIdentity) =>
    putJson<VehicleIdentity>('/simulation/identity', identity),
  doipSettings: () => getJson<DoIpSettings>('/doip/settings'),
  setDoIpSettings: (settings: DoIpSettings) => putJson<DoIpSettings>('/doip/settings', settings),
  serialPorts: () => getJson<SerialPorts>('/hw/ports'),
  hardwareStatus: () => getJson<HardwareStatus>('/hw/status'),
  /**
   * `serialBaudBps` is the host-to-adapter line speed. Leave it undefined and the engine asks
   * the adapter what it runs at, which is right almost always.
   */
  hardwareStart: (port: string, bitrateBps: number, serialBaudBps?: number) =>
    postJson<HardwareStatus>('/hw/start', { port, bitrateBps, serialBaudBps }),
  hardwareStop: () => postJson<HardwareStatus>('/hw/stop', {}),
  busLoad: () => getJson<BusLoad>('/hw/busload'),
  ecuBusState: (handle: string) => getJson<BusState>(`/simulation/ecus/${handle}/bus-state`),
  setEcuBusState: (handle: string, body: SetBusState) =>
    putJson<BusState>(`/simulation/ecus/${handle}/bus-state`, body),
  /** The loaded vehicle as simulation-file text, ready to save. Clears the unsaved marker. */
  simulationExport: () => getJson<SimFileExport>('/simulation/export'),
  setPermissiveMode: (enabled: boolean) =>
    postJson<SimulationState>('/simulation/permissive', { enabled }),
  ecuSecurityLevels: (handle: string) =>
    getJson<SecurityLevel[]>(`/simulation/ecus/${handle}/security`),
  setEcuSecurityLevels: (handle: string, levels: SecurityLevel[]) =>
    putJson<SecurityLevel[]>(`/simulation/ecus/${handle}/security`, { levels }),
  ecuOverrides: (handle: string) =>
    getJson<ResponseOverride[]>(`/simulation/ecus/${handle}/overrides`),
  setEcuOverrides: (handle: string, overrides: ResponseOverride[]) =>
    putJson<ResponseOverride[]>(`/simulation/ecus/${handle}/overrides`, { overrides }),
  ecuTiming: (handle: string) =>
    getJson<EcuTiming>(`/simulation/ecus/${handle}/timing`),
  setEcuTiming: (handle: string, timing: EcuTiming) =>
    putJson<EcuTimingUpdate>(`/simulation/ecus/${handle}/timing`, timing),
}
