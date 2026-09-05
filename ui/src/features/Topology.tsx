import { useEffect, useState } from 'react'
import { Badge, type BadgeTone } from '../components/primitives'
import { api, type Topology as TopologyModel, type TopologyNode } from '../shared/api'

/** Layout constants for the diagram, in SVG user units. */
const c_nodeWidth = 168
const c_nodeHeight = 62
const c_nodeGap = 24
const c_busY = 150
const c_ecuY = 210
const c_testerY = 40
const c_margin = 24

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

  const link = topology?.links[0]
  const width = Math.max(
    vecEcus.length * (c_nodeWidth + c_nodeGap) + c_margin,
    520,
  )

  return (
    <div className="space-y-4">
      <div className="flex items-baseline justify-between">
        <h3 className="text-sm font-medium uppercase tracking-wider text-slate-400">
          {topology?.vehicleName ?? 'Vehicle'}
        </h3>
        {link && (
          <span className="flex items-center gap-2 text-xs text-slate-500">
            {link.kind}
            <Badge tone={ConfidenceTone(link.membershipConfidence)}>
              membership {link.membershipConfidence.toLowerCase()}
            </Badge>
          </span>
        )}
      </div>

      <div className="overflow-x-auto rounded-lg border border-slate-800 bg-slate-900/50 p-4">
        <svg
          width={width}
          height={c_ecuY + c_nodeHeight + c_margin}
          role="img"
          aria-label="Vehicle topology"
        >
          <BusLine width={width} label={link?.label ?? 'Diagnostic link'} />
          <TesterNode width={width} />
          {vecEcus.map((node, iIndex) => (
            <EcuNode key={node.id} node={node} index={iIndex} />
          ))}
        </svg>
      </div>

      {link && link.functionalCanIdsHex.length > 0 && (
        <p className="text-xs text-slate-500">
          Broadcast identifiers on this link:{' '}
          <span className="font-mono text-slate-400">
            {link.functionalCanIdsHex.join(', ')}
          </span>
        </p>
      )}

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

function BusLine({ width, label }: { width: number; label: string }) {
  return (
    <g>
      <line
        x1={c_margin}
        y1={c_busY}
        x2={width - c_margin}
        y2={c_busY}
        stroke="#475569"
        strokeWidth={3}
      />
      <text x={c_margin} y={c_busY - 10} className="fill-slate-500 text-[11px]">
        {label}
      </text>
    </g>
  )
}

function TesterNode({ width }: { width: number }) {
  const x = width / 2 - c_nodeWidth / 2
  return (
    <g>
      <rect
        x={x}
        y={c_testerY}
        width={c_nodeWidth}
        height={40}
        rx={8}
        className="fill-slate-800 stroke-slate-600"
        strokeDasharray="4 3"
      />
      <text
        x={x + c_nodeWidth / 2}
        y={c_testerY + 25}
        textAnchor="middle"
        className="fill-slate-300 text-[12px]"
      >
        Tester
      </text>
      <line
        x1={width / 2}
        y1={c_testerY + 40}
        x2={width / 2}
        y2={c_busY}
        stroke="#475569"
        strokeWidth={2}
      />
    </g>
  )
}

function EcuNode({ node, index }: { node: TopologyNode; index: number }) {
  const x = c_margin + index * (c_nodeWidth + c_nodeGap)
  const centreX = x + c_nodeWidth / 2

  // An inferred identifier pair is drawn dashed: a diagram that renders a derived fact the
  // same as an observed one turns an inference into a claim.
  const bIsInferred = node.addressConfidence === 'Inferred'

  return (
    <g>
      <line x1={centreX} y1={c_busY} x2={centreX} y2={c_ecuY} stroke="#475569" strokeWidth={2} />
      <rect
        x={x}
        y={c_ecuY}
        width={c_nodeWidth}
        height={c_nodeHeight}
        rx={8}
        className={`fill-slate-800 ${bIsInferred ? 'stroke-amber-700' : 'stroke-slate-600'}`}
        strokeDasharray={bIsInferred ? '5 4' : undefined}
      />
      <text
        x={centreX}
        y={c_ecuY + 24}
        textAnchor="middle"
        className="fill-slate-100 text-[13px] font-medium"
      >
        {node.label}
      </text>
      <text
        x={centreX}
        y={c_ecuY + 41}
        textAnchor="middle"
        className="fill-slate-400 font-mono text-[11px]"
      >
        {node.requestCanIdHex} → {node.responseCanIdHex}
      </text>
      <text x={centreX} y={c_ecuY + 55} textAnchor="middle" className="fill-slate-600 text-[10px]">
        {node.addressingMode} · {node.addressConfidence?.toLowerCase()}
      </text>
    </g>
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
