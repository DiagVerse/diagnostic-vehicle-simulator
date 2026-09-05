import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
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
const WINDOW_BUFFER = 20000
const PANEL_BUFFER = 2000

/**
 * Height of one log line, in pixels.
 *
 * Fixed, and matched by the row styling, because that is what makes the virtual list possible:
 * with a known row height the visible slice is arithmetic rather than measurement. Lines do not
 * wrap — they scroll sideways — which is both how a log viewer should behave and what keeps the
 * height constant.
 */
const ROW_HEIGHT = 18

/** Extra rows rendered above and below the viewport, so a fast scroll does not show gaps. */
const OVERSCAN_ROWS = 12

/** How close to the bottom still counts as "following the tail". */
const FOLLOW_THRESHOLD_PX = 40

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
  const { entries, status, isPaused, setPaused, totalSeen, replay, clear } = useTrafficFeed(
    standalone ? WINDOW_BUFFER : PANEL_BUFFER,
  )
  const [filter, setFilter] = useState('')
  const [showFrames, setShowFrames] = useState(true)

  const vecVisible = useMemo(
    () => FilterEntries(entries, filter, showFrames),
    [entries, filter, showFrames],
  )

  const { scrollRef, isFollowing, unseenCount, jumpToLatest } = useTailFollow(vecVisible.length)
  const uDropped = Math.max(0, totalSeen - entries.length)

  return (
    <div className={standalone ? 'flex h-screen flex-col bg-slate-950 p-4' : 'space-y-2'}>
      <div className="flex flex-wrap items-center gap-2">
        <h3 className="text-sm font-medium uppercase tracking-wider text-slate-400">
          Communication log
        </h3>
        <StatusPill status={status} isPaused={isPaused} />
        {!isFollowing && (
          <Pill tone="border-sky-800 bg-sky-950/40 text-sky-300">scrolled back</Pill>
        )}

        <span className="text-xs text-slate-500">
          {entries.length} held · {totalSeen} seen
          {uDropped > 0 && ` · ${uDropped} dropped`}
          {vecVisible.length !== entries.length && ` · ${vecVisible.length} shown`}
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
          This monitor is holding the most recent {entries.length} events; {uDropped} older
          one(s) have been dropped to keep memory bounded. Save the log before it wraps if you
          need them.
        </p>
      )}

      <div className={`relative ${standalone ? 'flex min-h-0 flex-1 flex-col' : ''}`}>
        <VirtualLogList
          entries={vecVisible}
          scrollRef={scrollRef}
          isEmpty={entries.length === 0}
          className={standalone ? 'flex-1' : 'h-96'}
        />

        {!isFollowing && unseenCount > 0 && (
          <button
            onClick={jumpToLatest}
            className="absolute bottom-3 left-1/2 -translate-x-1/2 rounded-full border border-sky-700 bg-sky-950/90 px-3 py-1 text-xs text-sky-200 shadow-lg transition hover:bg-sky-900"
          >
            {unseenCount} new ↓
          </button>
        )}
      </div>

      {replay && standalone && (
        <p className="text-[11px] text-slate-600">
          {replay.count} earlier event(s) were replayed when this monitor attached
          {replay.droppedBefore > 0
            ? `; ${replay.droppedBefore} older one(s) had already been dropped by the engine.`
            : ' — the session from its start.'}
        </p>
      )}
    </div>
  )
}

/**
 * The list itself, rendering only the rows that are actually on screen.
 *
 * Twenty thousand log lines is twenty thousand DOM nodes if rendered naively, which is what
 * made a large buffer impossible before: the browser, not the data, was the limit. A spacer of
 * the full height gives the scrollbar the right size and feel, and only the visible slice is
 * built — so the buffer can be as deep as memory allows without the display filling up.
 */
function VirtualLogList({
  entries,
  scrollRef,
  isEmpty,
  className,
}: {
  entries: TrafficEntry[]
  scrollRef: React.RefObject<HTMLDivElement | null>
  isEmpty: boolean
  className: string
}) {
  const [scrollTop, setScrollTop] = useState(0)
  const [viewportHeight, setViewportHeight] = useState(400)

  // The viewport height decides how many rows to build, so it has to be measured rather than
  // assumed — the standalone window is whatever size the user made it.
  useEffect(() => {
    const element = scrollRef.current
    if (!element) return

    const measure = () => setViewportHeight(element.clientHeight)
    measure()

    const observer = new ResizeObserver(measure)
    observer.observe(element)
    return () => observer.disconnect()
  }, [scrollRef])

  const uFirst = Math.max(0, Math.floor(scrollTop / ROW_HEIGHT) - OVERSCAN_ROWS)
  const uCount = Math.ceil(viewportHeight / ROW_HEIGHT) + OVERSCAN_ROWS * 2
  const vecSlice = entries.slice(uFirst, uFirst + uCount)

  return (
    <div
      ref={scrollRef}
      onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)}
      className={`overflow-auto rounded-lg border border-slate-800 bg-slate-950/60 font-mono text-[11px] ${className}`}
    >
      {entries.length === 0 ? (
        <p className="px-2 py-6 text-center font-sans text-sm text-slate-500">
          {isEmpty
            ? 'Nothing yet. Send a request, or put the simulation on a wire and let a tester talk to it.'
            : 'Nothing matches that filter.'}
        </p>
      ) : (
        <div style={{ height: entries.length * ROW_HEIGHT, position: 'relative' }}>
          <div
            style={{
              position: 'absolute',
              top: uFirst * ROW_HEIGHT,
              left: 0,
              right: 0,
            }}
          >
            {vecSlice.map((entry) => (
              <EventLine key={entry.id} event={entry.event} />
            ))}
          </div>
        </div>
      )}
    </div>
  )
}

/**
 * Keep the view pinned to the newest line — unless the reader has scrolled back to look at
 * something, in which case leave them exactly where they are.
 *
 * This is the other half of not losing old messages. A log that yanks itself to the bottom
 * every time a frame arrives is unreadable during live traffic: the line you were reading is
 * gone before you finish it. So scrolling up suspends the follow, new lines accumulate behind
 * a count, and one click returns.
 */
function useTailFollow(uTotalRows: number) {
  const scrollRef = useRef<HTMLDivElement | null>(null)
  const [isFollowing, setFollowing] = useState(true)
  const [rowsWhenLeft, setRowsWhenLeft] = useState(0)

  // Mirrors `isFollowing` for the scroll handler, which is a plain DOM listener and cannot read
  // state. Deciding the transition here rather than inside a state updater keeps the updater
  // pure — a side effect in there can be run twice by React and lose the row count.
  const isFollowingRef = useRef(true)
  const totalRowsRef = useRef(uTotalRows)

  // Kept in an effect rather than assigned during render. At most one paint stale, which for a
  // "how many arrived while you were reading" badge is not something anyone can perceive.
  useEffect(() => {
    totalRowsRef.current = uTotalRows
  }, [uTotalRows])

  useEffect(() => {
    const element = scrollRef.current
    if (!element) return

    const onScroll = () => {
      const uFromBottom = element.scrollHeight - element.scrollTop - element.clientHeight
      const bAtBottom = uFromBottom <= FOLLOW_THRESHOLD_PX
      if (bAtBottom === isFollowingRef.current) {
        return
      }

      if (!bAtBottom) {
        // Stepping away: remember the tail as it was, so what arrives after can be counted.
        setRowsWhenLeft(totalRowsRef.current)
      }
      isFollowingRef.current = bAtBottom
      setFollowing(bAtBottom)
    }

    element.addEventListener('scroll', onScroll, { passive: true })
    return () => element.removeEventListener('scroll', onScroll)
  }, [])

  // Before paint, so the view never shows the pre-scroll frame and flickers.
  useLayoutEffect(() => {
    const element = scrollRef.current
    if (!element || !isFollowing) return
    element.scrollTop = element.scrollHeight
  }, [uTotalRows, isFollowing])

  const jumpToLatest = useCallback(() => {
    const element = scrollRef.current
    if (!element) return
    element.scrollTop = element.scrollHeight
    isFollowingRef.current = true
    setFollowing(true)
  }, [])

  const unseenCount = isFollowing ? 0 : Math.max(0, uTotalRows - rowsWhenLeft)

  return { scrollRef, isFollowing, unseenCount, jumpToLatest }
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

/**
 * One line of the log. Colour carries the direction so a session is readable at a glance.
 *
 * Exactly one line high, never wrapping: the virtual list computes what is on screen from a
 * fixed row height, so a line that wrapped would put every row below it in the wrong place. A
 * long response scrolls sideways with the rest of the log instead.
 */
function EventLine({ event }: { event: TrafficEvent }) {
  const strAt = FormatEventTime(event.atMs)

  return (
    <div
      className="flex items-center gap-1 whitespace-nowrap px-2"
      style={{ height: ROW_HEIGHT, lineHeight: `${ROW_HEIGHT}px` }}
    >
      <span className="text-slate-600">{strAt}</span>
      <EventBody event={event} />
    </div>
  )
}

function EventBody({ event }: { event: TrafficEvent }) {
  switch (event.kind) {
    case 'replayed':
      return (
        <span className="text-slate-500">
          — {event.count} earlier event(s) replayed
          {event.droppedBefore > 0
            ? `; ${event.droppedBefore} older one(s) already dropped by the engine`
            : ' (the session from its start)'}{' '}
          —
        </span>
      )

    case 'lagged':
      return (
        <span className="text-amber-400">
          fell behind — {event.missed} event(s) missed
        </span>
      )

    case 'lifecycle':
      return <span className="text-sky-400">{event.what}</span>

    case 'frame': {
      const bIsReceived = event.direction === 'rx'
      return (
        <span className={bIsReceived ? 'text-slate-400' : 'text-emerald-400/80'}>
          <span className="text-slate-600">{bIsReceived ? '→' : '←'}</span>{' '}
          <span className="text-slate-500">{event.canIdHex}</span> {event.dataHex}
          {event.isFlowControl && <span className="text-slate-600"> (flow control)</span>}
        </span>
      )
    }

    case 'exchange':
      return (
        <span>
          <span className="text-slate-500">{event.canIdHex}</span>{' '}
          <span className="text-slate-300">{event.requestHex}</span>{' '}
          <span className="text-slate-600">[{event.addressing}]</span>{' '}
          {event.responses.length > 0 ? (
            event.responses.map((response) => (
              <span
                key={response.responseCanIdHex}
                className={
                  IsNegative(response.responseHex) ? 'text-amber-400' : 'text-emerald-400'
                }
              >
                {response.ecuName}: {response.responseHex}{' '}
              </span>
            ))
          ) : (
            <span className="text-slate-500">{event.reason ?? 'no answer'}</span>
          )}
        </span>
      )
  }
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
