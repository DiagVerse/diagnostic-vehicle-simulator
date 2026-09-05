# ADR 0014 — Keeping the old messages

Status: accepted

## Context

Two ways the live feed lost what a reader wanted, reported after using it.

**Opening the monitor later showed nothing that had already happened.** ADR 0012 recorded this
as a known limit and reasoned that history would mean the engine holding a buffer on every
client's behalf. That reasoning was wrong about the shape of the problem: the buffer is not per
client, it is one bounded ring the engine keeps regardless. And the limit matters more than it
looked — a monitor is opened *because* something appeared wrong, which is necessarily after the
thing happened. A feed that starts blank always omits exactly what the person came to see.

**Scrolling back was fighting the arriving traffic.** Entries were stored newest-first and
prepended, so every new frame shifted the entire list under the reader. And the buffer had to
stay small (60 in the panel, 5000 in the window) because every entry was a DOM node — the
browser, not the data, was the limit.

## Decision

**The engine keeps a bounded history and replays it on attach.** A 20 000-event ring in
`TrafficChannel`, separate from the broadcast channel's 2048 because the two answer different
questions: the channel bounds how far a *connected* monitor may fall behind; the ring bounds how
much of the session a monitor that was not yet open can be told about.

**Snapshot and subscribe happen under one lock.** `Publish` holds the history mutex across the
`broadcast::send`, which makes "append to history" and "deliver to subscribers" one atomic step.
Without that there is a window in which an event is either missed by both the snapshot and the
receiver, or delivered by both — and a monitor that silently drops or duplicates around its own
attach point is worse than one that shows nothing. `broadcast::send` writes into a preallocated
ring and wakes waiters; it does no I/O and cannot block, so this is not a lock held across slow
work. There is a test for exactly this: replay and live feed must neither overlap nor gap.

**A partial replay says so.** A `replayed` event goes first, carrying how many events follow and
how many the engine had already dropped. A monitor opened an hour into a session must not be
left believing it is looking at the whole thing.

**Entries are stored oldest-first and appended.** That is how a log reads, and appending leaves
everything already on screen at the same offset — whereas prepending moved the list under the
reader on every frame. The prepend-and-compensate alternative exists, but getting it right
requires adjusting `scrollTop` by exactly the height of what arrived, on every batch, forever.
Appending makes the problem not exist.

**The list is virtual.** Fixed 18px rows, a full-height spacer for the scrollbar, and only the
visible slice built. Twenty thousand lines is twenty thousand DOM nodes rendered naively, which
is what forced the buffer to stay small. Rows do not wrap — long responses scroll sideways —
which is both correct for a log viewer and what keeps the row height constant enough for the
arithmetic to work.

Buffers accordingly: 20 000 in the standalone window, 2000 in the in-app panel.

**Scrolling back suspends the follow.** A log that yanks itself to the bottom on every frame is
unreadable during live traffic: the line you are reading is gone before you finish it. Scrolling
away sets a "scrolled back" state, new lines accumulate behind an `N new ↓` count, and one click
returns. Whether we are following is read from the scroll position rather than tracked
independently, so it cannot disagree with where the scrollbar actually is.

## Verification

Driven in the browser against an engine carrying continuous hardware traffic:

- 400 exchanges were generated with **no monitor attached**; attaching then delivered all 400
  plus the `replayed` summary — confirmed both over `curl` and in the UI (`805 earlier event(s)
  were replayed when this monitor attached — the session from its start`).
- Scrolled back mid-session: the `scrolled back` pill appeared, the view held its position while
  the counter climbed 806 → 831, and `25 new ↓` appeared. Clicking it returned to the tail and
  the pill cleared.
- A full ISO-TP segmented transfer stayed readable while scrolled back: the decoded exchange,
  the FirstFrame, the flow control frame, and every consecutive frame.

## Consequences

- The engine's memory grows by the history ring — a few megabytes at 20 000 events. Bounded, and
  the cost of being able to open the monitor after noticing something, which is when people do.
- `SaveTrafficLog` no longer reverses: the buffer is already in reading order.
- The row height is now load-bearing. A styling change that makes a log line taller or lets it
  wrap will misplace every row below it, so the constant and the row styling must move together.
