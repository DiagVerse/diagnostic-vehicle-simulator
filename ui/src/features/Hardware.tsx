import { useCallback, useEffect, useState } from 'react'
import { Badge } from '../components/primitives'
import { api, type HardwareStatus, type SerialPort } from '../shared/api'

/** The bus speeds an SLCAN adapter can select. */
const BITRATES = [10000, 20000, 50000, 100000, 125000, 250000, 500000, 800000, 1000000]

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
        setStatus(await api.hardwareStart(selected, bitrate))
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

        {status?.running && (
          <dl className="mt-4 flex flex-wrap gap-x-8 gap-y-2 text-sm">
            <div>
              <dt className="text-xs text-slate-500">Port</dt>
              <dd className="font-mono text-slate-300">{status.port}</dd>
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
