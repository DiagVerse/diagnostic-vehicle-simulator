export type ConnectionStatus = 'connecting' | 'online' | 'offline'

/** Small colored pill showing engine connection status in the header. */
export function StatusPill({ status, version }: { status: ConnectionStatus; version?: string }) {
  const map: Record<ConnectionStatus, { dot: string; label: string }> = {
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
