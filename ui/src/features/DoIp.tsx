import { useEffect, useState } from 'react'
import { Badge } from '../components/primitives'
import {
  api,
  type DoIpSettings,
  type DoIpStatus,
  type VehicleIdentity,
} from '../shared/api'

/**
 * The DoIP entity: put the simulation on an Ethernet wire, decide what it tells a tester about
 * itself, and make it misbehave on purpose.
 *
 * The last part is the point of the panel. A tester's handling of a vehicle that will not be
 * discovered, refuses routing activation, or negatively acknowledges everything is exactly what
 * is hardest to exercise against real hardware — you cannot ask a real vehicle to fail on
 * demand.
 */
export function DoIp() {
  const [status, setStatus] = useState<DoIpStatus | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [bind, setBind] = useState('0.0.0.0:13400')
  const [busy, setBusy] = useState(false)

  useEffect(() => {
    api
      .doipStatus()
      .then(setStatus)
      .catch((e) => setError(e instanceof Error ? e.message : String(e)))
  }, [])

  async function run(action: () => Promise<DoIpStatus>) {
    setBusy(true)
    try {
      setStatus(await action())
      setError(null)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="space-y-4">
      {error && (
        <div className="rounded-lg border border-red-900/60 bg-red-950/40 px-4 py-3 text-sm text-red-300">
          {error}
        </div>
      )}

      <section className="rounded-lg border border-slate-800 bg-slate-900/50 p-5">
        <div className="flex flex-wrap items-baseline justify-between gap-2">
          <h3 className="text-sm font-medium uppercase tracking-wider text-slate-400">
            DoIP entity
          </h3>
          {status?.running ? (
            <span className="flex items-center gap-2 text-xs text-slate-400">
              <Badge tone="emerald">listening on {status.boundAddress}</Badge>
              as entity 0x{status.entityAddressHex}
            </span>
          ) : (
            <Badge tone="slate">not listening</Badge>
          )}
        </div>

        <p className="mt-2 text-xs leading-relaxed text-slate-500">
          A tester discovers the vehicle over UDP and then opens a TCP connection to diagnose it.
          Binding to <span className="font-mono">0.0.0.0:13400</span> makes it reachable from
          another machine; <span className="font-mono">127.0.0.1</span> keeps it on this one.
        </p>

        <div className="mt-3 flex flex-wrap items-end gap-2">
          <label className="flex flex-col gap-1">
            <span className="text-xs text-slate-400">Bind address</span>
            <input
              value={bind}
              onChange={(e) => setBind(e.target.value)}
              disabled={status?.running}
              className="w-52 rounded-md border border-slate-700 bg-slate-950 px-3 py-2 font-mono text-sm text-slate-200 outline-none focus:border-slate-500 disabled:opacity-40"
            />
          </label>

          {status?.running ? (
            <button
              onClick={() => run(api.doipStop)}
              disabled={busy}
              className="rounded-md bg-rose-800 px-4 py-2 text-sm font-medium text-white transition hover:bg-rose-700 disabled:opacity-40"
            >
              Stop listening
            </button>
          ) : (
            <button
              onClick={() => run(() => api.doipStart(bind))}
              disabled={busy}
              className="rounded-md bg-emerald-700 px-4 py-2 text-sm font-medium text-white transition hover:bg-emerald-600 disabled:opacity-40"
            >
              Start listening
            </button>
          )}
        </div>

        {status && status.logicalAddressesHex.length > 0 && (
          <p className="mt-3 text-xs text-slate-500">
            Addresses a tester can target:{' '}
            <span className="font-mono text-slate-400">
              {status.logicalAddressesHex.map((hex) => `0x${hex}`).join(', ')}
            </span>
          </p>
        )}
      </section>

      <IdentityPanel onError={setError} />
      <SettingsPanel onError={setError} />
    </div>
  )
}

/**
 * What the vehicle announces about itself.
 *
 * This is what a tester reads in a vehicle identification response, so it is also how you make
 * the simulation impersonate a particular vehicle for a test that keys off the VIN.
 */
function IdentityPanel({ onError }: { onError: (message: string | null) => void }) {
  const [identity, setIdentity] = useState<VehicleIdentity | null>(null)
  const [busy, setBusy] = useState(false)
  const [saved, setSaved] = useState(false)

  useEffect(() => {
    api
      .vehicleIdentity()
      .then(setIdentity)
      .catch(() => {
        // No vehicle loaded yet; the panel simply stays empty rather than shouting about it.
        setIdentity(null)
      })
  }, [])

  async function save() {
    if (!identity) return
    setBusy(true)
    try {
      setIdentity(await api.setVehicleIdentity(identity))
      onError(null)
      setSaved(true)
      window.setTimeout(() => setSaved(false), 2000)
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }

  if (!identity) {
    return (
      <section className="rounded-lg border border-slate-800 bg-slate-900/50 p-5">
        <h3 className="text-sm font-medium uppercase tracking-wider text-slate-400">
          Vehicle identity
        </h3>
        <p className="mt-2 text-sm text-slate-500">
          Load a vehicle first — this is what it announces about itself.
        </p>
      </section>
    )
  }

  return (
    <section className="rounded-lg border border-slate-800 bg-slate-900/50 p-5">
      <div className="flex items-baseline justify-between">
        <h3 className="text-sm font-medium uppercase tracking-wider text-slate-400">
          Vehicle identity
        </h3>
        {saved && <Badge tone="emerald">saved</Badge>}
      </div>

      <p className="mt-2 text-xs leading-relaxed text-slate-500">
        What goes in the vehicle announcement and in the answer to an identification request.
        Leave the VIN blank for a vehicle that has not been programmed with one &mdash; it is
        announced as unset rather than as a plausible-looking wrong VIN.
      </p>

      <div className="mt-3 grid gap-3 sm:grid-cols-2">
        <Field
          label="VIN (17 characters)"
          value={identity.vin ?? ''}
          mono
          onChange={(vin) => setIdentity({ ...identity, vin: vin || null })}
        />
        <Field
          label="EID (6 bytes, hex)"
          value={identity.eidHex ?? ''}
          mono
          onChange={(eidHex) => setIdentity({ ...identity, eidHex: eidHex || null })}
        />
        <Field
          label="GID (6 bytes, hex)"
          value={identity.gidHex ?? ''}
          mono
          onChange={(gidHex) => setIdentity({ ...identity, gidHex: gidHex || null })}
        />
        <Choice
          label="Further action required"
          value={identity.furtherActionRequired}
          options={[
            [0x00, '0x00 — none'],
            [0x10, '0x10 — routing activation for central security'],
          ]}
          onChange={(furtherActionRequired) =>
            setIdentity({ ...identity, furtherActionRequired })
          }
        />
        <Choice
          label="VIN/GID synchronization"
          value={identity.vinGidSyncStatus}
          options={[
            [0x00, '0x00 — synchronized'],
            [0x10, '0x10 — incomplete (tester should wait and re-ask)'],
          ]}
          onChange={(vinGidSyncStatus) => setIdentity({ ...identity, vinGidSyncStatus })}
        />
      </div>

      <button
        onClick={save}
        disabled={busy}
        className="mt-3 rounded-md bg-emerald-700 px-4 py-2 text-sm font-medium text-white transition hover:bg-emerald-600 disabled:opacity-40"
      >
        Apply identity
      </button>
    </section>
  )
}

/**
 * The entity's own parameters, and the faults it can be made to inject.
 *
 * Kept in one panel because they answer the same question — "what will this entity say?" — and
 * separating them would hide that a healthy-looking parameter and an injected fault are the
 * same kind of setting from a tester's point of view.
 */
function SettingsPanel({ onError }: { onError: (message: string | null) => void }) {
  const [settings, setSettings] = useState<DoIpSettings | null>(null)
  const [busy, setBusy] = useState(false)

  useEffect(() => {
    api
      .doipSettings()
      .then(setSettings)
      .catch((e) => onError(e instanceof Error ? e.message : String(e)))
  }, [onError])

  async function apply(next: DoIpSettings) {
    setBusy(true)
    try {
      setSettings(await api.setDoIpSettings(next))
      onError(null)
    } catch (e) {
      // The engine refuses a code no vehicle would send, and says which.
      onError(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }

  if (!settings) {
    return null
  }

  return (
    <section className="rounded-lg border border-slate-800 bg-slate-900/50 p-5">
      <div className="flex items-baseline justify-between">
        <h3 className="text-sm font-medium uppercase tracking-wider text-slate-400">
          Entity parameters &amp; fault injection
        </h3>
        {settings.isInjectingFaults && <Badge tone="amber">injecting faults</Badge>}
      </div>

      <div className="mt-3 grid gap-3 sm:grid-cols-2">
        <Choice
          label="Diagnostic power mode"
          value={settings.powerMode}
          options={[
            [0x01, '0x01 — ready'],
            [0x00, '0x00 — not ready'],
            [0x02, '0x02 — not supported'],
          ]}
          onChange={(powerMode) => apply({ ...settings, powerMode })}
        />
        <Choice
          label="Node type"
          value={settings.nodeType}
          options={[
            [0x00, '0x00 — gateway'],
            [0x01, '0x01 — node'],
          ]}
          onChange={(nodeType) => apply({ ...settings, nodeType })}
        />
        <NumberField
          label="Max concurrent sockets"
          value={settings.maxSockets}
          disabled={busy}
          onCommit={(maxSockets) => apply({ ...settings, maxSockets })}
        />
        <NumberField
          label="Max data size (bytes)"
          value={settings.maxDataSize}
          disabled={busy}
          onCommit={(maxDataSize) => apply({ ...settings, maxDataSize })}
        />
      </div>

      <p className="mt-4 text-xs leading-relaxed text-slate-500">
        Below this line the entity stops behaving like a healthy one. That is the point: a
        tester&rsquo;s handling of a vehicle that will not be found, refuses routing activation,
        or rejects every message is the hardest thing to exercise against real hardware, because
        you cannot ask a real vehicle to fail on demand.
      </p>

      <div className="mt-3 space-y-3">
        <label className="flex items-center gap-2 text-sm text-slate-300">
          <input
            type="checkbox"
            checked={settings.suppressIdentificationResponse}
            disabled={busy}
            onChange={(e) =>
              apply({ ...settings, suppressIdentificationResponse: e.target.checked })
            }
            className="accent-amber-600"
          />
          Answer nothing to a vehicle identification request
          <span className="text-xs text-slate-500">(the vehicle cannot be discovered)</span>
        </label>

        <div className="grid gap-3 sm:grid-cols-3">
          <Choice
            label="Force routing activation code"
            value={settings.forcedRoutingActivationCode ?? -1}
            options={[
              [-1, 'decide normally'],
              [0x00, '0x00 — unknown source address'],
              [0x01, '0x01 — all sockets registered'],
              [0x02, '0x02 — source address mismatch'],
              [0x03, '0x03 — address in use elsewhere'],
              [0x04, '0x04 — missing authentication'],
              [0x06, '0x06 — unsupported activation type'],
            ]}
            onChange={(value) =>
              apply({
                ...settings,
                forcedRoutingActivationCode: value < 0 ? null : value,
              })
            }
          />
          <Choice
            label="Force diagnostic NACK"
            value={settings.forcedDiagnosticNack ?? -1}
            options={[
              [-1, 'route normally'],
              [0x02, '0x02 — invalid source address (closes socket)'],
              [0x03, '0x03 — unknown target address'],
              [0x04, '0x04 — message too large'],
              [0x05, '0x05 — out of memory'],
              [0x06, '0x06 — target unreachable'],
            ]}
            onChange={(value) =>
              apply({ ...settings, forcedDiagnosticNack: value < 0 ? null : value })
            }
          />
          <Choice
            label="Force generic header NACK"
            value={settings.forcedHeaderNack ?? -1}
            options={[
              [-1, 'read normally'],
              [0x00, '0x00 — bad pattern (closes socket)'],
              [0x01, '0x01 — unknown payload type'],
              [0x02, '0x02 — message too large'],
              [0x03, '0x03 — out of memory'],
              [0x04, '0x04 — invalid length (closes socket)'],
            ]}
            onChange={(value) =>
              apply({ ...settings, forcedHeaderNack: value < 0 ? null : value })
            }
          />
        </div>
      </div>
    </section>
  )
}

function Field({
  label,
  value,
  mono,
  onChange,
}: {
  label: string
  value: string
  mono?: boolean
  onChange: (value: string) => void
}) {
  return (
    <label className="flex flex-col gap-1">
      <span className="text-xs text-slate-400">{label}</span>
      <input
        value={value}
        onChange={(e) => onChange(e.target.value)}
        className={`rounded-md border border-slate-700 bg-slate-950 px-3 py-2 text-sm text-slate-200 outline-none focus:border-slate-500 ${
          mono ? 'font-mono' : ''
        }`}
      />
    </label>
  )
}

function Choice({
  label,
  value,
  options,
  onChange,
}: {
  label: string
  value: number
  options: [number, string][]
  onChange: (value: number) => void
}) {
  return (
    <label className="flex flex-col gap-1">
      <span className="text-xs text-slate-400">{label}</span>
      <select
        value={value}
        onChange={(e) => onChange(Number(e.target.value))}
        className="rounded-md border border-slate-700 bg-slate-950 px-3 py-2 text-sm text-slate-200 outline-none focus:border-slate-500"
      >
        {options.map(([optionValue, optionLabel]) => (
          <option key={optionValue} value={optionValue}>
            {optionLabel}
          </option>
        ))}
      </select>
    </label>
  )
}

/**
 * A number applied when you leave the field, not on every keystroke.
 *
 * Applying per keystroke would send "1", "10", "102"… to the engine on the way to 1024, and one
 * of those is below the minimum it accepts — so you would get an error for a value you were
 * halfway through typing.
 */
function NumberField({
  label,
  value,
  disabled,
  onCommit,
}: {
  label: string
  value: number
  disabled: boolean
  onCommit: (value: number) => void
}) {
  // The committed value is the source of truth; the text is the half-typed version of it. Keyed
  // on the value so a change from the engine resets the box, without an effect that writes state
  // during render.
  const [committed, setCommitted] = useState(value)
  const [text, setText] = useState(String(value))

  if (committed !== value) {
    setCommitted(value)
    setText(String(value))
  }

  return (
    <label className="flex flex-col gap-1">
      <span className="text-xs text-slate-400">{label}</span>
      <input
        value={text}
        disabled={disabled}
        onChange={(e) => setText(e.target.value)}
        onBlur={() => {
          const parsed = Number(text)
          if (Number.isFinite(parsed) && parsed !== value) {
            onCommit(parsed)
          }
        }}
        className="rounded-md border border-slate-700 bg-slate-950 px-3 py-2 font-mono text-sm text-slate-200 outline-none focus:border-slate-500 disabled:opacity-40"
      />
    </label>
  )
}
