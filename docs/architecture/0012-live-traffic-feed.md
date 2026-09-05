# ADR 0012 — The live traffic feed

Status: accepted

## Context

Two things the simulator did were invisible.

Frames crossing the CAN bridge were counted and nothing else: `GET /hw/status` returned
`framesReceived` and `framesSent`, so a real tester could hold an entire diagnostic session with
the simulation and the only evidence was two numbers going up. That is the headline use case of
the hardware bridge (ADR 0007) and it could not be watched.

And an exchange driven from the browser was visible only to the tab that sent it — held in React
state, gone on reload, invisible to a second window.

There was no streaming surface of any kind in the engine: no WebSocket, no SSE, no polling
endpoint beyond `/hw/status`.

## Decision

**Server-Sent Events at `GET /events`, not a WebSocket.** The feed is one-way. SSE reconnects on
its own, survives a proxy, needs no protocol upgrade, and is a `text/event-stream` a browser's
`EventSource` handles without anybody writing reconnect logic. A WebSocket would buy
bidirectionality nothing needs.

**A bounded broadcast channel, and lag is reported rather than hidden.** The channel holds 2048
events — a couple of seconds of a busy flashing sequence. A monitor that cannot keep up must not
be allowed to slow the simulator down or make it allocate without limit: answering a tester on
time is the job, watching is a convenience. So a slow receiver gets a `lagged` event carrying the
count of what it missed. A gap you can see is debuggable; a gap you cannot is a bug hunt.

**The bridge announces through a port, and knows nothing about HTTP.** `FrameObserver` lives in
`bridge`, in the same spirit as `CanBusPort` and `ProtocolHandler`. `TrafficChannel` in `api`
implements it. The bridge must not learn about SSE, and a test observer must be as easy to attach
as the real one — both tests here do exactly that.

**Frames *and* decoded exchanges, not one or the other.** By the time the bridge dispatches a
request it has already reassembled ISO-TP and knows which ECU answered. Publishing only frames
would throw that away and ask the reader to redo it by eye. `OnExchange` is defaulted to nothing
so an observer that only wants frames stays a one-method implementation.

**Silence is described, never left blank.** "No ECU listens on that identifier", "the simulation
is stopped" and "that ECU is switched off" are indistinguishable on a wire and are three
different problems. Every routing outcome is published with its reason, including the ones that
produced no bytes. Lifecycle events (loaded, started, stopped, on the wire) go on the same
timeline, so a sudden silence has its explanation immediately above it.

**Publishing with nobody listening is not an error and is not logged.** An engine nobody is
watching is the normal case; treating it as a failure would put a line in the log for every
request.

## Verification

Driven against a running engine with a tester at the far end of a PTY pair, which is how ADR 0007
was verified:

```
frame    rx  7E0   02 10 03
exchange 7E0   10 03      physical  UDS Reference ECU: 50 03 00 32 01 F4
frame    tx  7E8   06 50 03 00 32 01 F4 AA
```

The raw request, the decoded exchange naming the ECU, and the raw answer with its padding — none
of which was observable before. Browser-driven exchanges and their silences were confirmed the
same way, including `unrouted` and `stopped` carrying their reasons.

## Consequences

- Whatever renders the feed holds its own buffer. The engine's obligation ends at the channel.
- `AppState` gained `traffic`, so every construction site (the binary and the route tests) makes
  one. It is a cheap broadcast sender.
- A monitor attaching mid-session sees the feed from that moment; there is no replay of history.
  Adding one would mean the engine holding a buffer on every client's behalf, which is the thing
  the bounded channel exists to avoid.
- `api` gained `tokio-stream` and `futures-core` for the SSE plumbing, and `can` as a dev
  dependency so the tests can build frames the way the bridge does.
