import { useEffect, useState } from 'react'
import { Badge, PowerSwitch, type BadgeTone } from '../components/primitives'
import { TopologyDiagram } from './TopologyDiagram'
import {
  api,
  type NewNetwork,
  type Topology as TopologyModel,
  type TopologyLink,
  type TopologyNode,
} from '../shared/api'

/** The link the engine invents when nobody has said how the ECUs are wired. */
const DERIVED_LINK_ID = 'diagnostic-link'

/**
 * A picture of how the loaded vehicle is wired — and, just as importantly, of what nobody
 * actually knows.
 *
 * A vehicle that states its architecture is drawn as one: the tester attaches to an entry-point
 * link, gateways sit on it, and the buses behind each gateway are nested under it. A vehicle
 * reconstructed from a capture states nothing, because a tester-side capture sees one connector
 * — ECUs appearing together proves they are reachable through the same connection, not that
 * they share a wire. That caveat is rendered next to the diagram, not hidden in a tooltip, and
 * the editor below turns the guess into something stated.
 */
export function Topology() {
  const [topology, setTopology] = useState<TopologyModel | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [isEditing, setEditing] = useState(false)
  const [isBusy, setBusy] = useState(false)
  // The diagram answers "how is this wired"; the list answers "what exactly is this ECU". Both
  // are worth having, and neither is a good substitute for the other.
  const [view, setView] = useState<'diagram' | 'list'>('diagram')

  /**
   * Switch one ECU on or off.
   *
   * The engine answers with the simulation state, so the topology is re-read afterwards: a
   * gateway going off changes what is reachable behind it, which is a fact about other nodes.
   */
  async function toggleEcu(node: TopologyNode) {
    if (node.requestCanIdHex === null) {
      return
    }
    setBusy(true)
    try {
      await api.simulationSetEcuEnabled(node.requestCanIdHex, !node.isEnabled)
      setTopology(await api.simulationTopology())
      setError(null)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }

  useEffect(() => {
    api
      .simulationTopology()
      .then(setTopology)
      .catch((e) => setError(e instanceof Error ? e.message : String(e)))
  }, [])

  const vecEcus = topology?.nodes.filter((node) => node.kind === 'ecu') ?? []
  if (vecEcus.length === 0) {
    return (
      <div className="space-y-4">
        {error && <ErrorBanner message={error} />}
        <p className="rounded-lg border border-slate-800 bg-slate-900/40 px-4 py-10 text-center text-sm text-slate-500">
          Nothing to draw yet. Load a CAN log, load a simulation file, or build a vehicle in the
          Simulate tab.
        </p>
      </div>
    )
  }

  const vecLinks = topology?.links ?? []
  const vecEntryPoints = vecLinks.filter((link) => link.isEntryPoint)
  const vecUnreachableLinks = vecLinks.filter((link) => link.depth === null)
  const vecUnassigned = vecEcus.filter((node) => !node.linkId)
  const nGateways = vecEcus.filter((node) => node.gatewayForLinkIds.length > 0).length
  const nOff = vecEcus.filter((node) => !node.isEnabled).length

  return (
    <div className="space-y-4">
      {error && <ErrorBanner message={error} />}

      <div className="flex flex-wrap items-baseline justify-between gap-2">
        <h3 className="text-sm font-medium uppercase tracking-wider text-slate-400">
          {topology?.vehicleName ?? 'Vehicle'}
        </h3>
        <div className="flex items-center gap-3">
          <span className="text-xs text-slate-500">
            {vecLinks.length} bus{vecLinks.length === 1 ? '' : 'es'} · {vecEcus.length} ECU
            {vecEcus.length === 1 ? '' : 's'}
            {nGateways > 0 && ` · ${nGateways} gateway${nGateways === 1 ? '' : 's'}`}
          </span>
          <div className="flex rounded-md border border-slate-700">
            {(['diagram', 'list'] as const).map((candidate) => (
              <button
                key={candidate}
                onClick={() => setView(candidate)}
                className={`px-2.5 py-1 text-xs capitalize transition first:rounded-l-md last:rounded-r-md ${
                  view === candidate
                    ? 'bg-slate-700 text-slate-100'
                    : 'text-slate-400 hover:text-slate-200'
                }`}
              >
                {candidate}
              </button>
            ))}
          </div>
          <button
            onClick={() => setEditing((wasEditing) => !wasEditing)}
            className="rounded-md border border-slate-700 bg-slate-800 px-3 py-1 text-xs text-slate-200 transition hover:border-slate-500"
          >
            {isEditing ? 'Done' : 'Edit architecture'}
          </button>
        </div>
      </div>

      {nOff > 0 && (
        <p className="rounded-lg border border-slate-700 bg-slate-900/60 px-4 py-2 text-xs text-slate-400">
          {nOff} ECU{nOff === 1 ? ' is' : 's are'} switched off. A switched-off ECU keeps its
          configuration and answers nothing at all — the same silence an unpowered or unfitted
          ECU gives a tester, not a negative response.
        </p>
      )}

      {view === 'diagram' && (
        <TopologyDiagram
          links={vecLinks}
          nodes={vecEcus}
          onToggleEcu={toggleEcu}
          busy={isBusy}
        />
      )}

      <div className={`space-y-4 overflow-x-auto ${view === 'diagram' ? 'hidden' : ''}`}>
        {vecEntryPoints.map((link) => (
          <div key={link.id} className="space-y-2">
            <TesterAttachment link={link} />
            <BusBranch
              link={link}
              links={vecLinks}
              nodes={vecEcus}
              depth={0}
              onToggleEcu={toggleEcu}
              busy={isBusy}
            />
          </div>
        ))}

        {vecUnreachableLinks.length > 0 && (
          <section className="rounded-lg border border-dashed border-amber-800/70 bg-amber-950/10 p-4">
            <p className="mb-3 text-xs text-amber-500/90">
              No chain of gateways connects these buses to anywhere a tester attaches. They are
              in the model, but nothing could talk to them.
            </p>
            <div className="space-y-3">
              {vecUnreachableLinks.map((link) => (
                <BusBranch
                  key={link.id}
                  link={link}
                  links={vecLinks}
                  nodes={vecEcus}
                  depth={0}
                  onToggleEcu={toggleEcu}
                  busy={isBusy}
                />
              ))}
            </div>
          </section>
        )}

        {vecUnassigned.length > 0 && (
          <section className="rounded-lg border border-dashed border-slate-700 bg-slate-900/30 p-4">
            <p className="mb-3 text-xs text-slate-500">
              On no declared bus — nobody has said where these sit, which is not the same as
              saying they share one.
            </p>
            <div className="flex flex-wrap gap-3">
              {vecUnassigned.map((node) => (
                <EcuCardNode key={node.id} node={node} onToggle={toggleEcu} busy={isBusy} />
              ))}
            </div>
          </section>
        )}
      </div>

      {isEditing && (
        <ArchitectureEditor
          links={vecLinks}
          ecus={vecEcus}
          onChanged={setTopology}
          onError={setError}
        />
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

function ErrorBanner({ message }: { message: string }) {
  return (
    <div className="rounded-lg border border-red-900/60 bg-red-950/40 px-4 py-3 text-sm text-red-300">
      {message}
    </div>
  )
}

/** Where the tester plugs in. Drawn above the link so the diagram reads top-down. */
function TesterAttachment({ link }: { link: TopologyLink }) {
  const strWhat = link.id === DERIVED_LINK_ID ? 'the diagnostic connector' : link.label
  return (
    <div className="flex items-center gap-2 text-xs text-slate-500">
      <span className="rounded-md border border-emerald-800 bg-emerald-950/40 px-2 py-1 text-emerald-300">
        Tester
      </span>
      <span className="text-slate-600">attaches at</span>
      <span className="text-slate-400">{strWhat}</span>
    </div>
  )
}

/**
 * One bus, the ECUs on it, and — nested inside — every bus reached through one of them.
 *
 * The nesting is the point: a flat list of buses cannot distinguish "the tester can address
 * this directly" from "every request to it goes through the gateway above".
 */
function BusBranch({
  link,
  links,
  nodes,
  depth,
  onToggleEcu,
  busy,
}: {
  link: TopologyLink
  links: TopologyLink[]
  nodes: TopologyNode[]
  depth: number
  onToggleEcu: (node: TopologyNode) => void
  busy: boolean
}) {
  const vecOnThisBus = nodes.filter((node) => node.linkId === link.id)

  // Guard against a cycle the engine would have rejected, so a bad model cannot hang the UI.
  const vecBehind =
    depth > links.length
      ? []
      : links.filter((candidate) =>
          vecOnThisBus.some((node) => node.gatewayForLinkIds.includes(candidate.id)),
        )

  return (
    <div className="rounded-lg border border-slate-800 bg-slate-900/50 p-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <span className="text-sm text-slate-300">{link.label}</span>
        <span className="flex items-center gap-2 text-xs text-slate-500">
          {link.kind}
          {link.isEntryPoint && <Badge tone="emerald">tester attaches here</Badge>}
          {link.depth !== null && link.depth > 0 && (
            <Badge tone="sky">
              {link.depth} gateway{link.depth === 1 ? '' : 's'} deep
            </Badge>
          )}
          <Badge tone={ConfidenceTone(link.membershipConfidence)}>
            membership {link.membershipConfidence.toLowerCase()}
          </Badge>
        </span>
      </div>

      <div className="mt-2 h-px w-full bg-slate-600" />

      {vecOnThisBus.length === 0 ? (
        <p className="mt-3 text-xs text-slate-600">No ECUs on this bus.</p>
      ) : (
        <div className="mt-3 flex flex-wrap gap-3">
          {vecOnThisBus.map((node) => (
            <EcuCardNode key={node.id} node={node} onToggle={onToggleEcu} busy={busy} />
          ))}
        </div>
      )}

      {link.functionalCanIdsHex.length > 0 && (
        <p className="mt-3 text-xs text-slate-500">
          Broadcast:{' '}
          <span className="font-mono text-slate-400">{link.functionalCanIdsHex.join(', ')}</span>
        </p>
      )}

      {vecBehind.length > 0 && (
        <div className="mt-4 space-y-3 border-l-2 border-slate-700 pl-4">
          {vecBehind.map((behind) => (
            <div key={behind.id} className="space-y-1.5">
              <p className="text-[11px] text-slate-500">
                behind{' '}
                <span className="text-slate-400">
                  {GatewayNameFor(behind, vecOnThisBus)}
                </span>
              </p>
              <BusBranch
                link={behind}
                links={links}
                nodes={nodes}
                depth={depth + 1}
                onToggleEcu={onToggleEcu}
                busy={busy}
              />
            </div>
          ))}
        </div>
      )}
    </div>
  )
}

/** Which ECU on this bus forwards onto the one below. */
function GatewayNameFor(behind: TopologyLink, vecOnThisBus: TopologyNode[]): string {
  const gateway = vecOnThisBus.find((node) => node.gatewayForLinkIds.includes(behind.id))
  return gateway?.label ?? 'a gateway'
}

/**
 * One ECU. An inferred identifier pair is drawn dashed: a diagram that renders a derived fact
 * the same as an observed one turns an inference into a claim.
 */
function EcuCardNode({
  node,
  onToggle,
  busy,
}: {
  node: TopologyNode
  onToggle: (node: TopologyNode) => void
  busy: boolean
}) {
  const bIsInferred = node.addressConfidence === 'Inferred'
  const bIsGateway = node.gatewayForLinkIds.length > 0
  const bIsBlocked = node.blockedByEcuName !== null

  let strBorder = 'border-slate-600'
  if (!node.isEnabled) {
    strBorder = 'border-slate-700'
  } else if (node.isUnreachable || bIsInferred || bIsBlocked) {
    strBorder = 'border-dashed border-amber-700'
  } else if (bIsGateway) {
    strBorder = 'border-sky-700'
  }

  return (
    <div
      className={`min-w-44 max-w-64 rounded-lg border bg-slate-800/60 px-3 py-2 ${strBorder} ${
        node.isEnabled ? '' : 'opacity-60'
      }`}
      title={node.unreachableReason ?? undefined}
    >
      <div className="flex items-start justify-between gap-2">
        <span className="flex items-center gap-2">
          {node.requestCanIdHex !== null && (
            <PowerSwitch
              isOn={node.isEnabled}
              disabled={busy}
              label={`Switch ${node.label} ${node.isEnabled ? 'off' : 'on'}`}
              onToggle={() => onToggle(node)}
            />
          )}
          <span className="text-sm font-medium text-slate-100">{node.label}</span>
        </span>
        {bIsGateway && <Badge tone="sky">gateway</Badge>}
      </div>

      {node.requestCanIdHex ? (
        <div className="font-mono text-[11px] text-slate-400">
          {node.requestCanIdHex} → {node.responseCanIdHex}
        </div>
      ) : (
        <div className="font-mono text-[11px] text-slate-400">
          logical {node.logicalAddressHex}
        </div>
      )}

      <div className="text-[10px] text-slate-500">
        {node.transports.join(' + ')}
        {node.addressingMode && ` · ${node.addressingMode}`}
        {node.addressConfidence && ` · ${node.addressConfidence.toLowerCase()}`}
      </div>

      {node.hopCount > 0 && (
        <div className="mt-1 text-[10px] text-slate-500">
          via {node.reachedViaEcuNames.join(' → ')}
        </div>
      )}

      {!node.isEnabled && (
        <p className="mt-1.5 text-[10px] leading-snug text-slate-500">
          Switched off — it answers nothing at all.
        </p>
      )}

      {node.isUnreachable && (
        <p className="mt-1.5 text-[10px] leading-snug text-amber-500/90">
          {node.unreachableReason}
        </p>
      )}
    </div>
  )
}

/**
 * Declare buses and say which ECU sits where.
 *
 * Available whatever the vehicle came from, which is the point: a CAN capture can never observe
 * bus membership, so a reconstructed vehicle arrives with no architecture at all. This is where
 * someone who knows the vehicle supplies it — and a from-scratch vehicle is in exactly the same
 * position.
 */
function ArchitectureEditor({
  links,
  ecus,
  onChanged,
  onError,
}: {
  links: TopologyLink[]
  ecus: TopologyNode[]
  onChanged: (topology: TopologyModel) => void
  onError: (message: string | null) => void
}) {
  const [busy, setBusy] = useState(false)

  // The derived link is not a real bus, so it must not be offered as one to assign ECUs to.
  const vecRealLinks = links.filter((link) => link.id !== DERIVED_LINK_ID)

  async function run(action: () => Promise<TopologyModel>) {
    setBusy(true)
    try {
      onChanged(await action())
      onError(null)
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <section className="space-y-4 rounded-lg border border-slate-700 bg-slate-900/60 p-4">
      <h4 className="text-sm font-medium text-slate-200">Architecture</h4>
      <p className="text-xs leading-relaxed text-slate-500">
        A CAN capture cannot see which bus an ECU is on — every answer arrives at the same
        connector. Declare the buses here and place each ECU, and the diagram above becomes
        something stated rather than inferred.
      </p>

      <NetworkForm
        busy={busy}
        onDeclare={(network) => run(() => api.simulationDeclareNetwork(network))}
      />

      {vecRealLinks.length > 0 && (
        <div className="space-y-2">
          <h5 className="text-xs font-medium uppercase tracking-wider text-slate-500">
            Declared buses
          </h5>
          <ul className="space-y-1.5">
            {vecRealLinks.map((link) => (
              <li
                key={link.id}
                className="flex items-center justify-between rounded-md border border-slate-800 bg-slate-950/40 px-3 py-2 text-xs"
              >
                <span className="text-slate-300">
                  {link.label}{' '}
                  <span className="font-mono text-slate-600">({link.id})</span>
                </span>
                <button
                  disabled={busy}
                  onClick={() => run(() => api.simulationRemoveNetwork(link.id))}
                  className="rounded border border-slate-700 px-2 py-0.5 text-slate-400 transition hover:border-red-800 hover:text-red-300 disabled:opacity-40"
                >
                  Remove
                </button>
              </li>
            ))}
          </ul>
        </div>
      )}

      {vecRealLinks.length > 0 && (
        <div className="space-y-2">
          <h5 className="text-xs font-medium uppercase tracking-wider text-slate-500">
            Placement
          </h5>
          <ul className="space-y-2">
            {ecus
              .filter((node) => node.requestCanIdHex !== null)
              .map((node) => (
                <PlacementRow
                  key={node.id}
                  node={node}
                  links={vecRealLinks}
                  busy={busy}
                  onPlace={(placement) =>
                    run(() =>
                      api.simulationSetEcuPlacement(node.requestCanIdHex ?? '', placement),
                    )
                  }
                />
              ))}
          </ul>
          {ecus.some((node) => node.requestCanIdHex === null) && (
            <p className="text-[11px] text-slate-600">
              ECUs addressed only over DoIP are placed by the simulation file that declared
              them; the engine addresses ECUs by CAN identifier, so it has no handle to move
              them by yet.
            </p>
          )}
        </div>
      )}
    </section>
  )
}

/** Declare one bus. */
function NetworkForm({
  busy,
  onDeclare,
}: {
  busy: boolean
  onDeclare: (network: NewNetwork) => void
}) {
  const [id, setId] = useState('')
  const [name, setName] = useState('')
  const [kind, setKind] = useState('CAN')
  const [bitrate, setBitrate] = useState('')
  const [entryPoint, setEntryPoint] = useState(false)

  function submit(event: React.FormEvent) {
    event.preventDefault()
    if (!id.trim() || !name.trim()) return

    onDeclare({
      id: id.trim(),
      name: name.trim(),
      kind,
      // Left blank means unknown, which is shown as unknown rather than filled in with a
      // plausible-looking 500 kbit/s.
      bitrateBps: bitrate.trim() ? Number(bitrate) : null,
      entryPoint,
    })
    setId('')
    setName('')
    setBitrate('')
    setEntryPoint(false)
  }

  return (
    <form onSubmit={submit} className="flex flex-wrap items-end gap-2">
      <Field label="Id" value={id} onChange={setId} placeholder="powertrain" width="w-32" />
      <Field
        label="Name"
        value={name}
        onChange={setName}
        placeholder="Powertrain CAN"
        width="w-44"
      />
      <label className="flex flex-col gap-1">
        <span className="text-[10px] uppercase tracking-wider text-slate-500">Kind</span>
        <select
          value={kind}
          onChange={(e) => setKind(e.target.value)}
          className="rounded-md border border-slate-700 bg-slate-950 px-2 py-1.5 text-sm text-slate-200 outline-none focus:border-slate-500"
        >
          <option value="CAN">CAN</option>
          <option value="CAN-FD">CAN-FD</option>
          <option value="Ethernet">Ethernet / DoIP</option>
        </select>
      </label>
      <Field
        label="Bit rate"
        value={bitrate}
        onChange={setBitrate}
        placeholder="500000"
        width="w-28"
      />
      <label className="flex items-center gap-1.5 pb-2 text-xs text-slate-400">
        <input
          type="checkbox"
          checked={entryPoint}
          onChange={(e) => setEntryPoint(e.target.checked)}
          className="accent-emerald-600"
        />
        Tester attaches here
      </label>
      <button
        type="submit"
        disabled={busy}
        className="rounded-md bg-emerald-700 px-3 py-1.5 text-sm text-white transition hover:bg-emerald-600 disabled:opacity-40"
      >
        Declare bus
      </button>
    </form>
  )
}

/** Say which bus one ECU is on, and which buses it gateways onto. */
function PlacementRow({
  node,
  links,
  busy,
  onPlace,
}: {
  node: TopologyNode
  links: TopologyLink[]
  busy: boolean
  onPlace: (placement: { networkId: string | null; gatewayForNetworkIds: string[] }) => void
}) {
  const strOnLink = node.linkId ?? ''

  function toggleGateway(strLinkId: string) {
    const vecNext = node.gatewayForLinkIds.includes(strLinkId)
      ? node.gatewayForLinkIds.filter((id) => id !== strLinkId)
      : [...node.gatewayForLinkIds, strLinkId]
    onPlace({ networkId: node.linkId, gatewayForNetworkIds: vecNext })
  }

  return (
    <li className="rounded-md border border-slate-800 bg-slate-950/40 px-3 py-2">
      <div className="flex flex-wrap items-center gap-2">
        <span className="min-w-36 text-sm text-slate-200">{node.label}</span>
        <span className="font-mono text-[11px] text-slate-500">{node.requestCanIdHex}</span>

        <label className="flex items-center gap-1.5 text-xs text-slate-500">
          on
          <select
            disabled={busy}
            value={strOnLink}
            onChange={(e) =>
              onPlace({
                networkId: e.target.value || null,
                gatewayForNetworkIds: node.gatewayForLinkIds,
              })
            }
            className="rounded border border-slate-700 bg-slate-950 px-2 py-1 text-slate-200 outline-none focus:border-slate-500 disabled:opacity-40"
          >
            <option value="">nobody has said</option>
            {links.map((link) => (
              <option key={link.id} value={link.id}>
                {link.label}
              </option>
            ))}
          </select>
        </label>
      </div>

      <div className="mt-1.5 flex flex-wrap items-center gap-2 text-[11px] text-slate-500">
        gateways onto:
        {links
          .filter((link) => link.id !== node.linkId)
          .map((link) => (
            <button
              key={link.id}
              disabled={busy}
              onClick={() => toggleGateway(link.id)}
              className={`rounded border px-2 py-0.5 transition disabled:opacity-40 ${
                node.gatewayForLinkIds.includes(link.id)
                  ? 'border-sky-700 bg-sky-950/50 text-sky-300'
                  : 'border-slate-700 text-slate-400 hover:border-slate-500'
              }`}
            >
              {link.label}
            </button>
          ))}
        {links.filter((link) => link.id !== node.linkId).length === 0 && (
          <span className="text-slate-600">no other bus to forward onto</span>
        )}
      </div>
    </li>
  )
}

function Field({
  label,
  value,
  onChange,
  placeholder,
  width,
}: {
  label: string
  value: string
  onChange: (value: string) => void
  placeholder: string
  width: string
}) {
  return (
    <label className="flex flex-col gap-1">
      <span className="text-[10px] uppercase tracking-wider text-slate-500">{label}</span>
      <input
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={placeholder}
        className={`${width} rounded-md border border-slate-700 bg-slate-950 px-2 py-1.5 text-sm text-slate-200 outline-none focus:border-slate-500`}
      />
    </label>
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
