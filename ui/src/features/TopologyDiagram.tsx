import type { TopologyLink, TopologyNode } from '../shared/api'

// Drawing constants. Named because the layout maths below is unreadable with bare numbers in
// it, and because a future change to the box size must not require finding every `40`.
const ECU_WIDTH = 146
const ECU_HEIGHT = 46
const ECU_GAP = 18
/** Space between two sub-trees hanging off the same bus, so their branches stay separable. */
const BRANCH_GAP = 34
/** Bus line down to the top of the boxes hanging off it. */
const STEM_ABOVE = 30
/** Box bottom down to the bus line a gateway forwards onto. */
const STEM_BELOW = 40
const ROW_HEIGHT = STEM_ABOVE + ECU_HEIGHT + STEM_BELOW
const MARGIN_X = 28
const TESTER_WIDTH = 76
const TESTER_HEIGHT = 44

/**
 * Colours for the buses, in the order they are first drawn.
 *
 * One colour per bus rather than per protocol: the question a reader asks of this diagram is
 * "what is on the same wire", and colouring by kind would give every CAN segment in the
 * vehicle the same colour, which is the opposite of useful.
 */
const BUS_COLOURS = [
  { line: '#38bdf8', fill: '#0c4a6e', text: '#bae6fd' }, // sky
  { line: '#fb923c', fill: '#7c2d12', text: '#fed7aa' }, // orange
  { line: '#facc15', fill: '#713f12', text: '#fef08a' }, // yellow
  { line: '#4ade80', fill: '#14532d', text: '#bbf7d0' }, // green
  { line: '#f87171', fill: '#7f1d1d', text: '#fecaca' }, // red
  { line: '#c084fc', fill: '#4c1d95', text: '#e9d5ff' }, // violet
]

interface PlacedEcu {
  node: TopologyNode
  x: number
  y: number
  colour: (typeof BUS_COLOURS)[number]
}

interface PlacedBus {
  link: TopologyLink
  x0: number
  x1: number
  y: number
  colour: (typeof BUS_COLOURS)[number]
}

interface Segment {
  x0: number
  y0: number
  x1: number
  y1: number
  colour: string
  /** Dashed for a branch that is currently not carrying anything — a gateway switched off. */
  isDead: boolean
}

/** Where the tester plugs in. One per entry-point bus. */
interface PlacedTester {
  x: number
  y: number
}

interface Layout {
  buses: PlacedBus[]
  ecus: PlacedEcu[]
  testers: PlacedTester[]
  segments: Segment[]
  width: number
  height: number
}

/**
 * A picture of the vehicle: buses as horizontal lines, ECUs hanging off them, and the buses a
 * gateway forwards onto drawn beneath it.
 *
 * The layout is a plain recursive tree walk, not a physics simulation — the same vehicle draws
 * the same way every time, which matters when someone is comparing two of them. Nothing here
 * claims to be the physical position of anything: the model records which bus an ECU is on and
 * which gateway reaches it, and that is exactly what is drawn.
 */
export function TopologyDiagram({
  links,
  nodes,
  onToggleEcu,
  busy,
}: {
  links: TopologyLink[]
  nodes: TopologyNode[]
  onToggleEcu: (node: TopologyNode) => void
  busy: boolean
}) {
  const layout = BuildLayout(links, nodes)
  if (layout.buses.length === 0) {
    return null
  }

  return (
    <div className="overflow-x-auto rounded-lg border border-slate-800 bg-slate-950/60 p-2">
      <svg
        width={layout.width}
        height={layout.height}
        viewBox={`0 0 ${layout.width} ${layout.height}`}
        role="img"
        aria-label="Vehicle topology"
        className="min-w-full"
      >
        {layout.segments.map((segment, index) => (
          <line
            key={index}
            x1={segment.x0}
            y1={segment.y0}
            x2={segment.x1}
            y2={segment.y1}
            stroke={segment.colour}
            strokeWidth={segment.isDead ? 1 : 1.5}
            strokeDasharray={segment.isDead ? '4 4' : undefined}
            opacity={segment.isDead ? 0.4 : 0.85}
          />
        ))}

        {layout.buses.map((bus) => (
          <BusLine key={bus.link.id} bus={bus} />
        ))}

        {layout.testers.map((tester, index) => (
          <g key={index}>
            <rect
              x={tester.x}
              y={tester.y}
              width={TESTER_WIDTH}
              height={26}
              rx={5}
              fill="#064e3b"
              stroke="#34d399"
              strokeWidth={1.5}
            />
            <text
              x={tester.x + TESTER_WIDTH / 2}
              y={tester.y + 17}
              fill="#a7f3d0"
              fontSize={11}
              fontWeight={600}
              textAnchor="middle"
            >
              Tester
            </text>
          </g>
        ))}

        {layout.ecus.map((placed) => (
          <EcuBox
            key={placed.node.id}
            placed={placed}
            busy={busy}
            onToggle={() => onToggleEcu(placed.node)}
          />
        ))}
      </svg>
    </div>
  )
}

/** One bus: a line, its name, and a marker where the tester attaches. */
function BusLine({ bus }: { bus: PlacedBus }) {
  return (
    <g>
      <line
        x1={bus.x0}
        y1={bus.y}
        x2={bus.x1}
        y2={bus.y}
        stroke={bus.colour.line}
        strokeWidth={3}
        strokeLinecap="round"
      />
      <text x={bus.x0} y={bus.y - 9} fill={bus.colour.text} fontSize={11} fontWeight={500}>
        {bus.link.label}
      </text>
      <text x={bus.x0} y={bus.y + 15} fill="#64748b" fontSize={9}>
        {bus.link.kind}
        {bus.link.isEntryPoint && ' · tester attaches here'}
        {bus.link.depth !== null &&
          bus.link.depth > 0 &&
          ` · ${bus.link.depth} gateway${bus.link.depth === 1 ? '' : 's'} deep`}
        {bus.link.depth === null && ' · nothing reaches this bus'}
      </text>
    </g>
  )
}

/**
 * One ECU, with its switch.
 *
 * Three states worth telling apart at a glance: on, switched off by the operator, and on but
 * cut off because a gateway in front of it is switched off. The third is the one that would
 * otherwise waste someone's afternoon.
 */
function EcuBox({
  placed,
  busy,
  onToggle,
}: {
  placed: PlacedEcu
  busy: boolean
  onToggle: () => void
}) {
  const { node, x, y, colour } = placed
  const isBlocked = node.blockedByEcuName !== null
  const isDimmed = !node.isEnabled || isBlocked

  let stroke = colour.line
  if (!node.isEnabled) {
    stroke = '#475569'
  } else if (isBlocked || !node.isSimulated) {
    stroke = '#b45309'
  }

  const strTitle = !node.isEnabled
    ? `${node.label} — switched off, so it answers nothing`
    : isBlocked
      ? `${node.label} — on, but the gateway '${node.blockedByEcuName}' in front of it is switched off`
      : (node.unreachableReason ?? `${node.label} — ${node.transports.join(' + ')}`)

  return (
    <g opacity={isDimmed ? 0.55 : 1}>
      <title>{strTitle}</title>

      <rect
        x={x}
        y={y}
        width={ECU_WIDTH}
        height={ECU_HEIGHT}
        rx={6}
        fill={node.isEnabled ? colour.fill : '#1e293b'}
        stroke={stroke}
        strokeWidth={node.gatewayForLinkIds.length > 0 ? 2 : 1}
        strokeDasharray={isDimmed || node.addressConfidence === 'Inferred' ? '5 3' : undefined}
      />

      <text x={x + 9} y={y + 17} fill="#e2e8f0" fontSize={11} fontWeight={600}>
        {Truncate(node.label, 17)}
      </text>
      <text x={x + 9} y={y + 30} fill="#94a3b8" fontSize={9} fontFamily="ui-monospace, monospace">
        {node.requestCanIdHex ?? `logical ${node.logicalAddressHex}`}
      </text>
      <text x={x + 9} y={y + 41} fill="#64748b" fontSize={8}>
        {node.gatewayForLinkIds.length > 0 ? 'gateway · ' : ''}
        {node.transports.join(' + ')}
      </text>

      {/* The switch. Only an ECU the engine can actually drive has one to flick. */}
      {node.requestCanIdHex !== null && (
        <g
          onClick={busy ? undefined : onToggle}
          style={{ cursor: busy ? 'default' : 'pointer' }}
          role="switch"
          aria-checked={node.isEnabled}
          aria-label={`Switch ${node.label} ${node.isEnabled ? 'off' : 'on'}`}
        >
          <rect
            x={x + ECU_WIDTH - 32}
            y={y + 8}
            width={24}
            height={13}
            rx={6.5}
            fill={node.isEnabled ? '#059669' : '#334155'}
          />
          <circle
            cx={x + ECU_WIDTH - (node.isEnabled ? 14 : 25)}
            cy={y + 14.5}
            r={4.5}
            fill="#ffffff"
          />
        </g>
      )}
    </g>
  )
}

/**
 * Place every bus and box.
 *
 * Depth-first: a gateway's own width is whatever its sub-tree needs, so branches never
 * overlap and the drawing needs no collision pass.
 */
function BuildLayout(links: TopologyLink[], nodes: TopologyNode[]): Layout {
  const ecus = nodes.filter((node) => node.kind === 'ecu')
  const colours = new Map<string, (typeof BUS_COLOURS)[number]>()
  links.forEach((link, index) => colours.set(link.id, BUS_COLOURS[index % BUS_COLOURS.length]))

  const layout: Layout = {
    buses: [],
    ecus: [],
    testers: [],
    segments: [],
    width: 0,
    height: 0,
  }

  // Roots are the links a tester attaches to, plus any link nothing reaches — drawing those
  // too is the whole point of knowing they are unreachable.
  const roots = links.filter((link) => link.isEntryPoint || link.depth === null)

  let cursorX = MARGIN_X
  for (const root of roots) {
    const width = PlaceBus(root, 0, cursorX, links, ecus, colours, layout, new Set())
    cursorX += width + BRANCH_GAP * 2
  }

  const maxY = layout.ecus.reduce((deepest, placed) => Math.max(deepest, placed.y), 0)
  layout.width = Math.max(cursorX - BRANCH_GAP * 2 + MARGIN_X, 320)
  layout.height = maxY + ECU_HEIGHT + MARGIN_X + TESTER_HEIGHT
  return layout
}

/**
 * Place one bus and everything hanging off it. Returns how wide it ended up.
 *
 * `visited` stops a cycle from recursing forever. The engine refuses cyclic wiring, so this is
 * a guard against a bad response rather than an expected case — but a browser tab that hangs
 * is a worse way to find out.
 */
function PlaceBus(
  link: TopologyLink,
  depth: number,
  xStart: number,
  links: TopologyLink[],
  ecus: TopologyNode[],
  colours: Map<string, (typeof BUS_COLOURS)[number]>,
  layout: Layout,
  visited: Set<string>,
): number {
  if (visited.has(link.id)) {
    return 0
  }
  visited.add(link.id)

  const colour = colours.get(link.id) ?? BUS_COLOURS[0]
  const onThisBus = ecus.filter((node) => node.linkId === link.id)
  const busY = MARGIN_X + TESTER_HEIGHT + depth * ROW_HEIGHT
  const ecuY = busY + STEM_ABOVE

  let cursorX = xStart
  for (const node of onThisBus) {
    const subLinks = links.filter((candidate) => node.gatewayForLinkIds.includes(candidate.id))

    // Lay the sub-tree out first: the gateway is then centred over whatever it needs.
    let subCursorX = cursorX
    const subSpans: { link: TopologyLink; x0: number; width: number }[] = []
    for (const subLink of subLinks) {
      const width = PlaceBus(subLink, depth + 1, subCursorX, links, ecus, colours, layout, visited)
      if (width > 0) {
        subSpans.push({ link: subLink, x0: subCursorX, width })
        subCursorX += width + BRANCH_GAP
      }
    }

    const subWidth = subSpans.length > 0 ? subCursorX - BRANCH_GAP - cursorX : 0
    const slotWidth = Math.max(ECU_WIDTH, subWidth)
    const ecuX = cursorX + (slotWidth - ECU_WIDTH) / 2
    const ecuCentreX = ecuX + ECU_WIDTH / 2

    layout.ecus.push({ node, x: ecuX, y: ecuY, colour })

    // The stem up to the bus this ECU sits on.
    layout.segments.push({
      x0: ecuCentreX,
      y0: busY,
      x1: ecuCentreX,
      y1: ecuY,
      colour: colour.line,
      isDead: !node.isEnabled,
    })

    // And down to each bus it forwards onto. Dashed when it is switched off, because then
    // nothing is travelling along it.
    for (const span of subSpans) {
      const subColour = colours.get(span.link.id) ?? BUS_COLOURS[0]
      const subBusY = busY + ROW_HEIGHT
      const subCentreX = span.x0 + span.width / 2
      const elbowY = ecuY + ECU_HEIGHT + STEM_BELOW / 2

      layout.segments.push({
        x0: ecuCentreX,
        y0: ecuY + ECU_HEIGHT,
        x1: ecuCentreX,
        y1: elbowY,
        colour: subColour.line,
        isDead: !node.isEnabled,
      })
      layout.segments.push({
        x0: ecuCentreX,
        y0: elbowY,
        x1: subCentreX,
        y1: elbowY,
        colour: subColour.line,
        isDead: !node.isEnabled,
      })
      layout.segments.push({
        x0: subCentreX,
        y0: elbowY,
        x1: subCentreX,
        y1: subBusY,
        colour: subColour.line,
        isDead: !node.isEnabled,
      })
    }

    cursorX += slotWidth + ECU_GAP
  }

  const width = Math.max(cursorX - ECU_GAP - xStart, ECU_WIDTH)
  layout.buses.push({ link, x0: xStart, x1: xStart + width, y: busY, colour })

  // The tester, drawn above the link it plugs into. Placed to the right of the bus label so
  // the two do not collide on a narrow bus.
  if (link.isEntryPoint) {
    const testerX = xStart + width - TESTER_WIDTH
    layout.testers.push({ x: testerX, y: busY - TESTER_HEIGHT - 4 })
    layout.segments.push({
      x0: testerX + TESTER_WIDTH / 2,
      y0: busY - 4,
      x1: testerX + TESTER_WIDTH / 2,
      y1: busY,
      colour: '#34d399',
      isDead: false,
    })
  }

  return width
}

function Truncate(strText: string, uMaxLength: number): string {
  if (strText.length <= uMaxLength) {
    return strText
  }
  return `${strText.slice(0, uMaxLength - 1)}…`
}
