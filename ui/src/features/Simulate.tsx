import { useCallback, useEffect, useState } from 'react'
import { Badge, DetailRow, PowerSwitch, type BadgeTone } from '../components/primitives'
import {
  NEGATIVE_RESPONSES,
  UDS_CATALOGUE,
  VARIABLE_TAIL_SERVICES,
  type CatalogueService,
  type CatalogueVariant,
} from './udsCatalogue'
import { TrafficMonitor } from './TrafficMonitor'
import {
  api,
  type EcuTiming,
  type NewEcu,
  type ResponseOverride,
  type SecurityLevel,
  type SimulationEcu,
  type SimulationRequestResult,
  type SimulationResponse,
  type SimulationState,
  type TopologyLink,
} from '../shared/api'

/** A quick action maps a human label to a fixed UDS request (hex). */
const QUICK_ACTIONS: { label: string; hex: string }[] = [
  { label: 'Read VIN', hex: '22 F1 90' },
  { label: 'Read DTCs', hex: '19 02 FF' },
  { label: 'Enter Extended', hex: '10 03' },
  { label: 'Enter Default', hex: '10 01' },
  { label: 'Tester Present', hex: '3E 00' },
]

/**
 * Load a CAN log into the engine and drive the reconstructed vehicle: pick the CAN identifier
 * to address, send a UDS request, watch each ECU answer and change state.
 */
export function Simulate() {
  const [state, setState] = useState<SimulationState | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [lastResult, setLastResult] = useState<SimulationRequestResult | null>(null)
  const [canIdHex, setCanIdHex] = useState(RememberedEcu)
  const [hexInput, setHexInput] = useState('22 F1 90')
  const [busy, setBusy] = useState(false)
  const [source, setSource] = useState<VehicleSource>('log')

  useEffect(() => {
    refreshState()
  }, [])

  // The address actually in use is derived, not stored: `canIdHex` is only what the user
  // picked, and a newly loaded vehicle may not have that identifier at all. Deriving it here
  // means loading a different log cannot leave a stale selection behind.
  const vecAddressOptions = AddressOptions(state)
  const bIsSelectionValid = vecAddressOptions.some((option) => option.canIdHex === canIdHex)
  const strSelectedCanId = bIsSelectionValid
    ? canIdHex
    : (vecAddressOptions[0]?.canIdHex ?? '')

  // Switching tabs unmounts this view, so without remembering the choice it comes back on the
  // first ECU of the list. That reads as "my override disappeared" — the override is on the
  // server, the panel is just describing a different ECU — and it is the same trap that makes
  // a timing change look like it was ignored.
  function selectEcu(strCanIdHex: string) {
    setCanIdHex(strCanIdHex)
    RememberEcu(strCanIdHex)
  }

  useEffect(() => {
    if (strSelectedCanId) RememberEcu(strSelectedCanId)
  }, [strSelectedCanId])

  async function refreshState() {
    try {
      setState(await api.simulationState())
      setError(null)
    } catch (e) {
      setError(DescribeError(e))
    }
  }

  async function load(logText: string) {
    setBusy(true)
    try {
      setState(await api.simulationLoad(logText))
      setLastResult(null)
      setError(null)
    } catch (e) {
      // A rejected log leaves the previously loaded vehicle running, so the view stays usable.
      setError(DescribeError(e))
    } finally {
      setBusy(false)
    }
  }

  /**
   * Load a pcap or pcapng capture.
   *
   * The only source that arrives as binary, so it is base64-encoded here — the API client sends
   * JSON, and a capture is not text.
   */
  async function loadCapture(arrBytes: ArrayBuffer) {
    setBusy(true)
    try {
      setState(await api.simulationLoadCapture(arrBytes))
      setLastResult(null)
      setError(null)
    } catch (e) {
      // A capture with no DoIP traffic in it, or one that is not a capture at all, leaves the
      // previously loaded vehicle running and says which it was.
      setError(DescribeError(e))
    } finally {
      setBusy(false)
    }
  }

  async function loadSimFile(fileText: string) {
    setBusy(true)
    try {
      setState(await api.simulationLoadSimFile(fileText))
      setLastResult(null)
      setError(null)
    } catch (e) {
      // A rejected file leaves the previously loaded vehicle running, and the engine's message
      // names the ECU and the field it could not read.
      setError(DescribeError(e))
    } finally {
      setBusy(false)
    }
  }

  async function send(requestHex: string) {
    if (!strSelectedCanId) {
      setError('Pick a CAN identifier to address first.')
      return
    }
    setBusy(true)
    try {
      const result = await api.simulationRequest(strSelectedCanId, requestHex)
      setLastResult(result)
      setError(null)
      await refreshState()
    } catch (e) {
      setError(DescribeError(e))
    } finally {
      setBusy(false)
    }
  }

  async function toggleRunning() {
    setBusy(true)
    try {
      setState(await (state?.running ? api.simulationStop() : api.simulationStart()))
      setError(null)
    } catch (e) {
      setError(DescribeError(e))
    } finally {
      setBusy(false)
    }
  }

  async function reset() {
    setBusy(true)
    try {
      setState(await api.simulationReset())
      setLastResult(null)
      setError(null)
    } catch (e) {
      setError(DescribeError(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="space-y-6">
      {error && (
        <div className="rounded-lg border border-red-900/60 bg-red-950/40 px-4 py-3 text-sm text-red-300">
          {error}
        </div>
      )}

      {state?.loaded && !state.running && (
        <div className="rounded-lg border border-amber-900/60 bg-amber-950/30 px-4 py-3 text-sm text-amber-300">
          Simulation stopped — the ECUs are off the bus and answer nothing. Their sessions,
          security state and overrides are kept, so starting again resumes where it left off.
        </div>
      )}

      {state && !state.protocolLoaded && (
        <div className="rounded-lg border border-amber-900/60 bg-amber-950/30 px-4 py-3 text-sm text-amber-300">
          UDS protocol plugin not loaded — copy <code>libuds_plugin.*</code> into{' '}
          <code>plugins.d/</code> and restart the engine.
        </div>
      )}

      <SourcePicker source={source} onChange={setSource} />

      {source === 'log' && <LogLoader onLoad={load} busy={busy} />}
      {source === 'simfile' && <SimFileLoader onLoad={loadSimFile} busy={busy} />}
      {source === 'pcap' && <CaptureLoader onLoad={loadCapture} busy={busy} />}
      {source === 'build' && (
        <VehicleBuilder onChanged={setState} onError={setError} busy={busy} />
      )}

      {!state?.loaded ? (
        <EmptyState />
      ) : (
        <div className="grid gap-6 lg:grid-cols-[360px_1fr]">
          <EcuList
            state={state}
            onReset={reset}
            onToggleRunning={toggleRunning}
            onChanged={setState}
            onError={setError}
            busy={busy}
          />

          <section className="space-y-4">
            <RequestPanel
              options={vecAddressOptions}
              canIdHex={strSelectedCanId}
              onCanIdChange={selectEcu}
              hexInput={hexInput}
              onHexInputChange={setHexInput}
              onSend={send}
              busy={busy}
            />
            {/* Keyed by the ECU so switching address remounts the form: an unsaved draft for
                one ECU must never be applied to another. */}
            <OverridePanel
              key={`ov-${strSelectedCanId}`}
              ecu={FindEcuByRequestCanId(state, strSelectedCanId)}
              onError={setError}
              busy={busy}
            />
            <SecurityPanel
              key={`sec-${strSelectedCanId}`}
              ecu={FindEcuByRequestCanId(state, strSelectedCanId)}
              onSaved={refreshState}
              onError={setError}
              busy={busy}
            />
            <TimingPanel
              key={strSelectedCanId}
              ecu={FindEcuByRequestCanId(state, strSelectedCanId)}
              onSaved={refreshState}
              onError={setError}
              busy={busy}
            />
            {lastResult && <LastExchangeDetail result={lastResult} />}
            <TrafficMonitor />
          </section>
        </div>
      )}
    </div>
  )
}

// ---------------------------------------------------------------------------------------
// Where the vehicle comes from
// ---------------------------------------------------------------------------------------

/**
 * Turn the ECUs' own gates off for the whole vehicle.
 *
 * One switch, not one per ECU: a tester's sequence crosses several ECUs, and having it stop at
 * whichever one was left strict is the problem this removes.
 */
function PermissiveSwitch({
  state,
  onChanged,
  onError,
  busy,
}: {
  state: SimulationState
  onChanged: (state: SimulationState) => void
  onError: (message: string | null) => void
  busy: boolean
}) {
  const [working, setWorking] = useState(false)

  async function toggle(enabled: boolean) {
    setWorking(true)
    try {
      onChanged(await api.setPermissiveMode(enabled))
      onError(null)
    } catch (e) {
      onError(DescribeError(e))
    } finally {
      setWorking(false)
    }
  }

  if (!state.loaded) return null

  return (
    <div className="rounded-md border border-slate-800 bg-slate-900/40 px-3 py-2">
      <label className="flex items-start gap-2 text-xs text-slate-300">
        <input
          type="checkbox"
          checked={state.permissiveMode}
          disabled={busy || working}
          onChange={(e) => void toggle(e.target.checked)}
          className="mt-0.5"
        />
        <span>
          Permissive mode
          <span className="block text-[11px] text-slate-500">
            {state.permissiveMode
              ? 'Gates off: session restrictions, security locks and the supported-service list are not enforced, so anything you have configured is reachable. Responses are still only what you configured — nothing is invented.'
              : 'Gates on: each ECU enforces its sessions, security and declared services, as a real one does.'}
          </span>
        </span>
      </label>
    </div>
  )
}

/**
 * Write the loaded vehicle out as a simulation file.
 *
 * Everything configured here — renames, response overrides, security policies, flow control —
 * lives only in the running engine until this is pressed. The file it produces goes back in
 * through Load → Simulation file, so a worked-on vehicle is something you can keep, reload
 * tomorrow, or hand to someone else.
 *
 * A button with a marker rather than a prompt on every change: applying four overrides in a
 * row is one piece of work, and interrupting each of them would make the editor tiring to use.
 */
function SaveVehicleButton({
  state,
  onSaved,
  onError,
  busy,
}: {
  state: SimulationState
  onSaved: (state: SimulationState) => void
  onError: (message: string | null) => void
  busy: boolean
}) {
  const [saving, setSaving] = useState(false)
  const [note, setNote] = useState<string | null>(null)

  async function save() {
    setSaving(true)
    try {
      const exported = await api.simulationExport()
      DownloadTextFile(exported.fileName, exported.content)
      setNote(DescribeSavedFile(exported.fileName, exported.content))
      onError(null)
      // The engine clears its unsaved marker as it hands the file over, so re-read the state
      // rather than assuming: what the engine believes is the thing being displayed.
      onSaved(await api.simulationState())
    } catch (e) {
      onError(DescribeError(e))
    } finally {
      setSaving(false)
    }
  }

  if (!state.loaded) return null

  return (
    <div className="flex items-center gap-2">
      <button
        onClick={save}
        disabled={busy || saving}
        className="rounded-md border border-slate-700 px-3 py-1.5 text-xs text-slate-300 transition hover:border-slate-500 disabled:opacity-40"
      >
        {saving ? 'Saving…' : 'Save vehicle to file'}
      </button>
      {state.unsavedChanges ? (
        <span className="text-xs text-amber-400" title="Changes made here are not in any file yet">
          unsaved changes
        </span>
      ) : (
        <span className="text-xs text-slate-600">{note ?? 'saved'}</span>
      )}
    </div>
  )
}

/**
 * What the written file actually contains, counted from the file itself.
 *
 * Not decoration. "Did my overrides get saved?" is otherwise unanswerable until the file is
 * reloaded, and by then the answer arrives too late to do anything about.
 */
function DescribeSavedFile(strFileName: string, strContent: string): string {
  try {
    const doc = JSON.parse(strContent) as {
      ecus?: { responses?: unknown[]; security?: unknown[] }[]
    }
    const vecEcus = doc.ecus ?? []
    const uOverrides = vecEcus.reduce((total, ecu) => total + (ecu.responses?.length ?? 0), 0)
    const uLevels = vecEcus.reduce((total, ecu) => total + (ecu.security?.length ?? 0), 0)
    return `${strFileName}: ${vecEcus.length} ECUs, ${uOverrides} responses, ${uLevels} security levels`
  } catch {
    return strFileName
  }
}

/** Hand the browser a file to save. */
function DownloadTextFile(strFileName: string, strContent: string) {
  const blob = new Blob([strContent], { type: 'application/json' })
  const strUrl = URL.createObjectURL(blob)
  const anchor = document.createElement('a')
  anchor.href = strUrl
  anchor.download = strFileName

  // Two things that look unnecessary and are not. A detached anchor's click is ignored by
  // Firefox, so the element has to be in the document; and revoking the object URL in the same
  // tick can cancel a download that has not started reading yet. Either one fails silently —
  // no error, no file — which then looks like the save having lost the work.
  anchor.style.display = 'none'
  document.body.appendChild(anchor)
  anchor.click()
  document.body.removeChild(anchor)
  setTimeout(() => URL.revokeObjectURL(strUrl), 10_000)
}

const c_strRememberedEcuKey = 'dvsim.simulate.selectedEcu'

/**
 * The ECU this view was last pointed at.
 *
 * Session storage rather than component state because the tab strip unmounts the whole view;
 * and rather than local storage because an ECU identifier is only meaningful for the vehicle
 * currently loaded, which does not outlive the tab. A stale value is harmless: the selection is
 * still validated against the loaded vehicle before use.
 */
function RememberedEcu(): string {
  try {
    return sessionStorage.getItem(c_strRememberedEcuKey) ?? ''
  } catch {
    // Private browsing and blocked site data both throw here. Losing the selection is a far
    // better outcome than failing to render the tab.
    return ''
  }
}

function RememberEcu(strCanIdHex: string) {
  try {
    sessionStorage.setItem(c_strRememberedEcuKey, strCanIdHex)
  } catch {
    // Nothing to do, and nothing worth telling the operator about.
  }
}

/** A vehicle is either reconstructed from a capture or stated by hand. */
type VehicleSource = 'log' | 'simfile' | 'pcap' | 'build'

function SourcePicker({
  source,
  onChange,
}: {
  source: VehicleSource
  onChange: (source: VehicleSource) => void
}) {
  return (
    <div className="flex gap-1 rounded-lg border border-slate-800 bg-slate-900/50 p-1">
      <SourceTab active={source === 'log'} onClick={() => onChange('log')}>
        From a CAN log
      </SourceTab>
      <SourceTab active={source === 'simfile'} onClick={() => onChange('simfile')}>
        From a simulation file
      </SourceTab>
      <SourceTab active={source === 'pcap'} onClick={() => onChange('pcap')}>
        From a DoIP capture
      </SourceTab>
      <SourceTab active={source === 'build'} onClick={() => onChange('build')}>
        Build from scratch
      </SourceTab>
    </div>
  )
}

function SourceTab({
  active,
  onClick,
  children,
}: {
  active: boolean
  onClick: () => void
  children: React.ReactNode
}) {
  return (
    <button
      onClick={onClick}
      className={`flex-1 rounded-md px-3 py-2 text-sm transition ${
        active ? 'bg-slate-800 text-white' : 'text-slate-400 hover:bg-slate-800/50'
      }`}
    >
      {children}
    </button>
  )
}

/**
 * Start an empty vehicle and add ECUs to it one at a time — for someone working from a wiring
 * diagram rather than a capture.
 */
function VehicleBuilder({
  onChanged,
  onError,
  busy,
}: {
  onChanged: (state: SimulationState) => void
  onError: (message: string | null) => void
  busy: boolean
}) {
  const [vehicleName, setVehicleName] = useState('Bench vehicle')
  const [newBusId, setNewBusId] = useState('')
  const [newBusName, setNewBusName] = useState('')
  const [newBusKind, setNewBusKind] = useState('CAN')
  const [draft, setDraft] = useState<NewEcu>({
    name: '',
    requestCanIdHex: '',
    responseCanIdHex: '',
  })
  const [working, setWorking] = useState(false)
  const [links, setLinks] = useState<TopologyLink[]>([])

  // The buses this vehicle has, so an ECU can be placed as it is added rather than only
  // afterwards. Refreshed whenever the vehicle changes, since starting a new one clears them.
  // A failure leaves the list empty: a vehicle may simply not be loaded yet, and an empty
  // dropdown says "no buses declared", which is the truth in both cases.
  const refreshLinks = useCallback(
    () =>
      api
        .simulationTopology()
        .then((topology) =>
          setLinks(topology.links.filter((link) => link.id !== 'diagnostic-link')),
        )
        .catch(() => setLinks([])),
    [],
  )

  useEffect(() => {
    refreshLinks()
  }, [refreshLinks])

  async function run(action: () => Promise<SimulationState>) {
    setWorking(true)
    try {
      onChanged(await action())
      onError(null)
      await refreshLinks()
      return true
    } catch (e) {
      onError(DescribeError(e))
      return false
    } finally {
      setWorking(false)
    }
  }

  async function addEcu() {
    const ok = await run(() => api.simulationAddEcu(draft))
    if (ok) {
      // The bus is deliberately kept: adding several ECUs to one bus is the common case.
      setDraft({
        name: '',
        requestCanIdHex: '',
        responseCanIdHex: '',
        networkId: draft.networkId,
      })
    }
  }

  async function declareBus() {
    const strId = newBusId.trim()
    if (!strId) return
    const ok = await run(async () => {
      await api.simulationDeclareNetwork({ id: strId, name: newBusName.trim() || strId, kind: newBusKind })
      return api.simulationState()
    })
    if (ok) {
      setNewBusId('')
      setNewBusName('')
    }
  }

  const bCanAdd =
    draft.name.trim().length > 0 &&
    draft.requestCanIdHex.trim().length > 0 &&
    draft.responseCanIdHex.trim().length > 0

  return (
    <section className="rounded-lg border border-slate-800 bg-slate-900/50 p-5">
      <h3 className="text-sm font-medium uppercase tracking-wider text-slate-400">
        Build a vehicle
      </h3>

      <div className="mt-3 flex flex-wrap items-end gap-2">
        <label className="min-w-48 flex-1">
          <span className="text-xs text-slate-400">Vehicle name</span>
          <input
            value={vehicleName}
            onChange={(e) => setVehicleName(e.target.value)}
            className="mt-1 w-full rounded-md border border-slate-700 bg-slate-950 px-3 py-2 text-sm text-slate-200 outline-none focus:border-slate-500"
          />
        </label>
        <button
          onClick={() => run(() => api.simulationCreateVehicle(vehicleName))}
          disabled={busy || working || vehicleName.trim().length === 0}
          className="rounded-md border border-slate-700 bg-slate-800 px-4 py-2 text-sm text-slate-200 transition hover:border-slate-500 disabled:opacity-40"
        >
          Start empty vehicle
        </button>
      </div>

      <p className="mt-2 text-xs text-slate-600">
        Starting a vehicle replaces whatever is loaded. Then add ECUs one at a time — each gets
        every service the engine&rsquo;s UDS plugin implements, so it answers straight away.
      </p>

      <div className="mt-4 flex flex-wrap items-end gap-2 border-t border-slate-800 pt-4">
        <TextField
          label="Bus id"
          placeholder="powertrain"
          value={newBusId}
          onChange={setNewBusId}
        />
        <TextField
          label="Bus name"
          placeholder="Powertrain CAN"
          value={newBusName}
          onChange={setNewBusName}
        />
        <label className="flex flex-col gap-1">
          <span className="text-xs text-slate-400">Kind</span>
          <select
            value={newBusKind}
            onChange={(e) => setNewBusKind(e.target.value)}
            className="rounded-md border border-slate-700 bg-slate-950 px-3 py-2 text-sm text-slate-200 outline-none focus:border-slate-500"
          >
            <option value="CAN">CAN</option>
            <option value="CAN-FD">CAN-FD</option>
            <option value="Ethernet">Ethernet / DoIP</option>
          </select>
        </label>
        <button
          onClick={declareBus}
          disabled={busy || working || newBusId.trim().length === 0}
          className="rounded-md border border-slate-700 bg-slate-800 px-4 py-2 text-sm text-slate-200 transition hover:border-slate-500 disabled:opacity-40"
        >
          Declare bus
        </button>
      </div>

      <p className="mt-2 text-xs text-slate-600">
        Declare the buses first and each ECU can be placed as it is added. Which ECU gateways
        onto which bus is set in the Topology tab, where the whole architecture is visible at
        once.
      </p>

      <div className="mt-4 grid gap-2 sm:grid-cols-[1fr_7rem_7rem_auto] sm:items-end">
        <TextField
          label="ECU name"
          placeholder="Engine"
          value={draft.name}
          onChange={(v) => setDraft({ ...draft, name: v })}
        />
        <TextField
          label="Request id"
          placeholder="7E0"
          mono
          value={draft.requestCanIdHex}
          onChange={(v) => setDraft({ ...draft, requestCanIdHex: v })}
        />
        <TextField
          label="Response id"
          placeholder="7E8"
          mono
          value={draft.responseCanIdHex}
          onChange={(v) => setDraft({ ...draft, responseCanIdHex: v })}
        />
        <label className="flex flex-col gap-1">
          <span className="text-xs text-slate-400">On bus</span>
          <select
            value={draft.networkId ?? ''}
            onChange={(e) => setDraft({ ...draft, networkId: e.target.value || null })}
            className="rounded-md border border-slate-700 bg-slate-950 px-3 py-2 text-sm text-slate-200 outline-none focus:border-slate-500"
          >
            <option value="">nobody has said</option>
            {links.map((link) => (
              <option key={link.id} value={link.id}>
                {link.label}
              </option>
            ))}
          </select>
        </label>
        <button
          onClick={addEcu}
          disabled={busy || working || !bCanAdd}
          className="rounded-md bg-emerald-700 px-4 py-2 text-sm font-medium text-white transition hover:bg-emerald-600 disabled:opacity-40"
        >
          Add ECU
        </button>
      </div>
    </section>
  )
}

function TextField({
  label,
  placeholder,
  value,
  onChange,
  mono,
}: {
  label: string
  placeholder: string
  value: string
  onChange: (value: string) => void
  mono?: boolean
}) {
  return (
    <label className="block">
      <span className="text-xs text-slate-400">{label}</span>
      <input
        value={value}
        placeholder={placeholder}
        onChange={(e) => onChange(e.target.value)}
        className={`mt-1 w-full rounded-md border border-slate-700 bg-slate-950 px-3 py-2 text-sm text-slate-200 outline-none focus:border-slate-500 ${
          mono ? 'font-mono' : ''
        }`}
      />
    </label>
  )
}

// ---------------------------------------------------------------------------------------
// Loading a log
// ---------------------------------------------------------------------------------------

function LogLoader({ onLoad, busy }: { onLoad: (logText: string) => void; busy: boolean }) {
  const [text, setText] = useState('')
  const [fileName, setFileName] = useState<string | null>(null)

  async function readFile(file: File) {
    const content = await file.text()
    setText(content)
    setFileName(file.name)
  }

  return (
    <section className="rounded-lg border border-slate-800 bg-slate-900/50 p-5">
      <div className="flex items-baseline justify-between">
        <h3 className="text-sm font-medium uppercase tracking-wider text-slate-400">
          Load a CAN log
        </h3>
        <span className="text-xs text-slate-500">Vector .asc or candump</span>
      </div>

      <textarea
        value={text}
        onChange={(e) => {
          setText(e.target.value)
          setFileName(null)
        }}
        rows={6}
        spellCheck={false}
        placeholder={'(0.001000) can0 7E0#0210030000000000\n(0.002000) can0 7E8#065003003201F400'}
        className="mt-3 w-full rounded-md border border-slate-700 bg-slate-950 px-3 py-2 font-mono text-xs text-slate-300 outline-none focus:border-slate-500"
      />

      <div className="mt-3 flex flex-wrap items-center gap-3">
        <button
          onClick={() => onLoad(text)}
          disabled={busy || text.trim().length === 0}
          className="rounded-md bg-emerald-700 px-4 py-2 text-sm font-medium text-white transition hover:bg-emerald-600 disabled:opacity-40"
        >
          Load &amp; simulate
        </button>

        <label className="cursor-pointer rounded-md border border-slate-700 bg-slate-800 px-3 py-2 text-sm text-slate-200 transition hover:border-slate-500">
          Choose file…
          <input
            type="file"
            accept=".log,.asc,.txt,.csv"
            className="hidden"
            onChange={(e) => {
              const file = e.target.files?.[0]
              if (file) readFile(file)
            }}
          />
        </label>

        {fileName && <span className="font-mono text-xs text-slate-500">{fileName}</span>}
      </div>
    </section>
  )
}

/**
 * Load a vehicle described in a file.
 *
 * The only source that can state how the ECUs are wired: a capture cannot observe bus
 * membership and someone clicking ECUs together has not been asked, so this is what makes the
 * topology diagram show real buses.
 */
/**
 * Load a packet capture.
 *
 * Deliberately file-only, with no paste box: a capture is binary, and the other two loaders'
 * textarea would be worse than useless here — it would invite pasting something that cannot
 * work and then report a confusing failure.
 */
function CaptureLoader({
  onLoad,
  busy,
}: {
  onLoad: (arrBytes: ArrayBuffer) => void
  busy: boolean
}) {
  const [fileName, setFileName] = useState<string | null>(null)
  const [sizeBytes, setSizeBytes] = useState(0)
  const [arrBytes, setBytes] = useState<ArrayBuffer | null>(null)

  async function readFile(file: File) {
    setFileName(file.name)
    setSizeBytes(file.size)
    setBytes(await file.arrayBuffer())
  }

  return (
    <section className="rounded-lg border border-slate-800 bg-slate-900/50 p-5">
      <div className="flex items-baseline justify-between">
        <h3 className="text-sm font-medium uppercase tracking-wider text-slate-400">
          Load a DoIP capture
        </h3>
        <span className="text-xs text-slate-500">pcap or pcapng</span>
      </div>

      <p className="mt-2 text-xs leading-relaxed text-slate-500">
        Every logical address that answered becomes an ECU, and the vehicle announcement supplies
        the VIN, EID and GID. An address that was asked and never replied is not invented as an
        ECU. Traffic on port 3496 is TLS and cannot be read &mdash; it is counted and reported,
        never guessed at.
      </p>

      <div className="mt-3 flex flex-wrap items-center gap-2">
        <label className="cursor-pointer rounded-md border border-slate-700 bg-slate-800 px-4 py-2 text-sm text-slate-200 transition hover:border-slate-500">
          Choose file&hellip;
          <input
            type="file"
            accept=".pcap,.pcapng,.cap"
            className="hidden"
            onChange={(e) => {
              const file = e.target.files?.[0]
              if (file) readFile(file)
            }}
          />
        </label>

        <button
          onClick={() => arrBytes && onLoad(arrBytes)}
          disabled={busy || arrBytes === null}
          className="rounded-md bg-emerald-700 px-4 py-2 text-sm font-medium text-white transition hover:bg-emerald-600 disabled:opacity-40"
        >
          Reconstruct &amp; simulate
        </button>

        {fileName && (
          <span className="font-mono text-xs text-slate-400">
            {fileName} &middot; {(sizeBytes / 1024).toFixed(1)} kB
          </span>
        )}
      </div>
    </section>
  )
}

function SimFileLoader({
  onLoad,
  busy,
}: {
  onLoad: (fileText: string) => void
  busy: boolean
}) {
  const [text, setText] = useState('')
  const [fileName, setFileName] = useState<string | null>(null)

  async function readFile(file: File) {
    setText(await file.text())
    setFileName(file.name)
  }

  return (
    <section className="rounded-lg border border-slate-800 bg-slate-900/50 p-5">
      <div className="flex items-baseline justify-between">
        <h3 className="text-sm font-medium uppercase tracking-wider text-slate-400">
          Load a simulation file
        </h3>
        <span className="text-xs text-slate-500">JSON</span>
      </div>

      <p className="mt-1 text-xs text-slate-600">
        Buses, ECUs by name, their DIDs and DTCs, and the answers they give — in one file you
        can keep under version control. It is the only source that says how the ECUs are wired,
        so it is the one the topology diagram can draw real buses from.
      </p>

      <textarea
        value={text}
        onChange={(e) => {
          setText(e.target.value)
          setFileName(null)
        }}
        rows={8}
        spellCheck={false}
        placeholder={'{\n  "simfileVersion": 1,\n  "vehicle": "Demo vehicle",\n  "networks": [ … ],\n  "ecus": [ … ]\n}'}
        className="mt-3 w-full rounded-md border border-slate-700 bg-slate-950 px-3 py-2 font-mono text-xs text-slate-300 outline-none focus:border-slate-500"
      />

      <div className="mt-3 flex flex-wrap items-center gap-3">
        <button
          onClick={() => onLoad(text)}
          disabled={busy || text.trim().length === 0}
          className="rounded-md bg-emerald-700 px-4 py-2 text-sm font-medium text-white transition hover:bg-emerald-600 disabled:opacity-40"
        >
          Load &amp; simulate
        </button>

        <label className="cursor-pointer rounded-md border border-slate-700 bg-slate-800 px-3 py-2 text-sm text-slate-200 transition hover:border-slate-500">
          Choose file…
          <input
            type="file"
            accept=".json,.simfile,.txt"
            className="hidden"
            onChange={(e) => {
              const file = e.target.files?.[0]
              if (file) readFile(file)
            }}
          />
        </label>

        {fileName && <span className="font-mono text-xs text-slate-500">{fileName}</span>}
        <span className="text-xs text-slate-600">
          There is a worked example in <code>samples/demo-vehicle.simfile.json</code>.
        </span>
      </div>
    </section>
  )
}

function EmptyState() {
  return (
    <p className="rounded-lg border border-slate-800 bg-slate-900/40 px-4 py-10 text-center text-sm text-slate-500">
      No vehicle loaded. Reconstruct one from a CAN log or a DoIP capture, load a simulation
      file, or build one ECU at a time.
    </p>
  )
}

// ---------------------------------------------------------------------------------------
// The reconstructed vehicle
// ---------------------------------------------------------------------------------------

function EcuList({
  state,
  onReset,
  onToggleRunning,
  onChanged,
  onError,
  busy,
}: {
  state: SimulationState
  onReset: () => void
  onToggleRunning: () => void
  onChanged: (state: SimulationState) => void
  onError: (message: string | null) => void
  busy: boolean
}) {
  return (
    <aside className="h-fit space-y-3">
      <div className="flex items-baseline justify-between">
        <h3 className="text-sm font-medium uppercase tracking-wider text-slate-400">
          {state.vehicleName ?? 'Vehicle'}
        </h3>
        <span className="text-xs text-slate-500">
          {state.ecus.length} ECU{state.ecus.length === 1 ? '' : 's'}
        </span>
      </div>

      <PermissiveSwitch state={state} onChanged={onChanged} onError={onError} busy={busy} />

      <SaveVehicleButton state={state} onSaved={onChanged} onError={onError} busy={busy} />

      {state.ecus.map((ecu) => (
        <EcuCard
          key={ecu.requestCanIdHex}
          ecu={ecu}
          onChanged={onChanged}
          onError={onError}
          busy={busy}
        />
      ))}

      <div className="flex gap-2">
        <button
          onClick={onToggleRunning}
          disabled={busy}
          title={
            state.running
              ? 'Take the ECUs off the bus, keeping their state'
              : 'Put the ECUs back on the bus'
          }
          className={`flex-1 rounded-md px-3 py-2 text-sm font-medium text-white transition disabled:opacity-40 ${
            state.running ? 'bg-rose-800 hover:bg-rose-700' : 'bg-emerald-700 hover:bg-emerald-600'
          }`}
        >
          {state.running ? 'Stop simulation' : 'Start simulation'}
        </button>
        <button
          onClick={onReset}
          disabled={busy}
          title="Return every ECU to the default session with security locked; keeps the model, the timing and the overrides"
          className="rounded-md border border-slate-700 bg-slate-800 px-3 py-2 text-sm text-slate-200 transition hover:border-slate-500 disabled:opacity-40"
        >
          Reset state
        </button>
      </div>
    </aside>
  )
}

function EcuCard({
  ecu,
  onChanged,
  onError,
  busy,
}: {
  ecu: SimulationEcu
  onChanged: (state: SimulationState) => void
  onError: (message: string | null) => void
  busy: boolean
}) {
  const [renaming, setRenaming] = useState(false)
  const [name, setName] = useState(ecu.name)
  const [working, setWorking] = useState(false)

  async function run(action: () => Promise<SimulationState>) {
    setWorking(true)
    try {
      onChanged(await action())
      onError(null)
    } catch (e) {
      onError(DescribeError(e))
    } finally {
      setWorking(false)
    }
  }

  async function rename() {
    await run(() => api.simulationRenameEcu(ecu.handle, name))
    setRenaming(false)
  }

  return (
    <div className="rounded-lg border border-slate-800 bg-slate-900/50 p-4">
      <div className="flex items-center justify-between gap-2">
        {renaming ? (
          <form
            className="flex flex-1 gap-1"
            onSubmit={(e) => {
              e.preventDefault()
              if (name.trim()) rename()
            }}
          >
            <input
              autoFocus
              value={name}
              onChange={(e) => setName(e.target.value)}
              className="min-w-0 flex-1 rounded-md border border-slate-700 bg-slate-950 px-2 py-1 text-sm text-slate-200 outline-none focus:border-slate-500"
            />
            <button
              type="submit"
              disabled={working || name.trim().length === 0}
              className="rounded-md bg-sky-700 px-2 py-1 text-xs text-white disabled:opacity-40"
            >
              Save
            </button>
            <button
              type="button"
              onClick={() => {
                setName(ecu.name)
                setRenaming(false)
              }}
              className="px-1 text-xs text-slate-400"
            >
              Cancel
            </button>
          </form>
        ) : (
          <>
            <div className="flex items-center gap-2">
              <PowerSwitch
                isOn={ecu.isEnabled}
                disabled={busy || working}
                label={`Switch ${ecu.name} ${ecu.isEnabled ? 'off' : 'on'}`}
                onToggle={() =>
                  run(() => api.simulationSetEcuEnabled(ecu.handle, !ecu.isEnabled))
                }
              />
              <h4 className={`font-semibold ${ecu.isEnabled ? '' : 'text-slate-500'}`}>
                {ecu.name}
              </h4>
              {!ecu.isEnabled && <Badge tone="slate">off</Badge>}
            </div>
            <div className="flex items-center gap-2">
              <span className="font-mono text-xs text-slate-400">
                {ecu.requestCanIdHex} → {ecu.responseCanIdHex}
              </span>
              <button
                onClick={() => setRenaming(true)}
                disabled={busy || working}
                title="Rename this ECU"
                className="text-xs text-slate-500 transition hover:text-slate-300 disabled:opacity-40"
              >
                rename
              </button>
              <button
                onClick={() => run(() => api.simulationRemoveEcu(ecu.handle))}
                disabled={busy || working}
                title="Remove this ECU from the vehicle"
                className="text-xs text-slate-500 transition hover:text-rose-400 disabled:opacity-40"
              >
                remove
              </button>
            </div>
          </>
        )}
      </div>

      <dl className="mt-3 space-y-2 text-sm">
        <DetailRow label="Addressing">
          <span className="flex items-center justify-end gap-1.5">
            <span className="text-xs text-slate-400">{ecu.addressingMode}</span>
            <Badge tone={ConfidenceTone(ecu.addressConfidence)}>{ecu.addressConfidence}</Badge>
          </span>
        </DetailRow>
        {ecu.functionalCanIdHex && (
          <DetailRow label="Broadcast">
            <span className="font-mono text-xs text-slate-300">{ecu.functionalCanIdHex}</span>
          </DetailRow>
        )}
        <DetailRow label="Session">
          <Badge tone="sky">{ecu.sessionName}</Badge>
        </DetailRow>
        <DetailRow label="Security">
          {ecu.securityUnlocked ? (
            <Badge tone="emerald">Unlocked (L{ecu.securityLevel})</Badge>
          ) : (
            <Badge tone="slate">Locked</Badge>
          )}
        </DetailRow>
        <DetailRow label="Services">
          <span className="font-mono text-xs text-slate-300">
            {ecu.supportedServices.map(FormatByte).join(' ') || '—'}
          </span>
        </DetailRow>
        <DetailRow label="DIDs">
          <span className="font-mono text-xs text-slate-300">
            {ecu.dids.map(FormatDid).join(' ') || '—'}
          </span>
        </DetailRow>
        <DetailRow label="DTCs">
          <span className="text-slate-300">{ecu.dtcCount}</span>
        </DetailRow>
      </dl>
    </div>
  )
}

// ---------------------------------------------------------------------------------------
// Sending a request
// ---------------------------------------------------------------------------------------

/** One entry in the address picker: an ECU's own identifier, or a shared broadcast one. */
interface AddressOption {
  canIdHex: string
  label: string
}

function RequestPanel({
  options,
  canIdHex,
  onCanIdChange,
  hexInput,
  onHexInputChange,
  onSend,
  busy,
}: {
  options: AddressOption[]
  canIdHex: string
  onCanIdChange: (value: string) => void
  hexInput: string
  onHexInputChange: (value: string) => void
  onSend: (requestHex: string) => void
  busy: boolean
}) {
  return (
    <div className="space-y-4">
      <div>
        <h3 className="mb-2 text-sm font-medium uppercase tracking-wider text-slate-400">
          Address
        </h3>
        <select
          value={canIdHex}
          onChange={(e) => onCanIdChange(e.target.value)}
          className="w-full rounded-md border border-slate-700 bg-slate-950 px-3 py-2 font-mono text-sm text-slate-200 outline-none focus:border-slate-500"
        >
          {options.map((option) => (
            <option key={option.canIdHex} value={option.canIdHex}>
              {option.label}
            </option>
          ))}
        </select>
      </div>

      <div>
        <h3 className="mb-2 text-sm font-medium uppercase tracking-wider text-slate-400">
          Quick actions
        </h3>
        <div className="flex flex-wrap gap-2">
          {QUICK_ACTIONS.map((action) => (
            <button
              key={action.label}
              disabled={busy}
              onClick={() => onSend(action.hex)}
              className="rounded-md border border-slate-700 bg-slate-800 px-3 py-1.5 text-sm text-slate-200 transition hover:border-slate-500 hover:bg-slate-700 disabled:opacity-40"
            >
              {action.label}
            </button>
          ))}
        </div>
      </div>

      <div>
        <h3 className="mb-2 text-sm font-medium uppercase tracking-wider text-slate-400">
          Send raw request
        </h3>
        <form
          className="flex gap-2"
          onSubmit={(e) => {
            e.preventDefault()
            if (hexInput.trim()) onSend(hexInput)
          }}
        >
          <input
            value={hexInput}
            onChange={(e) => onHexInputChange(e.target.value)}
            placeholder="e.g. 22 F1 90"
            className="flex-1 rounded-md border border-slate-700 bg-slate-950 px-3 py-2 font-mono text-sm text-emerald-300 outline-none focus:border-slate-500"
          />
          <button
            type="submit"
            disabled={busy}
            className="rounded-md bg-emerald-700 px-4 py-2 text-sm font-medium text-white transition hover:bg-emerald-600 disabled:opacity-40"
          >
            Send
          </button>
        </form>
      </div>
    </div>
  )
}

// ---------------------------------------------------------------------------------------
// Response overrides
// ---------------------------------------------------------------------------------------

/**
 * Edit what one ECU answers to a particular request.
 *
 * The picker is service → sub-function, because `19 02` and `19 04` are different requests with
 * different parameter shapes; offering one row per service is what made this feel limited.
 * Both byte fields stay editable before the override is added, so anything the catalogue does
 * not cover can simply be typed.
 */
/**
 * Set what one ECU answers to one request.
 *
 * Deliberately not a list. A loaded simulation file can carry ninety-odd responses, and
 * rendering them all pushed the communication log so far down the page that the thing you
 * actually watch was unreachable. The pickers *are* the way in: choose a service and a
 * sub-function and you are editing that response, whether or not one was already set. Applying
 * folds the editor away again, because by then the interesting thing is the traffic.
 */
function OverridePanel({
  ecu,
  onError,
  busy,
}: {
  ecu: SimulationEcu | null
  onError: (message: string | null) => void
  busy: boolean
}) {
  const [overrides, setOverrides] = useState<ResponseOverride[] | null>(null)
  const [working, setWorking] = useState(false)
  const [isEditing, setEditing] = useState(false)
  const [serviceSid, setServiceSid] = useState(UDS_CATALOGUE[0].sid)
  const [variantIndex, setVariantIndex] = useState(0)
  const [requestHex, setRequestHex] = useState(UDS_CATALOGUE[0].variants[0].requestHex)
  const [responseHex, setResponseHex] = useState(UDS_CATALOGUE[0].variants[0].responseHex)
  const [lastApplied, setLastApplied] = useState<string | null>(null)
  const [matchTrailing, setMatchTrailing] = useState(
    DefaultMatchTrailing(UDS_CATALOGUE[0], UDS_CATALOGUE[0].variants[0]),
  )

  const requestCanIdHex = ecu?.requestCanIdHex

  useEffect(() => {
    // The panel is keyed by the selected identifier, so it remounts when the ECU changes and
    // cannot show one ECU's overrides against another. Nothing to reset here.
    if (!requestCanIdHex) {
      return
    }
    let cancelled = false
    api
      .ecuOverrides(requestCanIdHex)
      .then((loaded) => {
        if (!cancelled) setOverrides(loaded)
      })
      .catch((e) => {
        if (!cancelled) onError(DescribeError(e))
      })
    return () => {
      cancelled = true
    }
  }, [requestCanIdHex, onError])

  if (!ecu) {
    return null
  }

  const vecOverrides = overrides ?? []
  // The catalogue lists the well-known identifiers; this ECU's own come from the loaded
  // vehicle. Without them a simfile that defines 0x0111 offers no way to select it, and the
  // editor looks as though the engine only knows about VINs and part numbers.
  const vecServices = ServicesForEcu(ecu)
  const service = vecServices.find((entry) => entry.sid === serviceSid) ?? vecServices[0]
  const variant = service.variants[variantIndex] ?? service.variants[0]

  // Which existing response, if any, this request pattern is. Matching on the pattern is what
  // lets the pickers double as the way to find something already set: choose 0x19 then 0x04 and
  // you are looking at the response for `19 04`, not adding a second one beside it.
  const existingIndex = FindOverrideIndex(vecOverrides, requestHex)
  const existing = existingIndex >= 0 ? vecOverrides[existingIndex] : null

  /** Load whatever is already set for a request pattern, falling back to the catalogue. */
  function fillFor(strRequestHex: string, strCatalogueResponse: string) {
    const iFound = FindOverrideIndex(vecOverrides, strRequestHex)
    const found = iFound >= 0 ? vecOverrides[iFound] : null
    setRequestHex(strRequestHex)
    setResponseHex(found?.responseHex ?? strCatalogueResponse)
  }

  function selectService(sid: string) {
    // The per-ECU list, not the static catalogue: this ECU's own identifiers are folded into
    // 0x22 and 0x2E, and picking from the catalogue here would fill the fields with a
    // well-known identifier the operator did not choose.
    const next = vecServices.find((entry) => entry.sid === sid)
    if (!next) return
    setServiceSid(sid)
    setVariantIndex(0)
    fillFor(next.variants[0].requestHex, next.variants[0].responseHex)
    setMatchTrailing(DefaultMatchTrailing(next, next.variants[0]))
  }

  function selectVariant(index: number) {
    const next = service.variants[index]
    if (!next) return
    setVariantIndex(index)
    fillFor(next.requestHex, next.responseHex)
    setMatchTrailing(DefaultMatchTrailing(service, next))
  }

  async function save(vecNext: ResponseOverride[], strAppliedLabel: string | null) {
    if (!ecu) return
    setWorking(true)
    try {
      setOverrides(await api.setEcuOverrides(ecu.handle, vecNext))
      onError(null)
      if (strAppliedLabel) {
        // Applied: fold away and leave the log the room. The summary bar is the way back.
        setLastApplied(strAppliedLabel)
        setEditing(false)
      }
    } catch (e) {
      // The engine explains exactly which override it refused and why — and the editor stays
      // open, because a rejected response is precisely when you need it.
      onError(DescribeError(e))
    } finally {
      setWorking(false)
    }
  }

  function apply(action: 'substitute' | 'suppress') {
    const rule: ResponseOverride = {
      requestHex,
      matchTrailingBytes: matchTrailing,
      action,
      responseHex: action === 'substitute' ? responseHex : null,
      // Echo spans come from the catalogue entry, but only while its request template is
      // untouched: an edited template moves the bytes the span points at.
      echoSpans:
        action === 'substitute' && requestHex === variant.requestHex
          ? (variant.echoSpans ?? [])
          : [],
      enabled: true,
      respondEvenIfSuppressed: existing?.respondEvenIfSuppressed ?? false,
      note: `${service.name} — ${variant.label}`,
    }

    // Replacing rather than appending is what keeps the pickers honest: choosing a
    // sub-function that is already set must edit it, not quietly create a second rule that
    // shadows the first.
    const vecNext =
      existingIndex >= 0
        ? vecOverrides.map((rule_, i) => (i === existingIndex ? rule : rule_))
        : [...vecOverrides, rule]

    save(vecNext, `${requestHex} → ${action === 'suppress' ? 'silence' : responseHex}`)
  }

  function refuseWith(nrc: string) {
    const bySid = requestHex.trim().slice(0, 2).toUpperCase()
    setResponseHex(`7F ${bySid} ${nrc}`)
  }

  if (!isEditing) {
    return (
      <SummaryBar
        ecuName={ecu.name}
        count={vecOverrides.length}
        lastApplied={lastApplied}
        onEdit={() => setEditing(true)}
        disabled={busy || working}
      />
    )
  }

  return (
    <section className="rounded-lg border border-slate-700 bg-slate-900/50 p-4">
      <div className="flex items-baseline justify-between">
        <h3 className="text-sm font-medium uppercase tracking-wider text-slate-400">
          Responses — {ecu.name}
        </h3>
        <div className="flex items-center gap-2">
          <span className="text-xs text-slate-500">{vecOverrides.length} set</span>
          <button
            onClick={() => setEditing(false)}
            className="text-xs text-slate-400 transition hover:text-slate-200"
          >
            Done
          </button>
        </div>
      </div>

      <div className="mt-3 grid gap-2 sm:grid-cols-2">
        <label className="block">
          <span className="text-xs text-slate-400">Service</span>
          <select
            value={serviceSid}
            onChange={(e) => selectService(e.target.value)}
            className="mt-1 w-full rounded-md border border-slate-700 bg-slate-950 px-2 py-1.5 text-sm text-slate-200 outline-none focus:border-slate-500"
          >
            {vecServices.map((entry) => (
              <option key={entry.sid} value={entry.sid}>
                0x{entry.sid} {entry.name}
                {entry.implemented ? '' : ' — override only'}
              </option>
            ))}
          </select>
        </label>

        <label className="block">
          <span className="text-xs text-slate-400">Sub-function / variant</span>
          <select
            value={variantIndex}
            onChange={(e) => selectVariant(Number(e.target.value))}
            className="mt-1 w-full rounded-md border border-slate-700 bg-slate-950 px-2 py-1.5 text-sm text-slate-200 outline-none focus:border-slate-500"
          >
            {service.variants.map((entry, iIndex) => (
              <option key={entry.label} value={iIndex}>
                {entry.label}
              </option>
            ))}
          </select>
        </label>
      </div>

      <label className="mt-2 flex items-start gap-2 text-[11px] text-slate-400">
        <input
          type="checkbox"
          checked={matchTrailing}
          onChange={(e) => setMatchTrailing(e.target.checked)}
          className="mt-0.5"
        />
        <span>
          Match as a prefix, ignoring anything after it.
          {matchTrailing ? (
            <span className="text-slate-500">
              {' '}
              A request starting with these bytes matches however long it is.
            </span>
          ) : (
            <span className="text-amber-400/90">
              {' '}
              Off: the request must be exactly this length. A WriteDataByIdentifier carries the
              value it is writing, so an exact template never matches one.
            </span>
          )}
        </span>
      </label>

      {!service.implemented && (
        <p className="mt-2 rounded border border-amber-900/50 bg-amber-950/20 px-2 py-1.5 text-[11px] text-amber-400/90">
          The engine&rsquo;s UDS plugin does not implement 0x{service.sid}. Without an override
          here it answers <span className="font-mono">7F {service.sid} 11</span>.
        </p>
      )}

      <div className="mt-2 grid gap-2 sm:grid-cols-2">
        <label className="block">
          <span className="text-xs text-slate-400">Request (** = any byte)</span>
          <input
            value={requestHex}
            onChange={(e) => setRequestHex(e.target.value)}
            className="mt-1 w-full rounded-md border border-slate-700 bg-slate-950 px-2 py-1.5 font-mono text-xs text-slate-200 outline-none focus:border-slate-500"
          />
        </label>
        <label className="block">
          <span className="text-xs text-slate-400">Response</span>
          <input
            value={responseHex}
            onChange={(e) => setResponseHex(e.target.value)}
            className="mt-1 w-full rounded-md border border-slate-700 bg-slate-950 px-2 py-1.5 font-mono text-xs text-emerald-300 outline-none focus:border-slate-500"
          />
        </label>
      </div>

      <div className="mt-2 flex flex-wrap items-center gap-2">
        <button
          onClick={() => apply('substitute')}
          disabled={busy || working}
          className="rounded-md bg-emerald-700 px-3 py-1.5 text-xs font-medium text-white transition hover:bg-emerald-600 disabled:opacity-40"
        >
          {existing ? 'Update response' : 'Apply response'}
        </button>
        <button
          onClick={() => apply('suppress')}
          disabled={busy || working}
          title="Handle the request but transmit nothing"
          className="rounded-md border border-slate-700 bg-slate-800 px-3 py-1.5 text-xs text-slate-200 transition hover:border-slate-500 disabled:opacity-40"
        >
          Answer with silence
        </button>
        <select
          value=""
          onChange={(e) => e.target.value && refuseWith(e.target.value)}
          disabled={busy || working}
          className="rounded-md border border-slate-700 bg-slate-950 px-2 py-1.5 text-xs text-slate-300 outline-none focus:border-slate-500"
        >
          <option value="">Refuse with…</option>
          {NEGATIVE_RESPONSES.map((entry) => (
            <option key={entry.nrc} value={entry.nrc}>
              {entry.label}
            </option>
          ))}
        </select>
      </div>

      {existing && (
        <div className="mt-3">
          <p className="mb-1.5 text-[11px] text-sky-400/90">
            This request already has a response — applying replaces it rather than adding a
            second rule beside it.
          </p>
          <OverrideRow
            rule={existing}
            disabled={busy || working}
            onChange={(next) =>
              save(
                vecOverrides.map((rule_, i) => (i === existingIndex ? next : rule_)),
                null,
              )
            }
            onRemove={() =>
              save(
                vecOverrides.filter((_, i) => i !== existingIndex),
                null,
              )
            }
          />
        </div>
      )}
    </section>
  )
}

/**
 * The folded-away editor.
 *
 * Deliberately still visible rather than gone: hiding it completely would leave no way back
 * except somewhere a user has to be told about.
 */
function SummaryBar({
  ecuName,
  count,
  lastApplied,
  onEdit,
  disabled,
}: {
  ecuName: string
  count: number
  lastApplied: string | null
  onEdit: () => void
  disabled: boolean
}) {
  return (
    <section className="flex flex-wrap items-center gap-x-3 gap-y-1 rounded-lg border border-slate-800 bg-slate-900/50 px-4 py-2.5">
      <h3 className="text-sm font-medium uppercase tracking-wider text-slate-400">Responses</h3>
      <span className="text-xs text-slate-500">
        {count} set on {ecuName}
      </span>
      {lastApplied && (
        <span className="font-mono text-[11px] text-emerald-400/80">applied {lastApplied}</span>
      )}
      <button
        onClick={onEdit}
        disabled={disabled}
        className="ml-auto rounded-md border border-slate-700 bg-slate-800 px-3 py-1 text-xs text-slate-200 transition hover:border-slate-500 disabled:opacity-40"
      >
        Set a response
      </button>
    </section>
  )
}

/**
 * Find the response already set for a request pattern.
 *
 * Compared on normalised hex so `19 04` and `1904` are the same rule — otherwise the pickers
 * would appear to add a duplicate the engine would then have to arbitrate between.
 */
function FindOverrideIndex(vecOverrides: ResponseOverride[], requestHex: string): number {
  const strNeedle = NormaliseHex(requestHex)
  return vecOverrides.findIndex((rule) => NormaliseHex(rule.requestHex) === strNeedle)
}

function NormaliseHex(strHex: string): string {
  return strHex.replace(/\s+/g, '').toUpperCase()
}

function OverrideRow({
  rule,
  disabled,
  onChange,
  onRemove,
}: {
  rule: ResponseOverride
  disabled: boolean
  onChange: (rule: ResponseOverride) => void
  onRemove: () => void
}) {
  const [draft, setDraft] = useState(rule)
  const bIsDirty =
    draft.requestHex !== rule.requestHex ||
    draft.responseHex !== rule.responseHex ||
    draft.action !== rule.action

  return (
    <li className="rounded-md border border-slate-800 bg-slate-950/60 p-2.5">
      <div className="flex flex-wrap items-center gap-2">
        <input
          value={draft.requestHex}
          onChange={(e) => setDraft({ ...draft, requestHex: e.target.value })}
          className="w-36 rounded border border-slate-700 bg-slate-950 px-2 py-1 font-mono text-xs text-slate-300 outline-none focus:border-slate-500"
        />
        <span className="text-slate-600">→</span>
        {draft.action === 'suppress' ? (
          <span className="flex-1 font-mono text-xs text-slate-500">(silence)</span>
        ) : (
          <input
            value={draft.responseHex ?? ''}
            onChange={(e) => setDraft({ ...draft, responseHex: e.target.value })}
            className="min-w-0 flex-1 rounded border border-slate-700 bg-slate-950 px-2 py-1 font-mono text-xs text-emerald-300 outline-none focus:border-slate-500"
          />
        )}
        <button
          onClick={() =>
            setDraft({
              ...draft,
              action: draft.action === 'suppress' ? 'substitute' : 'suppress',
            })
          }
          disabled={disabled}
          title="Answer with bytes, or stay silent for this request"
          className="text-[11px] text-slate-500 hover:text-slate-300 disabled:opacity-40"
        >
          {draft.action === 'suppress' ? 'answer' : 'silence'}
        </button>
        <button
          onClick={() => onChange({ ...draft, enabled: !draft.enabled })}
          disabled={disabled}
          className="text-[11px] text-slate-500 hover:text-slate-300 disabled:opacity-40"
        >
          {rule.enabled ? 'disable' : 'enable'}
        </button>
        <button
          onClick={onRemove}
          disabled={disabled}
          className="text-[11px] text-slate-500 hover:text-rose-400 disabled:opacity-40"
        >
          remove
        </button>
      </div>

      <div className="mt-1.5 flex items-center gap-2">
        {!rule.enabled && <Badge tone="slate">disabled</Badge>}
        {rule.requestHex.includes('**') && <Badge tone="amber">wildcard</Badge>}
        {bIsDirty && (
          <button
            onClick={() => onChange(draft)}
            disabled={disabled}
            className="rounded bg-sky-700 px-2 py-0.5 text-[11px] text-white disabled:opacity-40"
          >
            Apply
          </button>
        )}
      </div>
    </li>
  )
}

// ---------------------------------------------------------------------------------------
// Timing controls
// ---------------------------------------------------------------------------------------
// SecurityAccess levels
// ---------------------------------------------------------------------------------------

const c_arrKeyPolicies: { value: SecurityLevel['keyPolicy']; label: string; hint: string }[] = [
  {
    value: 'acceptAny',
    label: 'Accept any key',
    hint: 'Unlock whatever the tester sends. The honest choice for a level taken from a capture — the seed was observed, the key never usefully was.',
  },
  {
    value: 'compare',
    label: 'Compare with the expected key',
    hint: 'Unlock only on an exact match, NRC 0x35 otherwise. For a level whose key is genuinely known.',
  },
  {
    value: 'refuse',
    label: 'Always refuse',
    hint: 'Never unlock; answer every key with the code below. Fault injection — the tester’s rejected-key path on demand.',
  },
]

const c_arrWildcardTokens = ['**', '??', '..', 'xx', 'XX']

/** Split a hex field the way the engine does: whitespace ignored, two characters per byte. */
function HexTokens(value: string): string[] {
  const clean = value.replace(/\s+/g, '')
  const tokens: string[] = []
  for (let i = 0; i < clean.length; i += 2) tokens.push(clean.slice(i, i + 2))
  return tokens
}

/**
 * What is wrong with a hex field, in the words the engine would use — checked here so the
 * operator sees it while typing rather than after a rejected round trip.
 */
function DescribeHexProblem(value: string, label: string, allowEmpty: boolean): string | null {
  if (value.trim() === '') {
    return allowEmpty ? null : `${label} is empty`
  }
  for (const token of HexTokens(value)) {
    if (c_arrWildcardTokens.includes(token)) {
      return `${label}: ${token} is a wildcard, and this field holds literal bytes. Wildcards match a request pattern, which is a response override's job.`
    }
    if (!/^[0-9a-fA-F]{2}$/.test(token)) {
      return `${label}: "${token}" is not a hex byte`
    }
  }
  return null
}

/** The one hex byte in a field, or null while it holds anything else. */
function SingleByteOf(value: string): number | null {
  const tokens = HexTokens(value)
  if (tokens.length !== 1 || !/^[0-9a-fA-F]{2}$/.test(tokens[0])) return null
  return parseInt(tokens[0], 16)
}

function Hex2(value: number): string {
  return value.toString(16).toUpperCase().padStart(2, '0')
}

/**
 * The requestSeed/sendKey pair a level would cover, or null when the field is not yet a usable
 * odd sub-function.
 *
 * Returns null for an even value rather than pairing it up. Someone who typed 02 meant "the
 * level for 27 02", and telling them it "handles 27 02 requestSeed and 27 03 sendKey" confirms
 * the misreading in the same breath the error below denies it.
 */
function CoveredPairOf(requestSeedHex: string): { requestSeed: string; sendKey: string } | null {
  const value = SingleByteOf(requestSeedHex)
  if (value === null || value % 2 === 0) return null
  return { requestSeed: Hex2(value), sendKey: Hex2(value + 1) }
}

/** For an even sub-function, the odd one that names the same level. */
function OddSubFunctionFor(requestSeedHex: string): string | null {
  const value = SingleByteOf(requestSeedHex)
  if (value === null || value % 2 !== 0 || value === 0) return null
  return Hex2(value - 1)
}

/** Everything wrong with one level, or null when it would be accepted. */
function DescribeLevelProblem(level: SecurityLevel): string | null {
  const seedTokens = HexTokens(level.requestSeedHex)
  if (seedTokens.length !== 1 || !/^[0-9a-fA-F]{2}$/.test(seedTokens[0])) {
    return 'requestSeed must be exactly one hex byte, e.g. 01'
  }

  const value = parseInt(seedTokens[0], 16)
  if (value % 2 === 0) {
    return `27 ${Hex2(value)} is the sendKey half of a pair, and a level is named by the requestSeed half — the odd value below it. Naming this level ${Hex2((value - 1 + 256) % 256)} is what makes it answer 27 ${Hex2(value)}.`
  }

  const seedProblem = DescribeHexProblem(level.seedHex, 'Seed', true)
  if (seedProblem) return seedProblem

  if (level.keyPolicy === 'compare') {
    const keyProblem = DescribeHexProblem(level.expectedKeyHex, 'Expected key', false)
    if (keyProblem) return keyProblem
  }

  if (level.keyPolicy === 'refuse') {
    const nrc = level.refusalNrcHex ?? ''
    const nrcProblem = DescribeHexProblem(nrc, 'Refusal NRC', false)
    if (nrcProblem) return nrcProblem
    if (parseInt(HexTokens(nrc)[0], 16) === 0) {
      return 'Refusal NRC 00 is the positive-response code, not a negative one'
    }
  }

  return null
}

/**
 * The request pattern of an override that already answers this level's requestSeed, if one
 * exists.
 *
 * Worth surfacing because the two features overlap invisibly: the protocol runs first and sets
 * the ECU's state, then an override replaces the bytes. So a level plus a matching override
 * gives a working unlock whose seed field is dead weight — which looks exactly like the level
 * not having been applied.
 */
function ShadowingOverrideOf(
  overrides: ResponseOverride[],
  level: SecurityLevel,
): string | null {
  const tokens = HexTokens(level.requestSeedHex)
  if (tokens.length !== 1) return null
  const wanted = `27 ${tokens[0].toUpperCase()}`

  const match = overrides.find(
    (rule) => rule.enabled && rule.requestHex.trim().toUpperCase().startsWith(wanted),
  )
  return match ? match.requestHex : null
}

/**
 * The response-editor catalogue with one ECU's configured data identifiers folded in.
 *
 * `UDS_CATALOGUE` carries the identifiers every vehicle shares — VIN, part numbers, the
 * standard 0xFxxx range. An ECU's *own* identifiers come from whatever populated the model: a
 * simulation file, a reconstruction, the builder. Those are the ones an operator actually wants
 * to override, and offering only the well-known ones makes the editor look as if the engine
 * knows nothing else.
 *
 * The ECU's own are listed first, because they are the reason someone opened this dropdown.
 * Duplicates are dropped so a DID that is both well-known and configured appears once.
 */
function ServicesForEcu(ecu: SimulationEcu | null): CatalogueService[] {
  if (!ecu || ecu.dids.length === 0) return UDS_CATALOGUE

  return UDS_CATALOGUE.map((service) => {
    if (service.sid !== '22' && service.sid !== '2E') return service

    const bIsRead = service.sid === '22'
    const vecOwn: CatalogueVariant[] = ecu.dids.map((u16Did) => {
      const strDid = u16Did.toString(16).toUpperCase().padStart(4, '0')
      const strRequestHex = `${bIsRead ? '22' : '2E'} ${strDid.slice(0, 2)} ${strDid.slice(2)}`
      const strResponseHex = `${bIsRead ? '62' : '6E'} ${strDid.slice(0, 2)} ${strDid.slice(2)}${bIsRead ? ' 00' : ''}`
      return {
        label: `${FormatDid(u16Did)} — on this ECU`,
        requestHex: strRequestHex,
        responseHex: strResponseHex,
        // A write carries the value after the identifier, and its length is whatever is being
        // written; a read of one identifier is exactly three bytes.
        matchTrailingBytes: !bIsRead,
      }
    })

    const setOwn = new Set(vecOwn.map((entry) => entry.requestHex))
    const vecRest = service.variants.filter((entry) => !setOwn.has(entry.requestHex))
    return { ...service, variants: [...vecOwn, ...vecRest] }
  })
}

/**
 * Whether a template should match as a prefix by default.
 *
 * The variant decides if it says so; otherwise the service does. Services in
 * `VARIABLE_TAIL_SERVICES` carry a tail no template can state — the value being written, the
 * key being sent, the block being transferred — and an exact-length match there can never fire.
 */
function DefaultMatchTrailing(service: CatalogueService, variant: CatalogueVariant): boolean {
  return variant.matchTrailingBytes ?? VARIABLE_TAIL_SERVICES.includes(service.sid)
}

function EmptySecurityLevel(): SecurityLevel {
  return {
    requestSeedHex: '01',
    seedHex: '',
    expectedKeyHex: '',
    keyPolicy: 'acceptAny',
    refusalNrcHex: null,
  }
}

/**
 * Edit an ECU's SecurityAccess levels.
 *
 * The whole list is replaced on save, matching the endpoint — what is on screen is what the
 * ECU ends up with.
 */
function SecurityPanel({
  ecu,
  onSaved,
  onError,
  busy,
}: {
  ecu: SimulationEcu | null
  onSaved: () => Promise<void> | void
  onError: (message: string | null) => void
  busy: boolean
}) {
  const [levels, setLevels] = useState<SecurityLevel[] | null>(null)
  const [overrides, setOverrides] = useState<ResponseOverride[]>([])
  const [draft, setDraft] = useState<SecurityLevel[] | null>(null)
  const [saving, setSaving] = useState(false)
  const [note, setNote] = useState<string | null>(null)

  const handle = ecu?.handle ?? null

  useEffect(() => {
    let cancelled = false
    if (!handle) return
    void (async () => {
      try {
        const [loaded, loadedOverrides] = await Promise.all([
          api.ecuSecurityLevels(handle),
          api.ecuOverrides(handle),
        ])
        if (!cancelled) {
          setLevels(loaded)
          setOverrides(loadedOverrides)
        }
      } catch (e) {
        if (!cancelled) onError(DescribeError(e))
      }
    })()
    return () => {
      cancelled = true
    }
    // onError is stable enough for this panel's lifetime; re-running on it would refetch on
    // every parent render.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [handle])

  if (!ecu) return null

  const shown = draft ?? levels
  if (shown === null) return null

  function update(index: number, patch: Partial<SecurityLevel>) {
    setNote(null)
    setDraft(shown!.map((level, i) => (i === index ? { ...level, ...patch } : level)))
  }

  async function save() {
    if (!handle) return
    setSaving(true)
    try {
      const saved = await api.setEcuSecurityLevels(handle, shown!)
      setLevels(saved)
      setDraft(null)
      onError(null)
      setNote('Saved. The change applies to the next SecurityAccess request.')
      await onSaved()
    } catch (e) {
      onError(DescribeError(e))
    } finally {
      setSaving(false)
    }
  }

  const bIsDirty = draft !== null
  const arrProblems = shown.map(DescribeLevelProblem)
  const firstProblem = arrProblems.find((problem) => problem !== null) ?? null

  return (
    <section className="rounded-lg border border-slate-800 bg-slate-900/50 p-4">
      <div className="flex items-baseline justify-between">
        <h3 className="text-sm font-medium uppercase tracking-wider text-slate-400">
          Security — {ecu.name}
        </h3>
        <span className="text-xs text-slate-500">ISO 14229-1 · 0x27</span>
      </div>

      <p className="mt-1 text-xs leading-relaxed text-slate-500">
        A simulator cannot compute a real key — the algorithm lives in the manufacturer&rsquo;s
        tooling, and a capture yields at best a seed. Say what each level should do instead. A
        key arriving with no preceding requestSeed is always refused with NRC 0x24, whichever
        policy is set.
      </p>

      {shown.length === 0 && (
        <p className="mt-3 rounded-md border border-slate-800 bg-slate-950/60 px-3 py-2 text-xs text-slate-400">
          No security levels. This ECU answers <span className="font-mono">27 02</span> with NRC
          0x12 (subFunctionNotSupported), because there is no level to send a key to.
        </p>
      )}

      <div className="mt-3 space-y-3">
        {shown.map((level, index) => (
          <div key={index} className="rounded-md border border-slate-800 bg-slate-950/40 p-3">
            <div className="grid gap-3 sm:grid-cols-2">
              <div>
                <TextField
                  label="requestSeed sub-function"
                  placeholder="01"
                  value={level.requestSeedHex}
                  onChange={(v) => update(index, { requestSeedHex: v })}
                  mono
                />
                <p className="mt-1 text-xs text-slate-500">
                  {CoveredPairOf(level.requestSeedHex) === null ? (
                    'One hex byte, odd — e.g. 01'
                  ) : (
                    <>
                      handles{' '}
                      <span className="font-mono text-slate-400">
                        27 {CoveredPairOf(level.requestSeedHex)!.requestSeed}
                      </span>{' '}
                      requestSeed and{' '}
                      <span className="font-mono text-slate-400">
                        27 {CoveredPairOf(level.requestSeedHex)!.sendKey}
                      </span>{' '}
                      sendKey
                    </>
                  )}
                </p>
              </div>
              <label className="block">
                <span className="text-xs text-slate-400">On sendKey</span>
                <select
                  value={level.keyPolicy}
                  onChange={(e) =>
                    update(index, { keyPolicy: e.target.value as SecurityLevel['keyPolicy'] })
                  }
                  className="mt-1 w-full rounded-md border border-slate-700 bg-slate-950 px-3 py-2 text-sm text-slate-200 outline-none focus:border-slate-500"
                >
                  {c_arrKeyPolicies.map((policy) => (
                    <option key={policy.value} value={policy.value}>
                      {policy.label}
                    </option>
                  ))}
                </select>
              </label>
            </div>

            <p className="mt-2 text-xs text-slate-500">
              {c_arrKeyPolicies.find((p) => p.value === level.keyPolicy)?.hint}
            </p>

            <div className="mt-3">
              <TextField
                label="Seed returned on requestSeed"
                placeholder="11 22 33 44 (literal bytes — no ** wildcards)"
                value={level.seedHex}
                onChange={(v) => update(index, { seedHex: v })}
                mono
              />
              {ShadowingOverrideOf(overrides, level) && (
                <p className="mt-1 text-xs text-amber-400">
                  A response override for{' '}
                  <span className="font-mono">{ShadowingOverrideOf(overrides, level)}</span>{' '}
                  already answers requestSeed on this ECU, and an override replaces the response
                  bytes. This seed will not reach the wire — leave it blank unless you delete
                  that override. The level is still what makes sendKey work.
                </p>
              )}
            </div>

            {level.keyPolicy === 'compare' && (
              <div className="mt-3">
                <TextField
                  label="Expected key"
                  placeholder="AA BB CC DD"
                  value={level.expectedKeyHex}
                  onChange={(v) => update(index, { expectedKeyHex: v })}
                  mono
                />
              </div>
            )}

            {level.keyPolicy === 'refuse' && (
              <div className="mt-3">
                <TextField
                  label="Refuse with NRC"
                  placeholder="35"
                  value={level.refusalNrcHex ?? ''}
                  onChange={(v) => update(index, { refusalNrcHex: v })}
                  mono
                />
              </div>
            )}

            {arrProblems[index] && (
              <div className="mt-3 rounded-md border border-rose-900/60 bg-rose-950/40 px-3 py-2 text-xs text-rose-300">
                <p>{arrProblems[index]}</p>
                {OddSubFunctionFor(level.requestSeedHex) && (
                  <button
                    onClick={() =>
                      update(index, { requestSeedHex: OddSubFunctionFor(level.requestSeedHex)! })
                    }
                    className="mt-2 rounded border border-rose-700 px-2 py-1 font-mono text-xs text-rose-200 transition hover:border-rose-500"
                  >
                    Use {OddSubFunctionFor(level.requestSeedHex)} instead
                  </button>
                )}
              </div>
            )}

            <button
              onClick={() => {
                setNote(null)
                setDraft(shown.filter((_, i) => i !== index))
              }}
              className="mt-3 text-xs text-rose-400 transition hover:text-rose-300"
            >
              Remove this level
            </button>
          </div>
        ))}
      </div>

      <div className="mt-4 flex items-center gap-3">
        <button
          onClick={() => {
            setNote(null)
            setDraft([...shown, EmptySecurityLevel()])
          }}
          className="rounded-md border border-slate-700 px-3 py-2 text-sm text-slate-300 transition hover:border-slate-500"
        >
          Add a level
        </button>
        <button
          onClick={save}
          disabled={busy || saving || !bIsDirty || firstProblem !== null}
          className="rounded-md bg-sky-700 px-4 py-2 text-sm font-medium text-white transition hover:bg-sky-600 disabled:opacity-40"
        >
          Apply security
        </button>
        {bIsDirty && firstProblem === null && (
          <span className="text-xs text-amber-400">unsaved</span>
        )}
        {note && <span className="text-xs text-slate-400">{note}</span>}
      </div>
    </section>
  )
}

// ---------------------------------------------------------------------------------------

/**
 * Edit one ECU's UDS server timing and make it real: a delay past P2Server_max makes the ECU
 * send NRC 0x78 ResponsePending before it answers, exactly as ISO 14229-1 requires.
 *
 * The form holds a draft so a half-typed number never reaches the engine, and the engine —
 * not the browser — validates: values it refuses come back with the reason.
 */
function TimingPanel({
  ecu,
  onSaved,
  onError,
  busy,
}: {
  ecu: SimulationEcu | null
  onSaved: () => Promise<void>
  onError: (message: string | null) => void
  busy: boolean
}) {
  const [draft, setDraft] = useState<EcuTiming | null>(null)
  const [saving, setSaving] = useState(false)
  const [note, setNote] = useState<string | null>(null)

  // A broadcast identifier addresses several ECUs, so there is no single timing to edit.
  if (!ecu) {
    return (
      <p className="rounded-lg border border-slate-800 bg-slate-900/40 px-4 py-3 text-xs text-slate-500">
        Timing is set per ECU — pick one ECU's own identifier rather than a broadcast to edit
        it.
      </p>
    )
  }

  const timing = draft ?? ecu.timing

  function update(patch: Partial<EcuTiming>) {
    setDraft({ ...timing, ...patch })
  }

  async function save() {
    if (!ecu) return
    setSaving(true)
    try {
      const result = await api.setEcuTiming(ecu.handle, timing)
      setDraft(null)
      onError(null)
      setNote(
        result.advertisedAtNextSessionControl
          ? 'Saved. The tester sees the new P2/P2* at its next DiagnosticSessionControl (0x10) — ISO 14229-1 carries them nowhere else.'
          : 'Saved.',
      )
      await onSaved()
    } catch (e) {
      onError(DescribeError(e))
    } finally {
      setSaving(false)
    }
  }

  const bIsDirty = draft !== null

  return (
    <section className="rounded-lg border border-slate-800 bg-slate-900/50 p-4">
      <div className="flex items-baseline justify-between">
        <h3 className="text-sm font-medium uppercase tracking-wider text-slate-400">
          Timing — {ecu.name}
        </h3>
        <span className="text-xs text-slate-500">ISO 14229-2</span>
      </div>

      <div className="mt-3 grid gap-3 sm:grid-cols-3">
        <NumberField
          label="P2 (ms)"
          hint="Deadline to start answering"
          value={timing.p2ServerMaxMs}
          onChange={(v) => update({ p2ServerMaxMs: v })}
        />
        <NumberField
          label="P2* (ms)"
          hint="Deadline after a 0x78"
          step={10}
          value={timing.p2StarServerMaxMs}
          onChange={(v) => update({ p2StarServerMaxMs: v })}
        />
        <NumberField
          label="Delay (ms)"
          hint="Injected think-time"
          value={timing.responseDelayMs}
          onChange={(v) => update({ responseDelayMs: v })}
        />
      </div>

      <div className="mt-3 space-y-2">
        <CheckboxField
          label="Force ResponsePending"
          hint="Send NRC 0x78 even when the delay would not require it"
          checked={timing.forceResponsePending}
          onChange={(v) => update({ forceResponsePending: v })}
        />
        {timing.forceResponsePending && (
          <div className="pl-6">
            <NumberField
              label="Repetitions"
              hint="How many 0x78 messages"
              value={timing.forcedResponsePendingCount}
              onChange={(v) => update({ forcedResponsePendingCount: v })}
            />
          </div>
        )}
        <CheckboxField
          label="Drop the final response"
          hint="A hung server: pendings go out, the answer never does"
          checked={timing.dropFinalResponse}
          onChange={(v) => update({ dropFinalResponse: v })}
        />
      </div>

      <div className="mt-4 border-t border-slate-800 pt-3">
        <div className="flex items-baseline justify-between">
          <h4 className="text-xs font-medium uppercase tracking-wider text-slate-400">
            Flow control — incoming requests
          </h4>
          <span className="text-xs text-slate-500">ISO 15765-2</span>
        </div>
        <p className="mt-1 text-xs text-slate-500">
          What this ECU puts in the FlowControl frame when a tester sends it a multi-frame
          request. Leave both at 0 on a fast link. Raise them if long requests arrive
          incomplete over a serial CAN adapter: at 115200 baud a dongle forwards roughly 427
          frames per second while a 500 kbit/s bus delivers about 3600, so a tester told to
          send 55 frames back to back will overrun it.
        </p>
        <div className="mt-3 grid gap-3 sm:grid-cols-2">
          <NumberField
            label="BlockSize"
            hint="Frames per FlowControl; 0 = send them all"
            value={timing.isoTpBlockSize}
            onChange={(v) => update({ isoTpBlockSize: v })}
          />
          <NumberField
            label="STmin (raw byte)"
            hint="0-127 = ms; 241-249 = 100-900 us"
            value={timing.isoTpSeparationTimeMin}
            onChange={(v) => update({ isoTpSeparationTimeMin: v })}
          />
        </div>
      </div>

      <div className="mt-4 flex items-center gap-3">
        <button
          onClick={save}
          disabled={busy || saving || !bIsDirty}
          className="rounded-md bg-sky-700 px-4 py-2 text-sm font-medium text-white transition hover:bg-sky-600 disabled:opacity-40"
        >
          Apply timing
        </button>
        {bIsDirty && (
          <button
            onClick={() => setDraft(null)}
            disabled={saving}
            className="text-xs text-slate-400 underline-offset-2 hover:underline"
          >
            Discard changes
          </button>
        )}
      </div>

      {note && !bIsDirty && <p className="mt-2 text-xs text-slate-500">{note}</p>}
    </section>
  )
}

function NumberField({
  label,
  hint,
  value,
  onChange,
  step,
}: {
  label: string
  hint: string
  value: number
  onChange: (value: number) => void
  step?: number
}) {
  return (
    <label className="block">
      <span className="text-xs text-slate-400">{label}</span>
      <input
        type="number"
        min={0}
        step={step ?? 1}
        value={value}
        onChange={(e) => onChange(Number(e.target.value))}
        className="mt-1 w-full rounded-md border border-slate-700 bg-slate-950 px-2 py-1.5 font-mono text-sm text-slate-200 outline-none focus:border-slate-500"
      />
      <span className="mt-0.5 block text-[11px] text-slate-600">{hint}</span>
    </label>
  )
}

function CheckboxField({
  label,
  hint,
  checked,
  onChange,
}: {
  label: string
  hint: string
  checked: boolean
  onChange: (checked: boolean) => void
}) {
  return (
    <label className="flex cursor-pointer items-start gap-2">
      <input
        type="checkbox"
        checked={checked}
        onChange={(e) => onChange(e.target.checked)}
        className="mt-0.5 h-4 w-4 rounded border-slate-600 bg-slate-950"
      />
      <span>
        <span className="text-sm text-slate-200">{label}</span>
        <span className="block text-[11px] text-slate-600">{hint}</span>
      </span>
    </label>
  )
}

// ---------------------------------------------------------------------------------------
// The exchange log
// ---------------------------------------------------------------------------------------

/**
 * What the last request you sent actually did, in detail.
 *
 * The communication log below shows every exchange, but only a request sent from here carries
 * the per-message timings, the ResponsePending count and the ISO 14229-2 conformance warnings —
 * the engine computes those while executing the response plan. Keeping the most recent one is
 * how that detail survives the log becoming a live stream.
 */
function LastExchangeDetail({ result }: { result: SimulationRequestResult }) {
  return (
    <div>
      <h3 className="mb-2 text-sm font-medium uppercase tracking-wider text-slate-400">
        Last request you sent
      </h3>
      <ul className="space-y-2">
        <ExchangeEntryView result={result} />
      </ul>
    </div>
  )
}

function ExchangeEntryView({ result }: { result: SimulationRequestResult }) {
  return (
    <li className="rounded-md border border-slate-800 bg-slate-950/60 p-3 font-mono text-xs">
      <div className="flex items-center gap-2 text-slate-400">
        <span className="text-slate-600">→</span>
        <span className="text-slate-500">{result.canIdHex}</span>
        <span>{result.requestHex}</span>
        {result.addressing === 'functional' && <Badge tone="amber">broadcast</Badge>}
      </div>

      {result.addressing === 'stopped' ? (
        <div className="mt-1 text-amber-500/80">
          <span className="text-slate-600">←</span> the simulation is stopped — the ECUs are off
          the bus
        </div>
      ) : result.addressing === 'silenced' ? (
        <div className="mt-1 text-amber-500/80">
          <span className="text-slate-600">←</span> silence — {result.silencedReason}
        </div>
      ) : !result.routed ? (
        <div className="mt-1 text-slate-500">
          <span className="text-slate-600">←</span> no ECU listens on {result.canIdHex} — silence
        </div>
      ) : result.responses.length === 0 ? (
        <div className="mt-1 text-slate-500">
          <span className="text-slate-600">←</span> every ECU stayed silent (negative responses
          are suppressed on a broadcast)
        </div>
      ) : (
        result.responses.map((response) => (
          <ResponseView key={response.responseCanIdHex} response={response} />
        ))
      )}
    </li>
  )
}

/**
 * One ECU's answer. An answer can be several messages over time — any NRC 0x78
 * ResponsePending, then the final response — so each is shown with the millisecond at which
 * it actually went out.
 */
function ResponseView({ response }: { response: SimulationResponse }) {
  const bHasFrames = response.frames.length > 0

  return (
    <div className="mt-1.5">
      {bHasFrames ? (
        response.frames.map((frame, iIndex) => (
          <div key={`${frame.kind}-${iIndex}`} className="flex items-start gap-2">
            <span className="text-slate-600">←</span>
            <span className="w-14 shrink-0 text-right text-slate-600">+{frame.actualMs}ms</span>
            <span className="text-slate-500">{response.responseCanIdHex}</span>
            <span className={FrameTone(frame.kind, frame.hex)}>{frame.hex}</span>
            {frame.kind === 'responsePending' && <Badge tone="amber">pending</Badge>}
          </div>
        ))
      ) : (
        <div className="flex items-start gap-2 text-slate-500">
          <span className="text-slate-600">←</span>
          <span className="text-slate-500">{response.responseCanIdHex}</span>
          <span>
            {response.finalResponseDropped
              ? 'final response withheld — the tester will time out'
              : `response suppressed; now in ${response.sessionName}`}
          </span>
        </div>
      )}

      {response.finalResponseDropped && bHasFrames && (
        <div className="mt-1 pl-[4.5rem] text-rose-400">
          final response withheld — the tester will time out after P2*
        </div>
      )}

      {!response.isoConformant &&
        response.conformanceWarnings.map((warning) => (
          <div key={warning} className="mt-1 pl-[4.5rem] text-amber-500/80">
            ⚠ {warning}
          </div>
        ))}
    </div>
  )
}

/** Colour a frame: a pending is provisional, a negative response is a refusal. */
function FrameTone(kind: string, hex: string): string {
  if (kind === 'responsePending') {
    return 'text-amber-400/80'
  }
  return IsNegative(hex) ? 'text-amber-400' : 'text-emerald-400'
}

// ---------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------

/**
 * Every identifier a tester can address: each ECU's own request identifier, plus each distinct
 * broadcast identifier once, with the ECUs that listen on it named.
 */
function AddressOptions(state: SimulationState | null): AddressOption[] {
  if (!state?.loaded) {
    return []
  }

  // Keyed by handle, so an ECU reachable only over DoIP can be addressed here too — otherwise
  // a vehicle imported from an Ethernet capture would list ECUs nothing could query.
  const vecOptions: AddressOption[] = state.ecus.map((ecu) => ({
    canIdHex: ecu.handle,
    label: `${ecu.requestCanIdHex ?? `logical ${ecu.logicalAddressHex}`} — ${ecu.name}`,
  }))

  const mapListeners = new Map<string, string[]>()
  for (const ecu of state.ecus) {
    if (!ecu.functionalCanIdHex) {
      continue
    }
    const vecNames = mapListeners.get(ecu.functionalCanIdHex) ?? []
    vecNames.push(ecu.name)
    mapListeners.set(ecu.functionalCanIdHex, vecNames)
  }

  for (const [functionalCanIdHex, vecNames] of mapListeners) {
    vecOptions.push({
      canIdHex: functionalCanIdHex,
      label: `${functionalCanIdHex} — broadcast (${vecNames.join(', ')})`,
    })
  }

  return vecOptions
}

/**
 * The ECU addressed by an identifier, or `null` when the identifier is a broadcast — those
 * reach several ECUs, so there is no single one whose timing could be edited.
 */
function FindEcuByRequestCanId(
  state: SimulationState | null,
  canIdHex: string,
): SimulationEcu | null {
  if (!state?.loaded) {
    return null
  }
  return state.ecus.find((ecu) => ecu.requestCanIdHex === canIdHex) ?? null
}

/** Colour a confidence state: an observed fact is stronger than a derived one. */
function ConfidenceTone(confidence: string): BadgeTone {
  switch (confidence) {
    case 'Confirmed':
      return 'emerald'
    case 'Observed':
      return 'sky'
    case 'Inferred':
      return 'amber'
    case 'Conflict':
      return 'rose'
    default:
      return 'slate'
  }
}

/** A UDS negative response starts with 0x7F. */
function IsNegative(responseHex: string): boolean {
  return responseHex.trim().toUpperCase().startsWith('7F')
}

function FormatByte(byValue: number): string {
  return '0x' + byValue.toString(16).toUpperCase().padStart(2, '0')
}

function FormatDid(u16Did: number): string {
  return '0x' + u16Did.toString(16).toUpperCase().padStart(4, '0')
}

/** Present an unknown thrown value as a message worth showing. */
function DescribeError(e: unknown): string {
  return e instanceof Error ? e.message : String(e)
}
