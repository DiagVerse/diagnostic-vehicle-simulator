# ADR 0013 — The response editor folds away, and the log gets its own window

Status: accepted

## Context

Loading the UDS superset sample (ADR: `samples/uds-superset.simfile.json`) exposed a layout that
did not survive contact with a real file. `OverridePanel` rendered the service and sub-function
pickers *and then a row for every existing override*. Ninety-two rows pushed the exchange log so
far down the page that the thing a person actually watches was unreachable.

Two other problems sat behind it. The exchange log only ever showed requests sent from that
browser tab — a real tester talking to the simulation over the CAN bridge produced nothing on
screen at all (fixed in the engine by ADR 0012). And a log that grows without limit inside the
main tab is exactly how a busy bus makes the whole app stutter.

## Decision

**The pickers are the way in, and there is no list.** Choosing a service and a sub-function
loads whatever is already set for that request pattern, or the catalogue default if nothing is.
Applying then *replaces* the matching rule rather than appending beside it — matching on
normalised hex, so `19 04` and `1904` are the same rule. Without that, picking a sub-function
that was already set would silently create a second rule for the engine to arbitrate between.

**Applying folds the editor away.** By the time a response is set, the interesting thing is the
traffic. It collapses to a one-line summary — `92 set on UDS Reference ECU` — with the last
applied rule shown and a button back in.

It collapses rather than disappearing. Hiding it completely would leave no way back except
somewhere a user has to be told about, and a control you have to be told about is a control
most people never find.

**A rejected response keeps the editor open.** The engine explains exactly which override it
refused and why; that is precisely the moment you need the fields still in front of you.

**Timing detail is kept for the last request, not thrown away.** The live feed carries the
request, the answer and the reason, but not the per-message timings, the ResponsePending count
or the ISO 14229-2 conformance warnings — the engine computes those while executing a response
plan, and only for a request driven through the API. Replacing the old log wholesale would have
quietly deleted the feature ADR 0005 exists for. So the most recent request keeps its full
detail above the log, and the log itself is the live stream.

**The monitor is a real browser window, not a panel.** `window.open` on `#monitor`, which
renders the same component standalone. The point is the separate document: its buffer lives in
that window's memory, so a busy bus filling it cannot make the workbench stutter. The in-app
panel keeps 60 events; the window keeps 5000.

**Events are batched before they reach React.** They land in a ref and are flushed on a 250 ms
interval. Calling `setState` per event would re-render the list hundreds of times a second
during a flashing sequence and lock the tab up — which is the failure the separate window was
asked to prevent, so preventing it only in the window would be solving half of it.

**The buffer is a ring, and says so.** Once full, the oldest go. The window says how many were
dropped, and the saved log file repeats it in its header, because a file that quietly omits
part of a capture will be read as a complete one.

**Vite must not buffer the feed.** `/events` is proxied with `Accept-Encoding: identity`. An SSE
stream held back until some buffer fills is indistinguishable from an engine with nothing to
say — which is the exact confusion the monitor exists to end. This was a real bug found in the
browser, not a precaution: the feed was missing from the dev proxy entirely and the monitor sat
on "reconnecting…".

## Consequences

- One log, one source of truth: requests sent from the UI and frames from a real tester appear
  in the same stream, in the same place, because both come from the engine's feed.
- A monitor attaching later starts from that moment. The engine holds no history (ADR 0012), so
  neither does the window.
- `#monitor` also responds to a hash change, not only a fresh load, so editing the address bar
  of an open tab does something rather than appearing to do nothing.
- The old `ExchangeLog` list component is gone; `ExchangeEntryView` and `ResponseView` remain
  and now render the single most recent exchange.
