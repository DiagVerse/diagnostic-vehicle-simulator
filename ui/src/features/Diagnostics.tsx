import { useEffect, useRef, useState } from 'react'
import { api, type EcuState, type RequestResult } from '../shared/api'

/** A quick-action button maps a human label to a fixed UDS request (hex). */
const QUICK_ACTIONS: { label: string; hex: string }[] = [
  { label: 'Read VIN', hex: '22 F1 90' },
  { label: 'Read DTCs', hex: '19 02 FF' },
  { label: 'Enter Extended', hex: '10 03' },
  { label: 'Enter Default', hex: '10 01' },
  { label: 'Request Seed', hex: '27 01' },
  { label: 'Send Key', hex: '27 02 AA BB CC DD' },
  { label: 'ECU Reset', hex: '11 01' },
  { label: 'Tester Present', hex: '3E 00' },
]

interface LogEntry {
  id: number
  result: RequestResult
}

/** Interactive view of the running virtual ECU: send UDS requests, watch state change. */
export function Diagnostics() {
  const [ecu, setEcu] = useState<EcuState | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [log, setLog] = useState<LogEntry[]>([])
  const [hexInput, setHexInput] = useState('22 F1 90')
  const [busy, setBusy] = useState(false)
  const nextId = useRef(1)

  useEffect(() => {
    refreshState()
  }, [])

  async function refreshState() {
    try {
      setEcu(await api.ecuState())
      setError(null)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    }
  }

  async function send(hex: string) {
    setBusy(true)
    try {
      const result = await api.ecuRequest(hex)
      setLog((prev) => [{ id: nextId.current++, result }, ...prev].slice(0, 50))
      await refreshState()
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }

  async function reset() {
    setBusy(true)
    try {
      setEcu(await api.ecuReset())
      setLog([])
      setError(null)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="grid gap-6 lg:grid-cols-[320px_1fr]">
      <EcuStateCard ecu={ecu} onReset={reset} busy={busy} />

      <section className="space-y-4">
        {error && (
          <div className="rounded-lg border border-red-900/60 bg-red-950/40 px-4 py-3 text-sm text-red-300">
            {error}
          </div>
        )}

        {ecu && !ecu.protocolLoaded && (
          <div className="rounded-lg border border-amber-900/60 bg-amber-950/30 px-4 py-3 text-sm text-amber-300">
            UDS protocol plugin not loaded — copy <code>libuds_plugin.*</code> into{' '}
            <code>plugins.d/</code> and restart the engine.
          </div>
        )}

        <div>
          <h3 className="mb-2 text-sm font-medium uppercase tracking-wider text-slate-400">
            Quick actions
          </h3>
          <div className="flex flex-wrap gap-2">
            {QUICK_ACTIONS.map((a) => (
              <button
                key={a.label}
                disabled={busy}
                onClick={() => send(a.hex)}
                className="rounded-md border border-slate-700 bg-slate-800 px-3 py-1.5 text-sm text-slate-200 transition hover:border-slate-500 hover:bg-slate-700 disabled:opacity-40"
              >
                {a.label}
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
              if (hexInput.trim()) send(hexInput)
            }}
          >
            <input
              value={hexInput}
              onChange={(e) => setHexInput(e.target.value)}
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

        <ExchangeLog log={log} />
      </section>
    </div>
  )
}

function EcuStateCard({
  ecu,
  onReset,
  busy,
}: {
  ecu: EcuState | null
  onReset: () => void
  busy: boolean
}) {
  return (
    <aside className="h-fit rounded-lg border border-slate-800 bg-slate-900/50 p-5">
      {!ecu ? (
        <p className="text-sm text-slate-500">Loading ECU…</p>
      ) : (
        <>
          <div className="flex items-center justify-between">
            <h3 className="font-semibold">{ecu.name}</h3>
            <span className="font-mono text-xs text-slate-500">
              0x{ecu.logicalAddress.toString(16).toUpperCase()}
            </span>
          </div>

          <dl className="mt-4 space-y-3 text-sm">
            <Row label="Session">
              <Badge tone="sky">{ecu.sessionName}</Badge>
            </Row>
            <Row label="Security">
              {ecu.securityUnlocked ? (
                <Badge tone="emerald">Unlocked (L{ecu.securityLevel})</Badge>
              ) : (
                <Badge tone="slate">Locked</Badge>
              )}
            </Row>
            <Row label="Services">
              <span className="font-mono text-xs text-slate-300">
                {ecu.supportedServices
                  .map((s) => '0x' + s.toString(16).toUpperCase().padStart(2, '0'))
                  .join(' ')}
              </span>
            </Row>
            <Row label="DIDs">
              <span className="font-mono text-xs text-slate-300">
                {ecu.dids
                  .map((d) => '0x' + d.toString(16).toUpperCase().padStart(4, '0'))
                  .join(' ')}
              </span>
            </Row>
            <Row label="DTCs">
              <span className="text-slate-300">{ecu.dtcCount}</span>
            </Row>
          </dl>

          <button
            onClick={onReset}
            disabled={busy}
            className="mt-5 w-full rounded-md border border-slate-700 bg-slate-800 px-3 py-2 text-sm text-slate-200 transition hover:border-slate-500 disabled:opacity-40"
          >
            Reset ECU
          </button>
        </>
      )}
    </aside>
  )
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex items-start justify-between gap-3">
      <dt className="text-slate-500">{label}</dt>
      <dd className="text-right">{children}</dd>
    </div>
  )
}

function Badge({ tone, children }: { tone: 'sky' | 'emerald' | 'slate'; children: React.ReactNode }) {
  const tones: Record<string, string> = {
    sky: 'bg-sky-950 text-sky-300 border-sky-800',
    emerald: 'bg-emerald-950 text-emerald-300 border-emerald-800',
    slate: 'bg-slate-800 text-slate-300 border-slate-700',
  }
  return (
    <span className={`rounded-full border px-2 py-0.5 text-xs ${tones[tone]}`}>{children}</span>
  )
}

function ExchangeLog({ log }: { log: LogEntry[] }) {
  return (
    <div>
      <h3 className="mb-2 text-sm font-medium uppercase tracking-wider text-slate-400">
        Exchange log
      </h3>
      {log.length === 0 ? (
        <p className="rounded-lg border border-slate-800 bg-slate-900/40 px-4 py-6 text-center text-sm text-slate-500">
          No requests yet. Use a quick action or send a raw request.
        </p>
      ) : (
        <ul className="space-y-2">
          {log.map((entry) => (
            <li
              key={entry.id}
              className="rounded-md border border-slate-800 bg-slate-950/60 p-3 font-mono text-xs"
            >
              <div className="text-slate-400">
                <span className="text-slate-600">→</span> {entry.result.requestHex}
              </div>
              {entry.result.error ? (
                <div className="mt-1 text-red-400">
                  <span className="text-slate-600">✗</span> {entry.result.error}
                </div>
              ) : entry.result.suppressed ? (
                <div className="mt-1 text-slate-500">
                  <span className="text-slate-600">←</span> (positive response suppressed)
                </div>
              ) : (
                <div className={`mt-1 ${isNegative(entry.result.responseHex) ? 'text-amber-400' : 'text-emerald-400'}`}>
                  <span className="text-slate-600">←</span> {entry.result.responseHex}
                </div>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}

/** A UDS negative response starts with 0x7F. */
function isNegative(responseHex: string): boolean {
  return responseHex.trim().toUpperCase().startsWith('7F')
}
