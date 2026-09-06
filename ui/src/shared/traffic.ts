import { useCallback, useEffect, useRef, useState } from 'react'

/** One CAN frame crossing the hardware bridge. */
export interface TrafficFrame {
  kind: 'frame'
  atMs: number
  /** 'rx' from the far end, 'tx' from the simulator. */
  direction: string
  canIdHex: string
  dataHex: string
  length: number
  isFlowControl: boolean
}

/** One ECU's answer inside an exchange. */
export interface TrafficExchangeResponse {
  ecuName: string
  responseCanIdHex: string
  responseHex: string
  suppressed: boolean
  /** True when one of your response overrides produced this answer, not the UDS plugin. */
  overridden: boolean
}

/** One request routed through the simulation, with whatever answered it. */
export interface TrafficExchange {
  kind: 'exchange'
  atMs: number
  canIdHex: string
  requestHex: string
  addressing: string
  routed: boolean
  responses: TrafficExchangeResponse[]
  /** Why nothing answered, when that is the interesting part. */
  reason: string | null
}

/** The simulation was loaded, started, stopped, or put on a wire. */
export interface TrafficLifecycle {
  kind: 'lifecycle'
  atMs: number
  what: string
}

/** This monitor fell behind and missed events. Shown, never hidden. */
export interface TrafficLagged {
  kind: 'lagged'
  atMs: number
  missed: number
}

/** The history the engine replayed when this monitor attached. */
export interface TrafficReplayed {
  kind: 'replayed'
  atMs: number
  count: number
  /** How many older events the engine had already dropped before we attached. */
  droppedBefore: number
}

export type TrafficEvent =
  | TrafficFrame
  | TrafficExchange
  | TrafficLifecycle
  | TrafficLagged
  | TrafficReplayed

/** One event with an id, so React can key a list that only ever grows at the front. */
export interface TrafficEntry {
  id: number
  event: TrafficEvent
}

export type FeedStatus = 'connecting' | 'live' | 'offline'

/**
 * How often the buffered events are handed to React.
 *
 * The feed can deliver hundreds of events a second during a flashing sequence. Calling
 * `setState` for each one would re-render the list hundreds of times a second and lock the tab
 * up — which is the failure this batching exists to prevent. Events land in a ref and are
 * flushed on this interval instead, so the render rate is bounded no matter what the bus does.
 */
const FLUSH_INTERVAL_MS = 250

/**
 * Subscribe to the engine's live traffic feed.
 *
 * Entries are held **oldest first**, with the newest at the end. That is how a log reads, and
 * it is also what makes scrolling survivable: appending to the end leaves everything already on
 * screen at the same offset, whereas prepending shifts the whole list under the reader's cursor
 * every time a frame arrives.
 *
 * `maxEntries` is a hard ceiling: the oldest are dropped once it is reached. A monitor left
 * open on a busy bus must not grow without bound, so the buffer is a ring and the honest
 * consequence — that you are looking at a window, not a complete record — is surfaced rather
 * than hidden.
 */
export function useTrafficFeed(maxEntries: number) {
  const [entries, setEntries] = useState<TrafficEntry[]>([])
  const [status, setStatus] = useState<FeedStatus>('connecting')
  const [isPaused, setPaused] = useState(false)
  /** Total events seen since this monitor attached, including ones dropped from the buffer. */
  const [totalSeen, setTotalSeen] = useState(0)
  /** What the engine said it replayed, so the monitor can be honest about where history starts. */
  const [replay, setReplay] = useState<TrafficReplayed | null>(null)

  const pendingRef = useRef<TrafficEntry[]>([])
  const nextIdRef = useRef(1)
  const isPausedRef = useRef(false)

  // Pausing must take effect inside the EventSource handler, which closes over its first
  // render. A ref is the state the handler can actually read.
  useEffect(() => {
    isPausedRef.current = isPaused
  }, [isPaused])

  useEffect(() => {
    const source = new EventSource('/events')

    source.onopen = () => setStatus('live')
    source.onerror = () => {
      // EventSource reconnects by itself; say so rather than implying the feed is finished.
      setStatus('offline')
    }
    source.onmessage = (message) => {
      if (isPausedRef.current) {
        return
      }
      try {
        const event = JSON.parse(message.data) as TrafficEvent
        if (event.kind === 'replayed') {
          setReplay(event)
        }
        pendingRef.current.push({ id: nextIdRef.current++, event })
      } catch {
        // A malformed line is not worth tearing the monitor down for; skip it and keep going.
      }
    }

    const flush = window.setInterval(() => {
      const pending = pendingRef.current
      if (pending.length === 0) {
        return
      }
      pendingRef.current = []
      setTotalSeen((seen) => seen + pending.length)
      // Appended at the end, and never longer than the ceiling: once full, the oldest go.
      setEntries((previous) => {
        const vecNext = [...previous, ...pending]
        return vecNext.length > maxEntries ? vecNext.slice(vecNext.length - maxEntries) : vecNext
      })
    }, FLUSH_INTERVAL_MS)

    return () => {
      window.clearInterval(flush)
      source.close()
    }
  }, [maxEntries])

  const clear = useCallback(() => {
    pendingRef.current = []
    setEntries([])
    setTotalSeen(0)
  }, [])

  return { entries, status, isPaused, setPaused, totalSeen, replay, clear }
}

/** Wall-clock time of an event, as a monitor should show it. */
export function FormatEventTime(atMs: number): string {
  const at = new Date(atMs)
  const strTime = at.toLocaleTimeString(undefined, { hour12: false })
  return `${strTime}.${String(at.getMilliseconds()).padStart(3, '0')}`
}

/** One event as a single line of text, for the saved log file. */
export function FormatEventLine(event: TrafficEvent): string {
  const strAt = FormatEventTime(event.atMs)
  switch (event.kind) {
    case 'frame':
      return `${strAt}  ${event.direction.toUpperCase()}  ${event.canIdHex.padEnd(8)} [${event.length}] ${event.dataHex}${
        event.isFlowControl ? '   (flow control)' : ''
      }`
    case 'exchange': {
      const strAnswers =
        event.responses.length > 0
          ? event.responses
              .map(
                (response) =>
                  `${response.ecuName}: ${response.responseHex}${response.overridden ? ' (override)' : ''}`,
              )
              .join(' | ')
          : (event.reason ?? 'no answer')
      return `${strAt}  --  ${event.canIdHex.padEnd(8)} ${event.requestHex}  [${event.addressing}]  ${strAnswers}`
    }
    case 'lifecycle':
      return `${strAt}  ==  ${event.what}`
    case 'lagged':
      return `${strAt}  !!  fell behind: ${event.missed} event(s) missed`
    case 'replayed':
      return event.droppedBefore > 0
        ? `${strAt}  ==  ${event.count} earlier event(s) replayed; ${event.droppedBefore} older one(s) had already been dropped by the engine`
        : `${strAt}  ==  ${event.count} earlier event(s) replayed — the session from its start`
  }
}

/**
 * Hand the collected log to the browser as a file.
 *
 * The buffer is a window, not a complete record, so the file says how many events the monitor
 * actually saw versus how many it still holds. A log that quietly omits that would be read as
 * a full capture.
 */
export function SaveTrafficLog(entries: TrafficEntry[], totalSeen: number): void {
  const vecLines = [
    `# Diagnostic Vehicle Simulator — traffic log`,
    `# saved ${new Date().toISOString()}`,
    `# ${entries.length} event(s) held, ${totalSeen} seen since this monitor attached`,
    entries.length < totalSeen
      ? `# the buffer is a window: ${totalSeen - entries.length} older event(s) were dropped`
      : `# nothing was dropped`,
    '',
    ...entries.map((entry) => FormatEventLine(entry.event)),
  ]

  const blob = new Blob([vecLines.join('\n')], { type: 'text/plain' })
  const strUrl = URL.createObjectURL(blob)
  const link = document.createElement('a')
  link.href = strUrl
  link.download = `dvsim-traffic-${new Date().toISOString().replace(/[:.]/g, '-')}.log`
  link.click()
  URL.revokeObjectURL(strUrl)
}
