import { useRef, useEffect, useState } from 'react'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import type {
  ActionEntry, ActiveEvent, DebugState, HttpExchange, LogEntry,
  PausedOn, StoredEvent, SystemState, TraceEntry,
} from '@/types/api'

// ── Local types ───────────────────────────────────────────────────────────────

type SelectedItem =
  | { kind: 'trace'; entry: TraceEntry }
  | { kind: 'queue'; event: StoredEvent }
  | { kind: 'current'; active: ActiveEvent }

interface DryrunResult {
  state_before: SystemState
  state_after: SystemState
  state_changed: boolean
  actions: unknown[]
}

// ── Helpers ───────────────────────────────────────────────────────────────────

function fmt(ts: string): string {
  return new Date(ts).toLocaleTimeString()
}

function displayValue(v: unknown): string {
  if (typeof v === 'string') return v
  return JSON.stringify(v, null, 2)
}

async function api(path: string, method = 'POST', body?: unknown): Promise<Response> {
  return fetch(`/api/v1/debug${path}`, {
    method,
    headers: body != null ? { 'Content-Type': 'application/json' } : undefined,
    body: body != null ? JSON.stringify(body) : undefined,
  })
}

// ── Small components ──────────────────────────────────────────────────────────

function JsonBlock({ value }: { value: unknown }) {
  return (
    <pre className="text-[10px] font-mono whitespace-pre-wrap break-all text-muted-foreground bg-muted/20 rounded p-2">
      {JSON.stringify(value, null, 2)}
    </pre>
  )
}

function StateDiff({ before, after }: { before: SystemState; after: SystemState }) {
  const fields = ['vpn', 'qbit', 'mam', 'known_torrents'] as const
  return (
    <div className="space-y-2 font-mono text-xs">
      {fields.map(f => {
        const bStr = JSON.stringify(before[f], null, 2)
        const aStr = JSON.stringify(after[f], null, 2)
        const changed = bStr !== aStr
        return (
          <div key={f}>
            <span className={`font-semibold ${changed ? 'text-yellow-500' : 'text-muted-foreground'}`}>
              {f}:
            </span>
            {changed ? (
              <div className="ml-2 space-y-1 mt-1">
                <div className="text-red-400 bg-red-950/20 rounded p-1 line-through">{bStr}</div>
                <div className="text-green-400 bg-green-950/20 rounded p-1">{aStr}</div>
              </div>
            ) : (
              <span className="ml-2 text-muted-foreground">{JSON.stringify(before[f])}</span>
            )}
          </div>
        )
      })}
    </div>
  )
}

function HttpExchangeEntry({ x }: { x: HttpExchange }) {
  return (
    <div className="text-[10px] font-mono space-y-1 border border-muted/20 rounded p-2 bg-muted/5">
      <div className="flex items-center gap-2">
        <Badge variant="secondary" className="text-[10px] shrink-0">{x.module}</Badge>
        <span className="font-bold text-primary shrink-0">{x.method}</span>
        <span className="truncate text-muted-foreground flex-1">{x.url}</span>
        <Badge variant={x.response_status < 400 ? 'success' : 'destructive'} className="text-[10px] shrink-0">
          {x.response_status}
        </Badge>
      </div>
      {x.request_body != null && (
        <div>
          <p className="text-muted-foreground text-[9px] uppercase tracking-wider mb-0.5">Request</p>
          <pre className="whitespace-pre-wrap break-all bg-muted/20 rounded p-1">{displayValue(x.request_body)}</pre>
        </div>
      )}
      <div>
        <p className="text-muted-foreground text-[9px] uppercase tracking-wider mb-0.5">Response</p>
        <pre className="whitespace-pre-wrap break-all bg-muted/20 rounded p-1">{displayValue(x.response_body)}</pre>
      </div>
    </div>
  )
}

function ActionTimeline({ actions }: { actions: ActionEntry[] }) {
  if (actions.length === 0) return null
  return (
    <div>
      <p className="text-xs font-semibold text-muted-foreground mb-2">Actions ({actions.length})</p>
      <div className="flex flex-col gap-4">
        {actions.map(action => (
          <div key={action.id} className="border-l-2 border-muted/30 pl-3">
            <div className="flex items-center gap-2 mb-2">
              <Badge variant={action.completed_at ? 'secondary' : 'default'} className="text-[10px] shrink-0">
                {action.variant}
              </Badge>
              {action.completed_at
                ? <span className="text-[10px] text-muted-foreground">{fmt(action.completed_at)}</span>
                : <span className="text-[10px] text-yellow-500">running…</span>
              }
              {action.caused_event_id && (
                <Badge variant="outline" className="text-[10px] text-blue-400 border-blue-400/30">→ caused event</Badge>
              )}
              {action.http_exchanges.length > 0 && (
                <span className="text-[10px] text-muted-foreground ml-auto">{action.http_exchanges.length} HTTP</span>
              )}
            </div>
            <JsonBlock value={action.payload} />
            {action.http_exchanges.length > 0 && (
              <div className="mt-2 space-y-2 pl-2 border-l border-muted/20">
                {action.http_exchanges.map((x, i) => <HttpExchangeEntry key={i} x={x} />)}
              </div>
            )}
          </div>
        ))}
      </div>
    </div>
  )
}

function LogLevel({ level }: { level: string }) {
  const colours: Record<string, string> = {
    TRACE: 'text-gray-500', DEBUG: 'text-blue-400',
    INFO: 'text-green-400', WARN: 'text-yellow-400', ERROR: 'text-red-400',
  }
  return <span className={`font-bold ${colours[level] ?? 'text-muted-foreground'}`}>{level.padEnd(5)}</span>
}

// ── Timeline ──────────────────────────────────────────────────────────────────

interface TimelineProps {
  state: DebugState
  selected: SelectedItem | null
  onSelect: (item: SelectedItem) => void
  onDelete: (id: string) => void
  onMoveUp: (id: string) => void
  onMoveDown: (id: string) => void
  onEditStart: (event: StoredEvent) => void
  onInject: (position: number) => void
}

function Timeline({ state, selected, onSelect, onDelete, onMoveUp, onMoveDown, onEditStart, onInject }: TimelineProps) {
  const selId = selected?.kind === 'trace' ? selected.entry.event.id
    : selected?.kind === 'queue' ? selected.event.id
    : selected?.kind === 'current' ? selected.active.stored.id
    : null

  return (
    <div className="flex flex-col gap-1 overflow-auto h-[calc(100vh-18rem)] pr-1">
      {/* Trace: newest first */}
      {[...state.trace].reverse().map(entry => (
        <div
          key={entry.event.id}
          className={`rounded px-2 py-1 cursor-pointer text-xs flex items-center gap-2 hover:bg-muted/40 transition-colors ${selId === entry.event.id ? 'bg-muted/60 ring-1 ring-primary/40' : ''}`}
          onClick={() => onSelect({ kind: 'trace', entry })}
        >
          <span className="text-muted-foreground shrink-0 tabular-nums">{fmt(entry.event.at)}</span>
          <Badge variant="secondary" className="text-[10px] shrink-0">{entry.event.variant}</Badge>
          {entry.actions.length > 0 && (
            <span className="text-[10px] text-muted-foreground">{entry.actions.length}×</span>
          )}
          {entry.event.caused_by_action && <span className="text-[10px] text-blue-400">causal</span>}
        </div>
      ))}

      {/* Currently processing */}
      {state.current_event && (
        <div
          className={`rounded px-2 py-1 cursor-pointer text-xs flex items-center gap-2 bg-yellow-900/20 border border-yellow-600/30 ${selId === state.current_event.stored.id ? 'ring-1 ring-yellow-500/60' : ''}`}
          onClick={() => onSelect({ kind: 'current', active: state.current_event! })}
        >
          <span className="animate-pulse text-yellow-400 shrink-0">▶</span>
          <span className="text-muted-foreground shrink-0 tabular-nums">{fmt(state.current_event.stored.at)}</span>
          <Badge variant="default" className="text-[10px] shrink-0 bg-yellow-600">{state.current_event.stored.variant}</Badge>
          <span className="text-[10px] text-yellow-500">processing…</span>
        </div>
      )}

      <div className="border-t border-dashed border-muted/40 my-1" />

      {/* Queue header */}
      <div className="flex items-center justify-between px-1">
        <p className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
          Queue ({state.event_queue.length})
        </p>
        <Button size="sm" variant="outline" className="h-5 text-[10px] px-2" onClick={() => onInject(0)}>
          + Inject
        </Button>
      </div>

      {state.event_queue.length === 0 && (
        <p className="text-[10px] text-muted-foreground px-2">Queue empty</p>
      )}

      {state.event_queue.map((ev, i) => (
        <div
          key={ev.id}
          className={`rounded border border-muted/30 px-2 py-1 ${selId === ev.id ? 'bg-muted/60 ring-1 ring-primary/40' : 'hover:bg-muted/20'}`}
        >
          <div className="flex items-center gap-1 text-xs">
            {/* Reorder buttons */}
            <div className="flex flex-col mr-1">
              <button
                className="text-[9px] text-muted-foreground hover:text-foreground leading-none disabled:opacity-30"
                onClick={() => onMoveUp(ev.id)}
                disabled={i === 0}
              >▲</button>
              <button
                className="text-[9px] text-muted-foreground hover:text-foreground leading-none disabled:opacity-30"
                onClick={() => onMoveDown(ev.id)}
                disabled={i === state.event_queue.length - 1}
              >▼</button>
            </div>
            <span className="text-muted-foreground shrink-0 tabular-nums text-[10px]">{fmt(ev.at)}</span>
            <Badge
              variant="outline"
              className="text-[10px] shrink-0 cursor-pointer hover:bg-muted/40"
              onClick={() => onSelect({ kind: 'queue', event: ev })}
            >
              {ev.variant}
            </Badge>
            {ev.caused_by_action && <span className="text-[10px] text-blue-400">causal</span>}
            <div className="flex gap-1 ml-auto">
              <Button size="sm" variant="ghost" className="h-5 text-[10px] px-1" onClick={() => onEditStart(ev)}>edit</Button>
              <Button size="sm" variant="ghost" className="h-5 text-[10px] px-1 text-red-400 hover:text-red-300" onClick={() => onDelete(ev.id)}>✕</Button>
            </div>
          </div>
          <div className="flex justify-center mt-0.5">
            <button className="text-[9px] text-muted-foreground hover:text-foreground px-2" onClick={() => onInject(i + 1)}>
              + insert after
            </button>
          </div>
        </div>
      ))}
    </div>
  )
}

// ── Detail pane ───────────────────────────────────────────────────────────────

function DetailPane({
  selected,
  dryrunResult,
  onDryrun,
  onEditStart,
}: {
  selected: SelectedItem | null
  dryrunResult: DryrunResult | null
  onDryrun: (event: StoredEvent) => void
  onEditStart: (event: StoredEvent) => void
}) {
  if (!selected) {
    return (
      <div className="flex items-center justify-center h-40 text-sm text-muted-foreground border rounded-lg">
        Select an event from the timeline to inspect it
      </div>
    )
  }

  // Queue item: show payload, edit/dryrun controls, result
  if (selected.kind === 'queue') {
    const ev = selected.event
    return (
      <div className="flex flex-col gap-4">
        <div className="flex items-center gap-2">
          <h3 className="font-semibold">{ev.variant}</h3>
          <span className="text-xs text-muted-foreground">{fmt(ev.at)}</span>
          {ev.caused_by_action && <Badge variant="outline" className="text-[10px]">causal</Badge>}
          <div className="ml-auto flex gap-2">
            <Button size="sm" variant="outline" onClick={() => onEditStart(ev)}>Edit payload</Button>
            <Button size="sm" variant="outline" onClick={() => onDryrun(ev)}>Dry-run ▶</Button>
          </div>
        </div>
        <div>
          <p className="text-xs font-semibold text-muted-foreground mb-1">Payload</p>
          <JsonBlock value={ev.payload} />
        </div>
        {dryrunResult && (
          <div className="flex flex-col gap-3">
            <p className="text-xs font-semibold text-muted-foreground">
              Dry-run result —{' '}
              <span className={dryrunResult.state_changed ? 'text-yellow-500' : 'text-green-500'}>
                {dryrunResult.state_changed ? 'state changes' : 'no state change'}
              </span>
            </p>
            <div>
              <p className="text-xs font-semibold text-muted-foreground mb-1">State diff</p>
              <StateDiff before={dryrunResult.state_before} after={dryrunResult.state_after} />
            </div>
            {dryrunResult.actions.length > 0 && (
              <div>
                <p className="text-xs font-semibold text-muted-foreground mb-1">
                  Actions ({dryrunResult.actions.length})
                </p>
                {dryrunResult.actions.map((a, i) => <div key={i} className="mb-1"><JsonBlock value={a} /></div>)}
              </div>
            )}
          </div>
        )}
      </div>
    )
  }

  // Trace or current event
  const storedEvent = selected.kind === 'trace' ? selected.entry.event : selected.active.stored
  const stateBefore = selected.kind === 'trace' ? selected.entry.state_before : selected.active.state_before
  const stateAfter = selected.kind === 'trace' ? selected.entry.state_after : null
  const actions = selected.kind === 'trace' ? selected.entry.actions : selected.active.actions

  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center gap-2">
        <h3 className="font-semibold">{storedEvent.variant}</h3>
        <span className="text-xs text-muted-foreground">{fmt(storedEvent.at)}</span>
        {storedEvent.caused_by_action && <Badge variant="outline" className="text-[10px]">causal</Badge>}
        {selected.kind === 'current' && (
          <Badge variant="default" className="text-[10px] bg-yellow-600">processing</Badge>
        )}
      </div>

      <div>
        <p className="text-xs font-semibold text-muted-foreground mb-1">Payload</p>
        <JsonBlock value={storedEvent.payload} />
      </div>

      {stateAfter ? (
        <div>
          <p className="text-xs font-semibold text-muted-foreground mb-1">State diff</p>
          <StateDiff before={stateBefore} after={stateAfter} />
        </div>
      ) : (
        <div>
          <p className="text-xs font-semibold text-muted-foreground mb-1">State before</p>
          <JsonBlock value={stateBefore} />
        </div>
      )}

      {actions.length > 0 && <ActionTimeline actions={actions} />}
    </div>
  )
}

// ── Log panel ─────────────────────────────────────────────────────────────────

function LogPanel({ logs }: { logs: LogEntry[] }) {
  const containerRef = useRef<HTMLDivElement>(null)
  useEffect(() => {
    const el = containerRef.current
    if (el) el.scrollTop = el.scrollHeight
  }, [logs.length])
  return (
    <div ref={containerRef} className="h-40 overflow-auto rounded-lg border bg-muted/10 font-mono text-[10px] p-2">
      {logs.length === 0 && (
        <p className="text-muted-foreground">No logs captured — enable debug mode to see log output.</p>
      )}
      {logs.map((l, i) => (
        <div key={i} className="whitespace-pre-wrap break-all leading-relaxed">
          <span className="text-muted-foreground mr-2 tabular-nums">{fmt(l.at)}</span>
          <LogLevel level={l.level} />
          <span className="text-muted-foreground mx-1">[{l.target}]</span>
          <span>{l.message}</span>
        </div>
      ))}
    </div>
  )
}

// ── Edit modal ────────────────────────────────────────────────────────────────

function EditModal({ event, onSave, onCancel }: {
  event: StoredEvent
  onSave: (id: string, payload: unknown) => void
  onCancel: () => void
}) {
  const [value, setValue] = useState(JSON.stringify(event.payload, null, 2))
  const [error, setError] = useState('')
  function handleSave() {
    try {
      const parsed: unknown = JSON.parse(value)
      setError('')
      onSave(event.id, parsed)
    } catch {
      setError('Invalid JSON')
    }
  }
  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
      <div className="bg-background border rounded-lg p-4 w-[600px] flex flex-col gap-3">
        <div className="flex items-center justify-between">
          <h3 className="font-semibold">Edit: {event.variant}</h3>
          <Button size="sm" variant="ghost" onClick={onCancel}>✕</Button>
        </div>
        <textarea
          className="font-mono text-xs bg-muted/20 border rounded p-2 resize-none h-72"
          value={value}
          onChange={e => setValue(e.target.value)}
          spellCheck={false}
        />
        {error && <p className="text-xs text-red-400">{error}</p>}
        <div className="flex gap-2 justify-end">
          <Button variant="ghost" onClick={onCancel}>Cancel</Button>
          <Button onClick={handleSave}>Save</Button>
        </div>
      </div>
    </div>
  )
}

// ── Inject modal ──────────────────────────────────────────────────────────────

const EVENT_TEMPLATES: Record<string, unknown> = {
  Wakeup: { Wakeup: { at: '' } },
  DockerGluetunDied: { DockerGluetunDied: { at: '' } },
  DockerGluetunHealthy: { DockerGluetunHealthy: { at: '' } },
  MamRateLimitViolation: { MamRateLimitViolation: { at: '' } },
}

function InjectModal({ position, eventVariants, onInject, onCancel }: {
  position: number
  eventVariants: string[]
  onInject: (payload: unknown, position: number) => void
  onCancel: () => void
}) {
  const [variant, setVariant] = useState(eventVariants[0] ?? 'Wakeup')
  const [value, setValue] = useState(() => {
    const t = EVENT_TEMPLATES[eventVariants[0] ?? 'Wakeup'] ?? { [eventVariants[0] ?? 'Wakeup']: {} }
    return JSON.stringify({ ...t as object, at: new Date().toISOString() }, null, 2)
  })
  const [error, setError] = useState('')

  function handleVariantChange(v: string) {
    setVariant(v)
    const t = EVENT_TEMPLATES[v] ?? { [v]: {} }
    setValue(JSON.stringify(t, null, 2))
  }

  function handleInject() {
    try {
      const parsed: unknown = JSON.parse(value)
      setError('')
      onInject(parsed, position)
    } catch {
      setError('Invalid JSON')
    }
  }

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
      <div className="bg-background border rounded-lg p-4 w-[600px] flex flex-col gap-3">
        <div className="flex items-center justify-between">
          <h3 className="font-semibold">Inject event at position {position}</h3>
          <Button size="sm" variant="ghost" onClick={onCancel}>✕</Button>
        </div>
        <select
          className="bg-muted/20 border rounded p-2 text-sm"
          value={variant}
          onChange={e => handleVariantChange(e.target.value)}
        >
          {eventVariants.map(v => <option key={v} value={v}>{v}</option>)}
        </select>
        <textarea
          className="font-mono text-xs bg-muted/20 border rounded p-2 resize-none h-48"
          value={value}
          onChange={e => setValue(e.target.value)}
          spellCheck={false}
        />
        {error && <p className="text-xs text-red-400">{error}</p>}
        <div className="flex gap-2 justify-end">
          <Button variant="ghost" onClick={onCancel}>Cancel</Button>
          <Button onClick={handleInject}>Inject</Button>
        </div>
      </div>
    </div>
  )
}

// ── Breakpoints ───────────────────────────────────────────────────────────────

function BreakpointList({ title, variants, active, onToggle }: {
  title: string
  variants: string[]
  active: string[]
  onToggle: (v: string, on: boolean) => void
}) {
  const [open, setOpen] = useState(false)
  return (
    <div className="flex flex-col gap-1">
      <button
        className="flex items-center gap-2 text-xs font-semibold uppercase tracking-wider text-muted-foreground hover:text-foreground mb-1 text-left"
        onClick={() => setOpen(o => !o)}
      >
        <span>{open ? '▼' : '▶'}</span>
        <span>{title}</span>
        {active.length > 0 && (
          <Badge variant="destructive" className="text-[10px] ml-1">{active.length}</Badge>
        )}
      </button>
      {open && variants.map(v => {
        const on = active.includes(v)
        return (
          <label key={v} className="flex items-center gap-2 cursor-pointer select-none">
            <input type="checkbox" checked={on} onChange={e => onToggle(v, e.target.checked)} className="accent-primary" />
            <span className={`text-xs font-mono ${on ? 'text-yellow-500 font-semibold' : 'text-foreground'}`}>{v}</span>
          </label>
        )
      })}
    </div>
  )
}

// ── Main component ────────────────────────────────────────────────────────────

export function Debug() {
  const [debugState, setDebugState] = useState<DebugState | null>(null)
  const [connected, setConnected] = useState(false)
  const [selected, setSelected] = useState<SelectedItem | null>(null)
  const [eventVariants, setEventVariants] = useState<string[]>([])
  const [actionVariants, setActionVariants] = useState<string[]>([])
  const [dryrunResult, setDryrunResult] = useState<DryrunResult | null>(null)
  const [editingEvent, setEditingEvent] = useState<StoredEvent | null>(null)
  const [injectPosition, setInjectPosition] = useState<number | null>(null)

  // Highest seq seen — used to discard stale snapshots from either SSE or GET.
  const highestSeq = useRef(-1)

  // SSE-first connect sequence: subscribe → GET.
  // The frontend discards snapshots with seq ≤ the highest seen so far, so
  // GET responses that arrive after SSE events are silently ignored.
  useEffect(() => {
    highestSeq.current = -1
    const es = new EventSource('/api/v1/debug/stream')
    es.onopen = () => setConnected(true)
    es.onerror = () => setConnected(false)
    es.addEventListener('snapshot', (e: MessageEvent) => {
      const snap = JSON.parse(e.data as string) as DebugState
      if (snap.seq >= highestSeq.current) {
        highestSeq.current = snap.seq
        setDebugState(snap)
      }
    })
    void fetch('/api/v1/debug').then(r => r.json()).then((snap: DebugState) => {
      if (snap.seq >= highestSeq.current) {
        highestSeq.current = snap.seq
        setDebugState(snap)
      }
    })
    void fetch('/api/v1/debug/events').then(r => r.json()).then(d => setEventVariants(d as string[]))
    void fetch('/api/v1/debug/actions').then(r => r.json()).then(d => setActionVariants(d as string[]))
    return () => es.close()
  }, [])

  // Keep selected item pointing at the latest version of the same object.
  useEffect(() => {
    if (!selected || !debugState) return
    if (selected.kind === 'trace') {
      const updated = debugState.trace.find(e => e.event.id === selected.entry.event.id)
      if (updated) setSelected({ kind: 'trace', entry: updated })
    } else if (selected.kind === 'queue') {
      const updated = debugState.event_queue.find(e => e.id === selected.event.id)
      if (updated) setSelected({ kind: 'queue', event: updated })
      else setSelected(null) // deleted or moved to current
    } else if (selected.kind === 'current') {
      if (debugState.current_event) setSelected({ kind: 'current', active: debugState.current_event })
      else setSelected(null)
    }
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [debugState])

  function handleSelect(item: SelectedItem) {
    setSelected(item)
    setDryrunResult(null)
  }

  // ── API actions ───────────────────────────────────────────────────────────

  async function toggleDebugMode() {
    await api(debugState?.debug_mode ? '/disable' : '/enable')
  }
  async function step() { await api('/step') }
  async function skip() { await api('/skip') }
  async function toggleBreakpoint(kind: 'event' | 'action', variant: string, on: boolean) {
    await api(`/breakpoints/${kind}/${encodeURIComponent(variant)}`, on ? 'POST' : 'DELETE')
  }
  async function deleteQueueEvent(id: string) {
    await api(`/queue/${id}`, 'DELETE')
  }
  async function saveQueueEdit(id: string, payload: unknown) {
    await api(`/queue/${id}`, 'PUT', { payload })
    setEditingEvent(null)
  }
  async function injectEvent(payload: unknown, position: number) {
    await api('/queue', 'POST', { payload, position })
    setInjectPosition(null)
  }
  async function moveUp(id: string) {
    if (!debugState) return
    const ids = debugState.event_queue.map(e => e.id)
    const i = ids.indexOf(id)
    if (i <= 0) return
    ;[ids[i - 1], ids[i]] = [ids[i], ids[i - 1]]
    await api('/queue/order', 'PUT', { ids })
  }
  async function moveDown(id: string) {
    if (!debugState) return
    const ids = debugState.event_queue.map(e => e.id)
    const i = ids.indexOf(id)
    if (i < 0 || i >= ids.length - 1) return
    ;[ids[i], ids[i + 1]] = [ids[i + 1], ids[i]]
    await api('/queue/order', 'PUT', { ids })
  }
  async function runDryrun(event: StoredEvent) {
    setDryrunResult(null)
    const res = await api('/dryrun', 'POST', event.payload)
    if (res.ok) setDryrunResult(await res.json() as DryrunResult)
  }

  const ds = debugState
  const paused: PausedOn | null = ds?.paused_on ?? null

  return (
    <div className="flex flex-col gap-4">
      {/* Header */}
      <div className="flex items-center justify-between flex-wrap gap-3">
        <div className="flex items-center gap-3">
          <h1 className="text-2xl font-bold">Debug</h1>
          <Badge variant={connected ? 'success' : 'destructive'}>{connected ? 'Live' : 'Disconnected'}</Badge>
          {ds?.debug_mode && <Badge variant="secondary">Debug Mode</Badge>}
          {paused && (
            <Badge variant="outline" className="text-yellow-500 border-yellow-500/40">
              Paused on {paused.kind === 'Event'
                ? `event: ${paused.variant}`
                : `action: ${paused.variant} (${paused.index}/${paused.of})`}
            </Badge>
          )}
        </div>
        <div className="flex gap-2">
          {paused && (
            <>
              <Button size="sm" variant="default" onClick={step}>Step ▶</Button>
              <Button size="sm" variant="outline" onClick={skip}>Skip ⏭</Button>
            </>
          )}
          <Button size="sm" variant={ds?.debug_mode ? 'destructive' : 'default'} onClick={toggleDebugMode}>
            {ds?.debug_mode ? 'Disable Debug' : 'Enable Debug'}
          </Button>
        </div>
      </div>

      {ds ? (
        <div className="grid grid-cols-[340px_1fr] gap-4">
          {/* Left: timeline */}
          <div>
            <p className="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-2">Timeline</p>
            <Timeline
              state={ds}
              selected={selected}
              onSelect={handleSelect}
              onDelete={deleteQueueEvent}
              onMoveUp={moveUp}
              onMoveDown={moveDown}
              onEditStart={setEditingEvent}
              onInject={pos => setInjectPosition(pos)}
            />
          </div>

          {/* Right: detail + breakpoints */}
          <div className="flex flex-col gap-4">
            <div className="overflow-auto">
              <DetailPane
                selected={selected}
                dryrunResult={dryrunResult}
                onDryrun={runDryrun}
                onEditStart={setEditingEvent}
              />
            </div>
            {ds.debug_mode && (
              <div className="rounded-lg border p-3 grid grid-cols-2 gap-4">
                <BreakpointList
                  title="Event Breakpoints"
                  variants={eventVariants}
                  active={ds.event_breakpoints}
                  onToggle={(v, on) => void toggleBreakpoint('event', v, on)}
                />
                <BreakpointList
                  title="Action Breakpoints"
                  variants={actionVariants}
                  active={ds.action_breakpoints}
                  onToggle={(v, on) => void toggleBreakpoint('action', v, on)}
                />
              </div>
            )}
          </div>
        </div>
      ) : (
        <p className="text-sm text-muted-foreground">Connecting to debug stream…</p>
      )}

      {/* Log panel */}
      <div>
        <p className="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">Logs</p>
        <LogPanel logs={ds?.logs ?? []} />
      </div>

      {/* Modals */}
      {editingEvent && (
        <EditModal event={editingEvent} onSave={saveQueueEdit} onCancel={() => setEditingEvent(null)} />
      )}
      {injectPosition !== null && (
        <InjectModal
          position={injectPosition}
          eventVariants={eventVariants}
          onInject={injectEvent}
          onCancel={() => setInjectPosition(null)}
        />
      )}
    </div>
  )
}

