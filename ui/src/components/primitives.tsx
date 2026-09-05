/**
 * Small presentational primitives shared by the feature views. They carry no behaviour and no
 * protocol knowledge — just the project's badge and label-row styling in one place, so the
 * Diagnostics and Simulate views stay visually consistent.
 */

export type BadgeTone = 'sky' | 'emerald' | 'amber' | 'rose' | 'slate'

const TONE_CLASSES: Record<BadgeTone, string> = {
  sky: 'bg-sky-950 text-sky-300 border-sky-800',
  emerald: 'bg-emerald-950 text-emerald-300 border-emerald-800',
  amber: 'bg-amber-950 text-amber-300 border-amber-800',
  rose: 'bg-rose-950 text-rose-300 border-rose-800',
  slate: 'bg-slate-800 text-slate-300 border-slate-700',
}

/** A small rounded label, e.g. the current session or a confidence state. */
export function Badge({ tone, children }: { tone: BadgeTone; children: React.ReactNode }) {
  return (
    <span className={`rounded-full border px-2 py-0.5 text-xs ${TONE_CLASSES[tone]}`}>
      {children}
    </span>
  )
}

/** One label/value line inside a detail card. */
export function DetailRow({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex items-start justify-between gap-3">
      <dt className="text-slate-500">{label}</dt>
      <dd className="text-right">{children}</dd>
    </div>
  )
}

/**
 * An on/off switch, drawn as a slider.
 *
 * Used wherever an ECU can be taken off the air. It is deliberately a switch rather than a
 * checkbox: switching an ECU off is not selecting an option, it is unplugging something, and
 * the control should feel like it.
 */
export function PowerSwitch({
  isOn,
  disabled,
  label,
  onToggle,
}: {
  isOn: boolean
  disabled?: boolean
  label: string
  onToggle: () => void
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={isOn}
      aria-label={label}
      title={label}
      disabled={disabled}
      onClick={onToggle}
      className={`relative inline-flex h-4 w-7 shrink-0 items-center rounded-full transition disabled:opacity-40 ${
        isOn ? 'bg-emerald-600' : 'bg-slate-700'
      }`}
    >
      <span
        className={`inline-block h-3 w-3 transform rounded-full bg-white transition ${
          isOn ? 'translate-x-3.5' : 'translate-x-0.5'
        }`}
      />
    </button>
  )
}
