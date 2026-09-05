import { useEffect, useState } from 'react'
import { api, type PluginInfo } from '../shared/api'

/** Overview: shows the plugins the engine loaded at startup. */
export function Overview() {
  const [plugins, setPlugins] = useState<PluginInfo[]>([])
  const [loaded, setLoaded] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    let cancelled = false
    api
      .plugins()
      .then((p) => {
        if (!cancelled) {
          setPlugins(p)
          setLoaded(true)
        }
      })
      .catch((e) => {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e))
      })
    return () => {
      cancelled = true
    }
  }, [])

  return (
    <section>
      <div className="mb-4 flex items-baseline justify-between">
        <h2 className="text-sm font-medium uppercase tracking-wider text-slate-400">
          Loaded plugins
        </h2>
        <span className="text-sm text-slate-500">{plugins.length} loaded</span>
      </div>

      {error && (
        <div className="rounded-lg border border-red-900/60 bg-red-950/40 px-4 py-3 text-sm text-red-300">
          Engine unreachable: {error}. Start it with{' '}
          <code className="rounded bg-slate-800 px-1.5 py-0.5">dvsim serve</code>.
        </div>
      )}

      {loaded && plugins.length === 0 && (
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
  )
}
