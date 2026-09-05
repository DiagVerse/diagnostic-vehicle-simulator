import { useEffect, useRef, useState } from 'react'
import { Badge, DetailRow, type BadgeTone } from '../components/primitives'
import {
  api,
  type EcuTiming,
  type NewEcu,
  type ResponseOverride,
  type SimulationEcu,
  type SimulationRequestResult,
  type SimulationResponse,
  type SimulationState,
} from '../shared/api'

/** A quick action maps a human label to a fixed UDS request (hex). */
const QUICK_ACTIONS: { label: string; hex: string }[] = [
  { label: 'Read VIN', hex: '22 F1 90' },
  { label: 'Read DTCs', hex: '19 02 FF' },
  { label: 'Enter Extended', hex: '10 03' },
  { label: 'Enter Default', hex: '10 01' },
  { label: 'Tester Present', hex: '3E 00' },
]

/** One sent request and everything it produced, newest first in the exchange log. */
interface ExchangeEntry {
  id: number
  result: SimulationRequestResult
}

/** How many exchanges to keep on screen before dropping the oldest. */
const MAX_LOG_ENTRIES = 50

/**
 * Load a CAN log into the engine and drive the reconstructed vehicle: pick the CAN identifier
 * to address, send a UDS request, watch each ECU answer and change state.
 */
export function Simulate() {
  const [state, setState] = useState<SimulationState | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [log, setLog] = useState<ExchangeEntry[]>([])
  const [canIdHex, setCanIdHex] = useState('')
  const [hexInput, setHexInput] = useState('22 F1 90')
  const [busy, setBusy] = useState(false)
  const [source, setSource] = useState<VehicleSource>('log')
  const nextEntryId = useRef(1)

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
      setLog([])
      setError(null)
    } catch (e) {
      // A rejected log leaves the previously loaded vehicle running, so the view stays usable.
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
      setLog((prev) => [{ id: nextEntryId.current++, result }, ...prev].slice(0, MAX_LOG_ENTRIES))
      setError(null)
      await refreshState()
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
      setLog([])
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

      {state && !state.protocolLoaded && (
        <div className="rounded-lg border border-amber-900/60 bg-amber-950/30 px-4 py-3 text-sm text-amber-300">
          UDS protocol plugin not loaded — copy <code>libuds_plugin.*</code> into{' '}
          <code>plugins.d/</code> and restart the engine.
        </div>
      )}

      <SourcePicker source={source} onChange={setSource} />

      {source === 'log' ? (
        <LogLoader onLoad={load} busy={busy} />
      ) : (
        <VehicleBuilder onChanged={setState} onError={setError} busy={busy} />
      )}

      {!state?.loaded ? (
        <EmptyState />
      ) : (
        <div className="grid gap-6 lg:grid-cols-[360px_1fr]">
          <EcuList
            state={state}
            onReset={reset}
            onChanged={setState}
            onError={setError}
            busy={busy}
          />

          <section className="space-y-4">
            <RequestPanel
              options={vecAddressOptions}
              canIdHex={strSelectedCanId}
              onCanIdChange={setCanIdHex}
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
            <TimingPanel
              key={strSelectedCanId}
              ecu={FindEcuByRequestCanId(state, strSelectedCanId)}
              onSaved={refreshState}
              onError={setError}
              busy={busy}
            />
            <ExchangeLog log={log} />
          </section>
        </div>
      )}
    </div>
  )
}

// ---------------------------------------------------------------------------------------
// Where the vehicle comes from
// ---------------------------------------------------------------------------------------

/** A vehicle is either reconstructed from a capture or stated by hand. */
type VehicleSource = 'log' | 'build'

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
  const [draft, setDraft] = useState<NewEcu>({
    name: '',
    requestCanIdHex: '',
    responseCanIdHex: '',
  })
  const [working, setWorking] = useState(false)

  async function run(action: () => Promise<SimulationState>) {
    setWorking(true)
    try {
      onChanged(await action())
      onError(null)
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
      setDraft({ name: '', requestCanIdHex: '', responseCanIdHex: '' })
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

function EmptyState() {
  return (
    <p className="rounded-lg border border-slate-800 bg-slate-900/40 px-4 py-10 text-center text-sm text-slate-500">
      No vehicle loaded. Paste or choose a CAN log above — the engine reconstructs the ECUs it
      finds and starts them.
    </p>
  )
}

// ---------------------------------------------------------------------------------------
// The reconstructed vehicle
// ---------------------------------------------------------------------------------------

function EcuList({
  state,
  onReset,
  onChanged,
  onError,
  busy,
}: {
  state: SimulationState
  onReset: () => void
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

      {state.ecus.map((ecu) => (
        <EcuCard
          key={ecu.requestCanIdHex}
          ecu={ecu}
          onChanged={onChanged}
          onError={onError}
          busy={busy}
        />
      ))}

      <button
        onClick={onReset}
        disabled={busy}
        className="w-full rounded-md border border-slate-700 bg-slate-800 px-3 py-2 text-sm text-slate-200 transition hover:border-slate-500 disabled:opacity-40"
      >
        Reset all ECUs
      </button>
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
    await run(() => api.simulationRenameEcu(ecu.requestCanIdHex, name))
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
            <h4 className="font-semibold">{ecu.name}</h4>
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
                onClick={() => run(() => api.simulationRemoveEcu(ecu.requestCanIdHex))}
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

/** The requests worth offering first — the ones a tester actually sends. */
const REQUEST_CATALOGUE: { label: string; requestHex: string; responseHex: string }[] = [
  { label: '0x22 ReadDataByIdentifier', requestHex: '22 F1 90', responseHex: '62 F1 90 00' },
  { label: '0x2E WriteDataByIdentifier', requestHex: '2E F1 90 00', responseHex: '6E F1 90' },
  { label: '0x14 ClearDiagnosticInformation', requestHex: '14 FF FF FF', responseHex: '54' },
  { label: '0x19 ReadDTCInformation', requestHex: '19 02 FF', responseHex: '59 02 FF' },
  { label: '0x2F InputOutputControl', requestHex: '2F F1 90 03 01', responseHex: '6F F1 90 03' },
  { label: '0x31 RoutineControl', requestHex: '31 01 F0 00', responseHex: '71 01 F0 00' },
  { label: '0x34 RequestDownload', requestHex: '34 00 44 00 00 00 00 00 00 01 00', responseHex: '74 20 04 00' },
  { label: '0x36 TransferData', requestHex: '36 01', responseHex: '76 01' },
  { label: '0x37 RequestTransferExit', requestHex: '37', responseHex: '77' },
  { label: '0x28 CommunicationControl', requestHex: '28 01 01', responseHex: '68 01' },
  { label: '0x85 ControlDTCSetting', requestHex: '85 02', responseHex: 'C5 02' },
  { label: '0x87 LinkControl', requestHex: '87 01 01', responseHex: 'C7 01' },
  { label: '0x23 ReadMemoryByAddress', requestHex: '23 14 20 00 00 04', responseHex: '63 00 00 00 00' },
]

/** Services the engine's UDS plugin answers without help. */
const IMPLEMENTED_SERVICES = ['10', '11', '19', '22', '27', '31', '3E']

/**
 * Edit what one ECU answers to a particular request.
 *
 * This is the only way to get a positive response out of the services the engine's UDS plugin
 * does not implement, and the only way to make an ECU refuse or ignore a request it would
 * otherwise answer.
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

  async function save(vecNext: ResponseOverride[]) {
    if (!ecu) return
    setWorking(true)
    try {
      setOverrides(await api.setEcuOverrides(ecu.requestCanIdHex, vecNext))
      onError(null)
    } catch (e) {
      // The engine explains exactly which override it refused and why.
      onError(DescribeError(e))
    } finally {
      setWorking(false)
    }
  }

  function addFromCatalogue(requestHex: string, responseHex: string) {
    save([
      ...vecOverrides,
      {
        requestHex,
        matchTrailingBytes: false,
        action: 'substitute',
        responseHex,
        echoSpans: [],
        enabled: true,
        respondEvenIfSuppressed: false,
        note: '',
      },
    ])
  }

  return (
    <section className="rounded-lg border border-slate-800 bg-slate-900/50 p-4">
      <div className="flex items-baseline justify-between">
        <h3 className="text-sm font-medium uppercase tracking-wider text-slate-400">
          Responses — {ecu.name}
        </h3>
        <span className="text-xs text-slate-500">{vecOverrides.length} override(s)</span>
      </div>

      <p className="mt-2 text-xs text-slate-600">
        The engine&rsquo;s UDS plugin answers {IMPLEMENTED_SERVICES.map((s) => '0x' + s).join(', ')}.
        Every other service returns <span className="font-mono">7F .. 11</span> until you define
        a response here.
      </p>

      <div className="mt-3">
        <select
          value=""
          disabled={busy || working}
          onChange={(e) => {
            const entry = REQUEST_CATALOGUE.find((c) => c.requestHex === e.target.value)
            if (entry) addFromCatalogue(entry.requestHex, entry.responseHex)
          }}
          className="w-full rounded-md border border-slate-700 bg-slate-950 px-3 py-2 text-sm text-slate-200 outline-none focus:border-slate-500"
        >
          <option value="">Add a response for…</option>
          {REQUEST_CATALOGUE.map((entry) => (
            <option key={entry.requestHex} value={entry.requestHex}>
              {entry.label}
            </option>
          ))}
        </select>
      </div>

      {vecOverrides.length > 0 && (
        <ul className="mt-3 space-y-2">
          {vecOverrides.map((rule, iIndex) => (
            <OverrideRow
              key={`${rule.requestHex}-${iIndex}`}
              rule={rule}
              disabled={busy || working}
              onChange={(next) =>
                save(vecOverrides.map((existing, i) => (i === iIndex ? next : existing)))
              }
              onRemove={() => save(vecOverrides.filter((_, i) => i !== iIndex))}
            />
          ))}
        </ul>
      )}
    </section>
  )
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
        {rule.maskHex && rule.maskHex.includes('00') && <Badge tone="amber">wildcard</Badge>}
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
      const result = await api.setEcuTiming(ecu.requestCanIdHex, timing)
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

function ExchangeLog({ log }: { log: ExchangeEntry[] }) {
  return (
    <div>
      <h3 className="mb-2 text-sm font-medium uppercase tracking-wider text-slate-400">
        Exchange log
      </h3>
      {log.length === 0 ? (
        <p className="rounded-lg border border-slate-800 bg-slate-900/40 px-4 py-6 text-center text-sm text-slate-500">
          No requests yet. Pick an address and use a quick action.
        </p>
      ) : (
        <ul className="space-y-2">
          {log.map((entry) => (
            <ExchangeEntryView key={entry.id} result={entry.result} />
          ))}
        </ul>
      )}
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

      {!result.routed ? (
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

  const vecOptions: AddressOption[] = state.ecus.map((ecu) => ({
    canIdHex: ecu.requestCanIdHex,
    label: `${ecu.requestCanIdHex} — ${ecu.name}`,
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
