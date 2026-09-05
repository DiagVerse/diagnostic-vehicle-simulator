import { useMemo, useState } from 'react'
import {
  FormatEventTime,
  SaveTrafficLog,
  useTrafficFeed,
  type TrafficEntry,
  type TrafficEvent,
} from '../shared/traffic'

/**
 * How much a standalone monitor window keeps, and how much the panel inside the app keeps.
 *
 * They differ on purpose. The popup is a window of its own with nothing else competing for the
 * tab's memory, so it can afford real history; the in-app panel sits alongside the whole rest
 * of the UI and only needs to show what just happened.
 */
const WINDOW_BUFFER = 5000
const PANEL_BUFFER = 60

/** Open the monitor in its own browser window, with its own memory. */
export function OpenMonitorWindow(): void {
  window.open(
    `${window.location.origin}${window.location.pathname}#monitor`,
    'dvsim-traffic-monitor',
    'width=1180,height=760,noopener=no',
  )
}

/**
 * The live view of everything crossing the simulator.
 *
 * Two shapes from one component. `standalone` is the popup: its own window, its own buffer, and
 * the room to keep thousands of events — which is the point of putting it in a separate window
 * rather than a panel, because a busy bus filling a buffer inside the main tab is what would
 * make the app itself stutter.
 */
export function TrafficMonitor({ standalone = false }: { standalone?: boolean }) {
  const { entries, status, isPaused, setPaused, totalSeen, clear } = useTrafficFeed(
    standalone ? WINDOW_BUFFER : PANEL_BUFFER,
  )
  const [filter, setFilter] = useState('')
  const [showFrames, setShowFrames] = useState(true)

  const vecVisible = useMemo(
    () => FilterEntries(entries, filter, showFrames),
    [entries, filter, showFrames],
  )

  const uDropped = Math.max(0, totalSeen - entries.length)

  return (
    <div className={standalone ? 'flex h-screen flex-col bg-slate-950 p-4' : 'space-y-2'}>
      <div className="flex flex-wrap items-center gap-2">
        <h3 className="text-sm font-medium uppercase tracking-wider text-slate-400">
          Communication log
        </h3>
        <StatusPill status={status} isPaused={isPaused} />

        <span className="text-xs text-slate-500">
          {entries.length} held · {totalSeen} seen
          {uDropped > 0 && ` · ${uDropped} dropped`}
        </span>

        <div className="ml-auto flex flex-wrap items-center gap-2">
          <input
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            placeholder="filter: 7E0, 22 F1, Engine…"
            className="w-44 rounded-md border border-slate-700 bg-slate-950 px-2 py-1 text-xs text-slate-200 outline-none focus:border-slate-500"
          />
          <label className="flex items-center gap-1.5 text-xs text-slate-400">
            <input
              type="checkbox"
              checked={showFrames}
              onChange={(e) => setShowFrames(e.target.checked)}
              className="accent-sky-600"
            />
            frames
          </label>
          <SmallButton onClick={() => setPaused(!isPaused)}>
            {isPaused ? 'Resume' : 'Pause'}
          </SmallButton>
          <SmallButton onClick={clear}>Clear</SmallButton>
          <SmallButton onClick={() => SaveTrafficLog(entries, totalSeen)}>Save log</SmallButton>
          {!standalone && (
            <SmallButton onClick={OpenMonitorWindow} title="Open in its own window">
              Open monitor window
            </SmallButton>
          )}
        </div>
      </div>

      {uDropped > 0 && standalone && (
        <p className="rounded border border-amber-900/50 bg-amber-950/20 px-2 py-1 text-[11px] text-amber-400/90">
          This is a window onto the traffic, not a complete record — {uDropped} older event(s)
          have been dropped to keep memory bounded. Save the log before it wraps if you need it.
        </p>
      )}

      <ul
        className={`space-y-0.5 overflow-y-auto rounded-lg border border-slate-800 bg-slate-950/60 p-2 font-mono text-[11px] ${
          standalone ? 'flex-1' : 'max-h-96'
        }`}
      >
        {vecVisible.length === 0 ? (
          <li className="px-1 py-6 text-center font-sans text-sm text-slate-500">
            {entries.length === 0
              ? 'Nothing yet. Send a request, or put the simulation on a wire and let a tester talk to it.'
              : 'Nothing matches that filter.'}
          </li>
        ) : (
          vecVisible.map((entry) => <EventLine key={entry.id} event={entry.event} />)
        )}
      </ul>
    </div>
  )
}

function StatusPill({ status, isPaused }: { status: string; isPaused: boolean }) {
  if (isPaused) {
    return <Pill tone="border-amber-800 bg-amber-950/40 text-amber-300">paused</Pill>
  }
  if (status === 'live') {
    return <Pill tone="border-emerald-800 bg-emerald-950/40 text-emerald-300">live</Pill>
  }
  if (status === 'connecting') {
    return <Pill tone="border-slate-700 bg-slate-800 text-slate-400">connecting…</Pill>
  }
  // EventSource reconnects on its own, so this is 'trying again', not 'gone'.
  return <Pill tone="border-red-900 bg-red-950/40 text-red-300">reconnecting…</Pill>
}

function Pill({ tone, children }: { tone: string; children: React.ReactNode }) {
  return <span className={`rounded-full border px-2 py-0.5 text-[10px] ${tone}`}>{children}</span>
}

function SmallButton({
  onClick,
  title,
  children,
}: {
  onClick: () => void
  title?: string
  children: React.ReactNode
}) {
  return (
    <button
      onClick={onClick}
      title={title}
      className="rounded-md border border-slate-700 bg-slate-800 px-2 py-1 text-xs text-slate-200 transition hover:border-slate-500"
    >
      {children}
    </button>
  )
}

/** One line of the log. Colour carries the direction so a session is readable at a glance. */
function EventLine({ event }: { event: TrafficEvent }) {
  const strAt = FormatEventTime(event.atMs)

  if (event.kind === 'lagged') {
    return (
      <li className="px-1 text-amber-400">
        <span className="text-slate-600">{strAt}</span> fell behind — {event.missed} event(s)
        missed
      </li>
    )
  }

  if (event.kind === 'lifecycle') {
    return (
      <li className="px-1 text-sky-400">
        <span className="text-slate-600">{strAt}</span> {event.what}
      </li>
    )
  }

  if (event.kind === 'frame') {
    const bIsReceived = event.direction === 'rx'
    return (
      <li className={`px-1 ${bIsReceived ? 'text-slate-400' : 'text-emerald-400/80'}`}>
        <span className="text-slate-600">{strAt}</span>{' '}
        <span className="text-slate-600">{bIsReceived ? '→' : '←'}</span>{' '}
        <span className="text-slate-500">{event.canIdHex}</span> {event.dataHex}
        {event.isFlowControl && <span className="text-slate-600"> (flow control)</span>}
      </li>
    )
  }

  return (
    <li className="px-1">
      <span className="text-slate-600">{strAt}</span>{' '}
      <span className="text-slate-500">{event.canIdHex}</span>{' '}
      <span className="text-slate-300">{event.requestHex}</span>{' '}
      <span className="text-slate-600">[{event.addressing}]</span>{' '}
      {event.responses.length > 0 ? (
        event.responses.map((response) => (
          <span
            key={response.responseCanIdHex}
            className={IsNegative(response.responseHex) ? 'text-amber-400' : 'text-emerald-400'}
          >
            {response.ecuName}: {response.responseHex}{' '}
          </span>
        ))
      ) : (
        <span className="text-slate-500">{event.reason ?? 'no answer'}</span>
      )}
    </li>
  )
}

/**
 * Keep the entries a filter matches.
 *
 * Matched against the whole line rather than one field, because the thing someone types is
 * whatever they happen to be chasing — an identifier, a service, an ECU name.
 */
function FilterEntries(
  entries: TrafficEntry[],
  filter: string,
  showFrames: boolean,
): TrafficEntry[] {
  const strNeedle = filter.trim().toUpperCase()

  return entries.filter((entry) => {
    if (!showFrames && entry.event.kind === 'frame') {
      return false
    }
    if (strNeedle.length === 0) {
      return true
    }
    return JSON.stringify(entry.event).toUpperCase().includes(strNeedle)
  })
}

/** A UDS negative response starts with 0x7F. */
function IsNegative(responseHex: string): boolean {
  return responseHex.trim().toUpperCase().startsWith('7F')
}
