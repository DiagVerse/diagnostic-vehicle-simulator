import { useCallback, useEffect, useState } from 'react'
import { Badge } from '../components/primitives'
import {
  api,
  type BusLoad,
  type BusLoadSample,
  type HardwareStatus,
  type SerialPort,
} from '../shared/api'

/** The bus speeds an SLCAN adapter can select. */
const BITRATES = [10000, 20000, 50000, 100000, 125000, 250000, 500000, 800000, 1000000]

/**
 * Host-to-adapter line speeds, which are not the CAN bitrate.
 *
 * Offered explicitly because detection only works if the adapter answers something, and a
 * firmware that implements none of the identity commands cannot be asked. The fallback is then
 * 115200 — which is a guess, and on a dongle that runs faster it is a guess that throws away
 * seven eighths of the link and loses the middle of every long request.
 */
const SERIAL_BAUDS = [115200, 230400, 250000, 460800, 500000, 921600, 1000000, 2000000]

/**
 * Put the simulation on a wire.
 *
 * Two ways to use this. With a USB-CAN adapter, pick its port and the vehicle's bus speed, and
 * the simulated ECUs answer a real tester on a real bus. Without one, a virtual port pair — a
 * pseudo-terminal on macOS or Linux, com0com on Windows — lets a tester tool on this machine
 * talk to the engine with no hardware between them.
 */
export function Hardware() {
  const [ports, setPorts] = useState<SerialPort[]>([])
  const [serialSupported, setSerialSupported] = useState(true)
  const [status, setStatus] = useState<HardwareStatus | null>(null)
  const [selected, setSelected] = useState('')
  const [bitrate, setBitrate] = useState(500000)
  /** 0 means "ask the adapter", which is right until an adapter will not say. */
  const [serialBaud, setSerialBaud] = useState(0)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const refresh = useCallback(async () => {
    try {
      const [portList, current] = await Promise.all([api.serialPorts(), api.hardwareStatus()])
      setPorts(portList.ports)
      setSerialSupported(portList.serialSupported)
      setStatus(current)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    }
  }, [])

  useEffect(() => {
    // Synchronising with an external system — the engine, over HTTP — which is what an effect
    // is for. While a link is up the frame counters are the only sign it is alive, so they are
    // polled rather than fetched once.
    // oxlint-disable-next-line react/set-state-in-effect
    refresh()
    const id = setInterval(refresh, 2000)
    return () => clearInterval(id)
  }, [refresh])

  async function toggle() {
    setBusy(true)
    try {
      if (status?.running) {
        setStatus(await api.hardwareStop())
      } else {
        setStatus(await api.hardwareStart(selected, bitrate, serialBaud || undefined))
      }
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

      {!serialSupported && (
        <div className="rounded-lg border border-amber-900/60 bg-amber-950/30 px-4 py-3 text-sm text-amber-300">
          This build has no serial support compiled in, so no ports can be listed. A virtual
          port can still be opened by typing its path below.
        </div>
      )}

      <section className="rounded-lg border border-slate-800 bg-slate-900/50 p-5">
        <div className="flex items-baseline justify-between">
          <h3 className="text-sm font-medium uppercase tracking-wider text-slate-400">
            CAN link
          </h3>
          {status?.running ? (
            <Badge tone="emerald">on the bus</Badge>
          ) : (
            <Badge tone="slate">not connected</Badge>
          )}
        </div>

        <div className="mt-3 grid gap-2 sm:grid-cols-[1fr_10rem_auto] sm:items-end">
          <label className="block">
            <span className="text-xs text-slate-400">Port</span>
            <input
              list="serial-ports"
              value={selected}
              onChange={(e) => setSelected(e.target.value)}
              placeholder="/dev/tty.usbmodem1101 or COM3"
              disabled={status?.running}
              className="mt-1 w-full rounded-md border border-slate-700 bg-slate-950 px-3 py-2 font-mono text-sm text-slate-200 outline-none focus:border-slate-500 disabled:opacity-50"
            />
            <datalist id="serial-ports">
              {ports.map((port) => (
                <option key={port.name} value={port.name}>
                  {port.description}
                </option>
              ))}
            </datalist>
          </label>

          <label className="block">
            <span className="text-xs text-slate-400">Bus speed</span>
            <select
              value={bitrate}
              onChange={(e) => setBitrate(Number(e.target.value))}
              disabled={status?.running}
              className="mt-1 w-full rounded-md border border-slate-700 bg-slate-950 px-2 py-2 text-sm text-slate-200 outline-none focus:border-slate-500 disabled:opacity-50"
            >
              {BITRATES.map((rate) => (
                <option key={rate} value={rate}>
                  {rate / 1000} kbit/s
                </option>
              ))}
            </select>
          </label>

          <label className="block">
            <span className="text-xs text-slate-400">Host link</span>
            <select
              value={serialBaud}
              onChange={(e) => setSerialBaud(Number(e.target.value))}
              disabled={status?.running}
              className="mt-1 w-full rounded-md border border-slate-700 bg-slate-950 px-2 py-2 text-sm text-slate-200 outline-none focus:border-slate-500 disabled:opacity-50"
            >
              <option value={0}>Ask the adapter</option>
              {SERIAL_BAUDS.map((rate) => (
                <option key={rate} value={rate}>
                  {rate} baud
                </option>
              ))}
            </select>
          </label>

          <button
            onClick={toggle}
            disabled={busy || (!status?.running && selected.trim().length === 0)}
            className={`rounded-md px-4 py-2 text-sm font-medium text-white transition disabled:opacity-40 ${
              status?.running ? 'bg-rose-800 hover:bg-rose-700' : 'bg-emerald-700 hover:bg-emerald-600'
            }`}
          >
            {status?.running ? 'Disconnect' : 'Connect'}
          </button>
        </div>

        {status?.running && <BusLoadChart />}

        {status?.running && (
          <dl className="mt-4 flex flex-wrap gap-x-8 gap-y-2 text-sm">
            <div>
              <dt className="text-xs text-slate-500">Port</dt>
              <dd className="font-mono text-slate-300">{status.port}</dd>
            </div>
            <div>
              <dt className="text-xs text-slate-500">Host link</dt>
              <dd className="font-mono text-slate-300">
                {status.serialBaudBps === null ? '—' : `${status.serialBaudBps} baud`}
              </dd>
              {status.serialBaudSource && (
                <dd className="text-xs text-slate-500">
                  {status.serialBaudSource}
                  {status.adapterVersion ? ` · ${status.adapterVersion}` : ''}
                </dd>
              )}
            </div>
            <div>
              <dt className="text-xs text-slate-500">Frames in</dt>
              <dd className="font-mono text-slate-300">{status.framesReceived}</dd>
            </div>
            <div>
              <dt className="text-xs text-slate-500">Frames out</dt>
              <dd className="font-mono text-slate-300">{status.framesSent}</dd>
            </div>
          </dl>
        )}
      </section>

      <div className="rounded-lg border border-slate-800 bg-slate-900/40 px-4 py-3 text-xs leading-relaxed text-slate-500">
        <p className="mb-2 text-slate-400">Connecting a tester without a CAN adapter</p>
        <p>
          Create a virtual port pair and give the engine one end; your tester tool opens the
          other and speaks SLCAN to it, exactly as it would to a CANable or USBtin.
        </p>
        <pre className="mt-2 overflow-x-auto rounded bg-slate-950/70 p-2 font-mono text-[11px] text-slate-400">
{`socat -d -d pty,raw,echo=0,link=/tmp/dvsim-engine \\
             pty,raw,echo=0,link=/tmp/dvsim-tester`}
        </pre>
        <p className="mt-2">
          Then connect the engine to <code>/tmp/dvsim-engine</code>. On Windows, com0com creates
          the same kind of pair. <code>raw</code> and <code>echo=0</code> are both required —
          the default line discipline rewrites the carriage return SLCAN ends every line with.
        </p>
      </div>
    </div>
  )
}


// ---------------------------------------------------------------------------------------
// Bus load
// ---------------------------------------------------------------------------------------

/** How often to pull the series. The engine buckets by the second, so this is generous. */
const BUS_LOAD_POLL_MS = 1000

/**
 * How busy the bus is, second by second.
 *
 * Plotted as a range rather than a line, because a load figure computed from decoded frames is
 * one: bit stuffing depends on the payload's bit pattern and on a CRC the frame record does not
 * carry, so the true figure lies between the nominal and the worst case. A single line would be
 * a number that is subtly wrong presented as one that is right.
 */
function BusLoadChart() {
  const [load, setLoad] = useState<BusLoad | null>(null)

  useEffect(() => {
    let cancelled = false
    const poll = () => {
      api
        .busLoad()
        .then((next) => {
          if (!cancelled) setLoad(next)
        })
        .catch(() => {
          /* A failed poll is a gap in the chart, not something to shout about. */
        })
    }
    poll()
    const id = setInterval(poll, BUS_LOAD_POLL_MS)
    return () => {
      cancelled = true
      clearInterval(id)
    }
  }, [])

  if (!load || load.bitrateBps === 0) return null

  const samples = load.samples
  const peakPercent = (load.peak?.loadWorstCase ?? 0) * 100
  // A fixed 10% floor on the axis stops a quiet bus drawing a dramatic-looking mountain range
  // out of a fraction of a percent.
  const ceiling = Math.max(10, Math.ceil(peakPercent / 10) * 10)

  return (
    <section className="mt-4 rounded-lg border border-slate-800 bg-slate-900/50 p-4">
      <div className="flex flex-wrap items-baseline justify-between gap-2">
        <h3 className="text-sm font-medium uppercase tracking-wider text-slate-400">Bus load</h3>
        <span className="font-mono text-xs text-slate-500">
          {(load.bitrateBps / 1000).toFixed(0)} kbit/s · {samples.length}s history
        </span>
      </div>

      <div className="mt-3 flex flex-wrap gap-x-8 gap-y-2">
        <Figure
          label="now"
          sample={load.current}
          hint="the second still being filled, so it reads low until it closes"
        />
        <Figure label="peak" sample={load.peak} hint="busiest completed second" />
      </div>

      {samples.length === 0 ? (
        <p className="mt-3 text-xs text-slate-500">
          Nothing has crossed the bus yet. The chart starts at the first frame.
        </p>
      ) : (
        <BusLoadPlot samples={samples} ceiling={ceiling} />
      )}

      <p className="mt-3 text-[11px] leading-relaxed text-slate-500">
        The band is the range the true figure lies in. Bit stuffing adds bits that depend on each
        frame&rsquo;s bit pattern and on a CRC a decoded frame does not carry, so the solid line
        is a floor and the top of the band is the most the standard allows.
      </p>
    </section>
  )
}

function Figure({
  label,
  sample,
  hint,
}: {
  label: string
  sample: BusLoadSample | null
  hint: string
}) {
  return (
    <div title={hint}>
      <div className="text-xs text-slate-500">{label}</div>
      <div className="font-mono text-lg text-slate-200 tabular-nums">
        {sample ? `${(sample.loadNominal * 100).toFixed(1)}–${(sample.loadWorstCase * 100).toFixed(1)}%` : '—'}
      </div>
      <div className="font-mono text-[11px] text-slate-500 tabular-nums">
        {sample ? `${sample.frames} frames` : ''}
      </div>
    </div>
  )
}

/**
 * The series as an area between nominal and worst case.
 *
 * Drawn to one scale: the same y-mapping places the band, the grid lines and the labels, so a
 * gridline labelled 20% sits exactly where 20% is.
 */
function BusLoadPlot({ samples, ceiling }: { samples: BusLoadSample[]; ceiling: number }) {
  const W = 640
  const H = 132
  const PAD_L = 34
  const PAD_B = 16
  const PAD_T = 6

  const plotW = W - PAD_L - 6
  const plotH = H - PAD_B - PAD_T
  const n = Math.max(samples.length, 2)
  const x = (i: number) => PAD_L + (i / (n - 1)) * plotW
  const y = (percent: number) => PAD_T + plotH - (percent / ceiling) * plotH

  const top = samples.map((s, i) => `${x(i).toFixed(1)},${y(s.loadWorstCase * 100).toFixed(1)}`)
  const bottom = samples
    .map((s, i) => `${x(i).toFixed(1)},${y(s.loadNominal * 100).toFixed(1)}`)
    .reverse()
  const bandPath = `M${top.join(' L')} L${bottom.join(' L')} Z`
  const linePath = `M${samples.map((s, i) => `${x(i).toFixed(1)},${y(s.loadNominal * 100).toFixed(1)}`).join(' L')}`

  const ticks = [0, ceiling / 2, ceiling]

  return (
    <div className="mt-3 overflow-x-auto">
      <svg
        viewBox={`0 0 ${W} ${H}`}
        width="100%"
        height={H}
        role="img"
        aria-label={`Bus load over the last ${samples.length} seconds, peaking near ${ceiling} percent`}
      >
        {ticks.map((t) => (
          <g key={t}>
            <line
              x1={PAD_L}
              y1={y(t)}
              x2={W - 6}
              y2={y(t)}
              stroke="currentColor"
              strokeWidth="1"
              className="text-slate-800"
            />
            <text
              x={PAD_L - 6}
              y={y(t) + 3.5}
              textAnchor="end"
              className="fill-slate-500"
              style={{ fontSize: '9px', fontFamily: 'ui-monospace, monospace' }}
            >
              {t.toFixed(0)}%
            </text>
          </g>
        ))}

        <path d={bandPath} className="fill-sky-500/20" stroke="none" />
        <path d={linePath} className="stroke-sky-400" strokeWidth="1.5" fill="none" />
        {samples.length > 0 && (
          <circle
            cx={x(samples.length - 1)}
            cy={y(samples[samples.length - 1].loadNominal * 100)}
            r="2.5"
            className="fill-sky-300"
          />
        )}

        <text
          x={PAD_L}
          y={H - 4}
          className="fill-slate-500"
          style={{ fontSize: '9px', fontFamily: 'ui-monospace, monospace' }}
        >
          -{samples.length}s
        </text>
        <text
          x={W - 6}
          y={H - 4}
          textAnchor="end"
          className="fill-slate-500"
          style={{ fontSize: '9px', fontFamily: 'ui-monospace, monospace' }}
        >
          now
        </text>
      </svg>
    </div>
  )
}
