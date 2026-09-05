import { useEffect, useRef, useState } from 'react'
import { Badge, DetailRow, type BadgeTone } from '../components/primitives'
import {
  api,
  type SimulationEcu,
  type SimulationRequestResult,
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

      <LogLoader onLoad={load} busy={busy} />

      {!state?.loaded ? (
        <EmptyState />
      ) : (
        <div className="grid gap-6 lg:grid-cols-[360px_1fr]">
          <EcuList state={state} onReset={reset} busy={busy} />

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
            <ExchangeLog log={log} />
          </section>
        </div>
      )}
    </div>
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
  busy,
}: {
  state: SimulationState
  onReset: () => void
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
        <EcuCard key={ecu.requestCanIdHex} ecu={ecu} />
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

function EcuCard({ ecu }: { ecu: SimulationEcu }) {
  return (
    <div className="rounded-lg border border-slate-800 bg-slate-900/50 p-4">
      <div className="flex items-center justify-between">
        <h4 className="font-semibold">{ecu.name}</h4>
        <span className="font-mono text-xs text-slate-400">
          {ecu.requestCanIdHex} → {ecu.responseCanIdHex}
        </span>
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
          <div key={response.responseCanIdHex} className="mt-1 flex items-start gap-2">
            <span className="text-slate-600">←</span>
            <span className="text-slate-500">{response.responseCanIdHex}</span>
            {response.suppressed ? (
              <span className="text-slate-500">
                (response suppressed; now in {response.sessionName})
              </span>
            ) : (
              <span className={IsNegative(response.responseHex) ? 'text-amber-400' : 'text-emerald-400'}>
                {response.responseHex}
              </span>
            )}
          </div>
        ))
      )}
    </li>
  )
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
