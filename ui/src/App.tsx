import { useEffect, useState } from 'react'
import { api, type Health, type PluginInfo } from './shared/api'

// Phase 0 shell: confirm the UI can reach the engine and render the plugins it loaded.
// Real feature views (topology, ECU inspector, live trace, terminals) arrive in later phases.

type Status = 'connecting' | 'online' | 'offline'

export default function App() {
  const [status, setStatus] = useState<Status>('connecting')
  const [health, setHealth] = useState<Health | null>(null)
  const [plugins, setPlugins] = useState<PluginInfo[]>([])
  const [error, setError] = useState<string | null>(null)

  async function refresh() {
    try {
      const [h, p] = await Promise.all([api.health(), api.plugins()])
      setHealth(h)
      setPlugins(p)
      setStatus('online')
      setError(null)
    } catch (e) {
      setStatus('offline')
      setError(e instanceof Error ? e.message : String(e))
    }
  }

  useEffect(() => {
    refresh()
    const id = setInterval(refresh, 4000)
    return () => clearInterval(id)
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
      </header>

      <main className="mx-auto max-w-5xl px-8 py-10">
        <section>
          <div className="mb-4 flex items-baseline justify-between">
            <h2 className="text-sm font-medium uppercase tracking-wider text-slate-400">
              Loaded plugins
            </h2>
            <span className="text-sm text-slate-500">{plugins.length} loaded</span>
          </div>

          {status === 'offline' && (
            <div className="rounded-lg border border-red-900/60 bg-red-950/40 px-4 py-3 text-sm text-red-300">
              Engine unreachable{error ? `: ${error}` : ''}. Start it with{' '}
              <code className="rounded bg-slate-800 px-1.5 py-0.5">dvsim serve</code>.
            </div>
          )}

          {status === 'online' && plugins.length === 0 && (
            <div className="rounded-lg border border-slate-800 bg-slate-900/40 px-4 py-8 text-center text-sm text-slate-400">
              No plugins loaded. Drop a compiled plugin into{' '}
              <code className="rounded bg-slate-800 px-1.5 py-0.5">plugins.d/</code> and restart.
            </div>
          )}

          <ul className="grid gap-3 sm:grid-cols-2">
            {plugins.map((p) => (
              <li
                key={p.name}
                className="rounded-lg border border-slate-800 bg-slate-900/50 p-4 transition hover:border-slate-700"
              >
                <div className="flex items-center justify-between">
                  <span className="font-medium">{p.name}</span>
                  <span className="rounded-full bg-slate-800 px-2 py-0.5 text-xs uppercase tracking-wide text-slate-300">
                    {p.kind}
                  </span>
                </div>
                <p className="mt-2 text-sm text-slate-400">{p.description}</p>
                <p className="mt-3 font-mono text-xs text-slate-600">v{p.version}</p>
              </li>
            ))}
          </ul>
        </section>
      </main>
    </div>
  )
}

function StatusPill({ status, version }: { status: Status; version?: string }) {
  const map: Record<Status, { dot: string; label: string }> = {
    connecting: { dot: 'bg-amber-400', label: 'Connecting…' },
    online: { dot: 'bg-emerald-400', label: version ? `Engine v${version}` : 'Online' },
    offline: { dot: 'bg-red-500', label: 'Offline' },
  }
  const s = map[status]
  return (
    <div className="flex items-center gap-2 rounded-full border border-slate-800 bg-slate-900 px-3 py-1.5 text-sm">
      <span className={`h-2 w-2 rounded-full ${s.dot}`} />
      <span className="text-slate-300">{s.label}</span>
    </div>
  )
}
