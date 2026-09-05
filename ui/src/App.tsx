import { useEffect, useState } from 'react'
import { StatusPill, type ConnectionStatus } from './components/StatusPill'
import { Diagnostics } from './features/Diagnostics'
import { Overview } from './features/Overview'
import { api, type Health } from './shared/api'

type Tab = 'diagnostics' | 'overview'

export default function App() {
  const [status, setStatus] = useState<ConnectionStatus>('connecting')
  const [health, setHealth] = useState<Health | null>(null)
  const [tab, setTab] = useState<Tab>('diagnostics')

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
        <div className="mx-auto flex max-w-5xl items-center justify-between">
          <div>
            <h1 className="text-lg font-semibold tracking-tight">Diagnostic Vehicle Simulator</h1>
            <p className="text-sm text-slate-400">Reconstruct · Simulate · Diagnose</p>
          </div>
          <StatusPill status={status} version={health?.engine_version} />
        </div>
        <nav className="mx-auto mt-4 flex max-w-5xl gap-1">
          <TabButton active={tab === 'diagnostics'} onClick={() => setTab('diagnostics')}>
            Diagnostics
          </TabButton>
          <TabButton active={tab === 'overview'} onClick={() => setTab('overview')}>
            Plugins
          </TabButton>
        </nav>
      </header>

      <main className="mx-auto max-w-5xl px-8 py-10">
        {tab === 'diagnostics' ? <Diagnostics /> : <Overview />}
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
