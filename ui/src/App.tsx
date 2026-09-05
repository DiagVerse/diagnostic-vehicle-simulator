import { useEffect, useState } from 'react'
import { StatusPill, type ConnectionStatus } from './components/StatusPill'
import { Diagnostics } from './features/Diagnostics'
import { Overview } from './features/Overview'
import { Simulate } from './features/Simulate'
import { Hardware } from './features/Hardware'
import { Topology } from './features/Topology'
import { TrafficMonitor } from './features/TrafficMonitor'
import { api, type Health } from './shared/api'

type Tab = 'simulate' | 'topology' | 'hardware' | 'diagnostics' | 'overview'

export default function App() {
  // The monitor opens as its own browser window, addressed by a hash rather than a route: it
  // needs its own document — and therefore its own memory for the traffic buffer — which is the
  // whole reason it is a separate window rather than a panel.
  //
  // A popup always arrives as a fresh load, so reading the hash once would be enough for it.
  // The listener is for the other way in: someone editing the address bar of a tab that is
  // already open, where a hash change navigates nothing and would otherwise appear to do
  // nothing at all.
  const [isMonitor, setMonitor] = useState(window.location.hash === '#monitor')

  useEffect(() => {
    const onHashChange = () => setMonitor(window.location.hash === '#monitor')
    window.addEventListener('hashchange', onHashChange)
    return () => window.removeEventListener('hashchange', onHashChange)
  }, [])

  if (isMonitor) {
    return <TrafficMonitor standalone />
  }

  return <Workbench />
}

function Workbench() {
  const [status, setStatus] = useState<ConnectionStatus>('connecting')
  const [health, setHealth] = useState<Health | null>(null)
  const [tab, setTab] = useState<Tab>('simulate')

  useEffect(() => {
    let cancelled = false
    const poll = () => {
      api
        .health()
        .then((h) => {
          if (!cancelled) {
            setHealth(h)
            setStatus('online')
          }
        })
        .catch(() => {
          if (!cancelled) setStatus('offline')
        })
    }
    poll()
    const id = setInterval(poll, 4000)
    return () => {
      cancelled = true
      clearInterval(id)
    }
  }, [])

  return (
    <div className="min-h-full bg-slate-950 text-slate-100">
      <header className="border-b border-slate-800 bg-slate-900/60 px-8 py-5 backdrop-blur">
        <div className="mx-auto flex max-w-6xl items-center justify-between">
          <div>
            <h1 className="text-lg font-semibold tracking-tight">Diagnostic Vehicle Simulator</h1>
            <p className="text-sm text-slate-400">Reconstruct · Simulate · Diagnose</p>
          </div>
          <StatusPill status={status} version={health?.engine_version} />
        </div>
        <nav className="mx-auto mt-4 flex max-w-6xl gap-1">
          <TabButton active={tab === 'simulate'} onClick={() => setTab('simulate')}>
            Simulate
          </TabButton>
          <TabButton active={tab === 'topology'} onClick={() => setTab('topology')}>
            Topology
          </TabButton>
          <TabButton active={tab === 'hardware'} onClick={() => setTab('hardware')}>
            Hardware
          </TabButton>
          <TabButton active={tab === 'diagnostics'} onClick={() => setTab('diagnostics')}>
            Diagnostics
          </TabButton>
          <TabButton active={tab === 'overview'} onClick={() => setTab('overview')}>
            Plugins
          </TabButton>
        </nav>
      </header>

      <main className="mx-auto max-w-6xl px-8 py-10">
        {tab === 'simulate' && <Simulate />}
        {tab === 'topology' && <Topology />}
        {tab === 'hardware' && <Hardware />}
        {tab === 'diagnostics' && <Diagnostics />}
        {tab === 'overview' && <Overview />}
      </main>
    </div>
  )
}

function TabButton({
  active,
  onClick,
  children,
}: {
  active: boolean
  onClick: () => void
  children: React.ReactNode
}) {
  return (
    <button
      onClick={onClick}
      className={`rounded-md px-3 py-1.5 text-sm transition ${
        active
          ? 'bg-slate-800 text-white'
          : 'text-slate-400 hover:bg-slate-800/50 hover:text-slate-200'
      }`}
    >
      {children}
    </button>
  )
}
