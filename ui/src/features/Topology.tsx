import { useEffect, useState } from 'react'
import { Badge, type BadgeTone } from '../components/primitives'
import {
  api,
  type Topology as TopologyModel,
  type TopologyLink,
  type TopologyNode,
} from '../shared/api'


/**
 * A picture of how the loaded vehicle is wired — and, just as importantly, of what nobody
 * actually knows. A tester-side capture sees one connector, so ECUs appearing together proves
 * they are reachable through the same connection, not that they share a wire. The engine sends
 * that caveat with the data and it is rendered next to the diagram, not hidden in a tooltip.
 */
export function Topology() {
  const [topology, setTopology] = useState<TopologyModel | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    api
      .simulationTopology()
      .then(setTopology)
      .catch((e) => setError(e instanceof Error ? e.message : String(e)))
  }, [])

  if (error) {
    return (
      <div className="rounded-lg border border-red-900/60 bg-red-950/40 px-4 py-3 text-sm text-red-300">
        {error}
      </div>
    )
  }

  const vecEcus = topology?.nodes.filter((node) => node.kind === 'ecu') ?? []
  if (vecEcus.length === 0) {
    return (
      <p className="rounded-lg border border-slate-800 bg-slate-900/40 px-4 py-10 text-center text-sm text-slate-500">
        Nothing to draw yet. Load a CAN log or build a vehicle in the Simulate tab.
      </p>
    )
  }

  const vecLinks = topology?.links ?? []
  const unassigned = vecEcus.filter((node) => !node.linkId)

  return (
    <div className="space-y-4">
      <div className="flex items-baseline justify-between">
        <h3 className="text-sm font-medium uppercase tracking-wider text-slate-400">
          {topology?.vehicleName ?? 'Vehicle'}
        </h3>
        <span className="text-xs text-slate-500">
          {vecLinks.length} bus{vecLinks.length === 1 ? '' : 'es'} · {vecEcus.length} ECU
          {vecEcus.length === 1 ? '' : 's'}
        </span>
      </div>

      <div className="space-y-3 overflow-x-auto">
        {vecLinks.map((link) => (
          <BusDiagram
            key={link.id}
            link={link}
            nodes={vecEcus.filter((node) => node.linkId === link.id)}
          />
        ))}

        {unassigned.length > 0 && (
          <div className="rounded-lg border border-dashed border-slate-700 bg-slate-900/30 p-4">
            <p className="mb-3 text-xs text-slate-500">
              On no declared bus — nobody has said where these sit, which is not the same as
              saying they share one.
            </p>
            <div className="flex flex-wrap gap-3">
              {unassigned.map((node) => (
                <EcuCardNode key={node.id} node={node} />
              ))}
            </div>
          </div>
        )}
      </div>

      {topology && topology.caveats.length > 0 && (
        <ul className="space-y-1.5 rounded-lg border border-slate-800 bg-slate-900/40 px-4 py-3">
          {topology.caveats.map((caveat) => (
            <li key={caveat} className="text-xs leading-relaxed text-slate-500">
              — {caveat}
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}

/** One bus, with the ECUs on it hanging below the line. */
function BusDiagram({ link, nodes }: { link: TopologyLink; nodes: TopologyNode[] }) {
  return (
    <div className="rounded-lg border border-slate-800 bg-slate-900/50 p-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <span className="text-sm text-slate-300">{link.label}</span>
        <span className="flex items-center gap-2 text-xs text-slate-500">
          {link.kind}
          <Badge tone={ConfidenceTone(link.membershipConfidence)}>
            membership {link.membershipConfidence.toLowerCase()}
          </Badge>
        </span>
      </div>

      <div className="mt-2 h-px w-full bg-slate-600" />

      {nodes.length === 0 ? (
        <p className="mt-3 text-xs text-slate-600">No ECUs on this bus.</p>
      ) : (
        <div className="mt-3 flex flex-wrap gap-3">
          {nodes.map((node) => (
            <EcuCardNode key={node.id} node={node} />
          ))}
        </div>
      )}

      {link.functionalCanIdsHex.length > 0 && (
        <p className="mt-3 text-xs text-slate-500">
          Broadcast:{' '}
          <span className="font-mono text-slate-400">
            {link.functionalCanIdsHex.join(', ')}
          </span>
        </p>
      )}
    </div>
  )
}

/**
 * One ECU. An inferred identifier pair is drawn dashed: a diagram that renders a derived fact
 * the same as an observed one turns an inference into a claim.
 */
function EcuCardNode({ node }: { node: TopologyNode }) {
  const bIsInferred = node.addressConfidence === 'Inferred'

  return (
    <div
      className={`min-w-44 rounded-lg border bg-slate-800/60 px-3 py-2 ${
        bIsInferred ? 'border-dashed border-amber-700' : 'border-slate-600'
      }`}
    >
      <div className="text-sm font-medium text-slate-100">{node.label}</div>
      <div className="font-mono text-[11px] text-slate-400">
        {node.requestCanIdHex} → {node.responseCanIdHex}
      </div>
      <div className="text-[10px] text-slate-500">
        {node.addressingMode} · {node.addressConfidence?.toLowerCase()}
      </div>
    </div>
  )
}

function ConfidenceTone(confidence: string): BadgeTone {
  switch (confidence) {
    case 'Confirmed':
      return 'emerald'
    case 'Observed':
      return 'sky'
    case 'Inferred':
      return 'amber'
    default:
      return 'slate'
  }
}
