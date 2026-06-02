import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import {
  ALL_CORES,
  Breakpoint,
  CoreCounters,
  CoreId,
  CoreStatus,
  LossCounters,
  SseMessage,
  StoredHttpExchange,
  StoredLogLine,
  StoredStepRecord,
} from '@/types/observability'

// ── Helpers ───────────────────────────────────────────────────────────────────

function fmtTime(ts: string): string {
  const d = new Date(ts)
  return d.toLocaleTimeString('en-GB', { hour12: false }) + '.' + String(d.getMilliseconds()).padStart(3, '0')
}

function fmtDuration(ms: number): string {
  if (ms < 1) return '<1ms'
  if (ms < 1000) return `${ms}ms`
  return `${(ms / 1000).toFixed(2)}s`
}

function statusLabel(s: CoreStatus): string {
  switch (s.state) {
    case 'running':
      return '▶ running'
    case 'pause_requested':
      return '‖ pause requested'
    case 'parked_at_event':
      return `‖ park @ event ${s.variant}`
    case 'parked_at_outcome':
      return `‖ park @ outcome ${s.source_variant}`
    case 'parked_at_http':
      return `‖ park @ ${s.method} ${truncate(s.url, 32)}`
    case 'stepping':
      return '▷ stepping'
  }
}

function truncate(s: string, n: number): string {
  return s.length > n ? s.slice(0, n - 1) + '…' : s
}

function isPaused(s: CoreStatus): boolean {
  return s.state !== 'running' && s.state !== 'stepping'
}

// ── Cores rail ────────────────────────────────────────────────────────────────

function CoresRail({
  selected,
  statuses,
  onSelect,
  loss,
}: {
  selected: CoreId
  statuses: Map<CoreId, CoreStatus>
  onSelect: (c: CoreId) => void
  loss: LossCounters
}) {
  const post = useCallback((path: string) => {
    void fetch(`/api/v1/observability/${path}`, { method: 'POST' })
  }, [])
  return (
    <div className="flex flex-col gap-2 text-sm">
      <div className="flex gap-2">
        <Button size="sm" variant="outline" onClick={() => post('pause_all')}>Pause All</Button>
        <Button size="sm" variant="outline" onClick={() => post('step_all')}>Step All</Button>
      </div>
      <div className="flex flex-col gap-1 border-t pt-2">
        {ALL_CORES.map(core => {
          const status = statuses.get(core) ?? { state: 'running' as const }
          const paused = isPaused(status)
          const counters: CoreCounters | undefined = loss.per_core[core]
          return (
            <div
              key={core}
              className={`rounded p-2 cursor-pointer ${selected === core ? 'bg-muted' : 'hover:bg-muted/50'}`}
              onClick={() => onSelect(core)}
            >
              <div className="flex items-center justify-between">
                <span className="font-mono text-xs font-semibold uppercase">{core}</span>
                <span className={`text-xs ${paused ? 'text-yellow-400' : 'text-muted-foreground'}`}>
                  {statusLabel(status)}
                </span>
              </div>
              {counters && (counters.dropped_steps + counters.truncated_bodies + counters.reservation_failures > 0) && (
                <div className="text-[10px] text-muted-foreground mt-1">
                  loss: {counters.dropped_steps} dropped, {counters.truncated_bodies} truncated
                </div>
              )}
            </div>
          )
        })}
      </div>
      <div className="flex gap-2 border-t pt-2">
        <Button
          size="sm"
          variant={statuses.get(selected) && isPaused(statuses.get(selected)!) ? 'outline' : 'default'}
          onClick={() =>
            post(statuses.get(selected) && isPaused(statuses.get(selected)!) ? `resume/${selected}` : `pause/${selected}`)
          }
        >
          {statuses.get(selected) && isPaused(statuses.get(selected)!) ? 'Resume' : 'Pause'} {selected.toUpperCase()}
        </Button>
        <Button size="sm" variant="outline" onClick={() => post(`step/${selected}`)}>
          Step {selected.toUpperCase()}
        </Button>
      </div>
    </div>
  )
}

// ── StepRecord row ────────────────────────────────────────────────────────────

function StepRow({ step, prevState }: { step: StoredStepRecord; prevState: unknown }) {
  const [expanded, setExpanded] = useState(false)
  const stateDelta = useMemo(() => diffJson(prevState, step.state_after), [prevState, step.state_after])
  return (
    <div className="border-b py-2 text-xs cursor-pointer hover:bg-muted/30" onClick={() => setExpanded(!expanded)}>
      <div className="flex items-center gap-2">
        <span className="text-muted-foreground tabular-nums shrink-0">{fmtTime(step.recorded_at)}</span>
        <span className="font-semibold">{step.event_variant}</span>
        <span className="text-muted-foreground">{fmtDuration(step.duration_ms)}</span>
        <span className="text-muted-foreground">
          a:{step.actions.length} p:{step.publishes.length}
        </span>
        <span className="text-muted-foreground truncate">{stateDelta || 'no change'}</span>
        <span className="ml-auto text-muted-foreground">{expanded ? '▼' : '▶'}</span>
      </div>
      {expanded && (
        <div className="mt-2 pl-4 space-y-2">
          <Section title="cause">
            <pre className="text-[10px]">{JSON.stringify(step.event_cause, null, 2)}</pre>
          </Section>
          <Section title="event">
            <pre className="text-[10px] overflow-auto max-h-48">{JSON.stringify(step.event, null, 2)}</pre>
          </Section>
          {step.actions.length > 0 && (
            <Section title={`actions (${step.actions.length})`}>
              {step.actions.map(a => (
                <div key={a.action_id} className="border-l-2 border-blue-500/40 pl-2 mb-1">
                  <div className="text-blue-300">{a.variant}</div>
                  <pre className="text-[10px]">{JSON.stringify(a.payload, null, 2)}</pre>
                </div>
              ))}
            </Section>
          )}
          {step.publishes.length > 0 && (
            <Section title={`publishes (${step.publishes.length})`}>
              {step.publishes.map(p => (
                <div key={p.publish_id} className="border-l-2 border-green-500/40 pl-2 mb-1">
                  <div className="text-green-300">{p.topic}: {p.variant}</div>
                  <pre className="text-[10px]">{JSON.stringify(p.payload, null, 2)}</pre>
                </div>
              ))}
            </Section>
          )}
          <Section title="state after">
            <pre className="text-[10px] overflow-auto max-h-48">{JSON.stringify(step.state_after, null, 2)}</pre>
          </Section>
        </div>
      )}
    </div>
  )
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div>
      <div className="text-[10px] uppercase tracking-wider text-muted-foreground mb-1">{title}</div>
      {children}
    </div>
  )
}

// Cheap structural diff — returns a one-line summary of which leaf keys changed.
function diffJson(a: unknown, b: unknown, path = ''): string {
  if (a === b) return ''
  if (a === undefined || b === undefined || a === null || b === null || typeof a !== typeof b) {
    return `${path || '<root>'}: ${jsonShort(a)} → ${jsonShort(b)}`
  }
  if (typeof a !== 'object') {
    return JSON.stringify(a) === JSON.stringify(b) ? '' : `${path || '<root>'}: ${jsonShort(a)} → ${jsonShort(b)}`
  }
  const ao = a as Record<string, unknown>
  const bo = b as Record<string, unknown>
  const keys = new Set([...Object.keys(ao), ...Object.keys(bo)])
  const parts: string[] = []
  for (const k of keys) {
    const sub = diffJson(ao[k], bo[k], path ? `${path}.${k}` : k)
    if (sub) parts.push(sub)
  }
  return parts.join(', ')
}

function jsonShort(v: unknown): string {
  if (v === undefined) return 'undef'
  const s = JSON.stringify(v)
  return s.length > 24 ? s.slice(0, 23) + '…' : s
}

// ── Bottom panel: HTTP / Logs ─────────────────────────────────────────────────

function BottomPanel({
  http,
  logs,
}: {
  http: StoredHttpExchange[]
  logs: StoredLogLine[]
}) {
  const [tab, setTab] = useState<'http' | 'logs'>('http')
  return (
    <div className="border-t bg-background">
      <div className="flex gap-2 border-b px-2 py-1">
        <Button size="sm" variant={tab === 'http' ? 'default' : 'ghost'} onClick={() => setTab('http')}>
          HTTP ({http.length})
        </Button>
        <Button size="sm" variant={tab === 'logs' ? 'default' : 'ghost'} onClick={() => setTab('logs')}>
          Logs ({logs.length})
        </Button>
      </div>
      <div className="h-48 overflow-auto text-xs">
        {tab === 'http' && (
          <div className="font-mono">
            {http.slice(-200).reverse().map(x => (
              <div key={x.exchange_id} className="flex gap-2 px-2 py-1 border-b">
                <span className="text-muted-foreground tabular-nums">{fmtTime(x.at)}</span>
                <span className="text-muted-foreground">{x.core.toUpperCase()}</span>
                <span className="font-semibold">{x.method}</span>
                <span className="truncate max-w-[600px]">{x.url}</span>
                <span className={x.response_status < 400 ? 'text-green-400' : 'text-red-400'}>{x.response_status}</span>
                <span className="text-muted-foreground ml-auto">{fmtDuration(x.duration_ms)}</span>
              </div>
            ))}
          </div>
        )}
        {tab === 'logs' && (
          <div className="font-mono">
            {logs.slice(-200).reverse().map((l, i) => (
              <div key={i} className="flex gap-2 px-2 py-1 border-b">
                <span className="text-muted-foreground tabular-nums">{fmtTime(l.at)}</span>
                <span className={
                  l.level === 'ERROR' ? 'text-red-400' :
                  l.level === 'WARN' ? 'text-yellow-400' :
                  l.level === 'INFO' ? 'text-blue-400' : 'text-muted-foreground'
                }>{l.level}</span>
                <span className="text-muted-foreground">{l.target}</span>
                <span className="truncate">{l.message}</span>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  )
}

// ── Breakpoint drawer ─────────────────────────────────────────────────────────

function BreakpointDrawer({
  breakpoints,
  onClose,
  onChange,
}: {
  breakpoints: Breakpoint[]
  onClose: () => void
  onChange: () => void
}) {
  const [kind, setKind] = useState<'event' | 'action' | 'publish' | 'http'>('event')
  const [value, setValue] = useState('')

  const add = useCallback(async () => {
    if (!value.trim()) return
    await fetch(`/api/v1/observability/breakpoints/${kind}/${encodeURIComponent(value.trim())}`, {
      method: 'POST',
    })
    setValue('')
    onChange()
  }, [kind, value, onChange])

  const remove = useCallback(async (bp: Breakpoint) => {
    let v: string
    let k: string
    switch (bp.kind) {
      case 'event_variant':
        k = 'event'; v = bp.variant; break
      case 'action_variant':
        k = 'action'; v = bp.variant; break
      case 'publish_variant':
        k = 'publish'; v = bp.variant; break
      case 'http_url_pattern':
        k = 'http'; v = bp.pattern; break
    }
    await fetch(`/api/v1/observability/breakpoints/${k}/${encodeURIComponent(v)}`, {
      method: 'DELETE',
    })
    onChange()
  }, [onChange])

  return (
    <div className="fixed top-0 right-0 h-full w-80 bg-background border-l z-50 p-4 flex flex-col gap-3">
      <div className="flex items-center justify-between">
        <h3 className="font-semibold">Breakpoints</h3>
        <Button size="sm" variant="ghost" onClick={onClose}>×</Button>
      </div>
      <div className="flex gap-1">
        <select
          className="text-xs bg-background border rounded px-2"
          value={kind}
          onChange={e => setKind(e.target.value as typeof kind)}
        >
          <option value="event">event</option>
          <option value="action">action</option>
          <option value="publish">publish</option>
          <option value="http">http url</option>
        </select>
        <input
          className="flex-1 text-xs bg-background border rounded px-2"
          placeholder="variant or URL substring"
          value={value}
          onChange={e => setValue(e.target.value)}
          onKeyDown={e => e.key === 'Enter' && void add()}
        />
        <Button size="sm" onClick={() => void add()}>Add</Button>
      </div>
      <div className="flex-1 overflow-auto text-xs">
        {breakpoints.length === 0 && <div className="text-muted-foreground">no breakpoints</div>}
        {breakpoints.map((bp, i) => (
          <div key={i} className="flex items-center justify-between border-b py-1">
            <div>
              <span className="text-muted-foreground mr-2 text-[10px] uppercase">
                {bp.kind.replace(/_/g, ' ').replace(' variant', '').replace(' pattern', '')}
              </span>
              <span className="font-mono">
                {bp.kind === 'http_url_pattern' ? bp.pattern : bp.variant}
              </span>
            </div>
            <Button size="sm" variant="ghost" onClick={() => void remove(bp)}>×</Button>
          </div>
        ))}
      </div>
    </div>
  )
}

// ── Main page ────────────────────────────────────────────────────────────────

export function Observability() {
  const [connected, setConnected] = useState(false)
  const [statuses, setStatuses] = useState<Map<CoreId, CoreStatus>>(new Map())
  const [stepsByCore, setStepsByCore] = useState<Map<CoreId, StoredStepRecord[]>>(new Map())
  const [http, setHttp] = useState<StoredHttpExchange[]>([])
  const [logs, setLogs] = useState<StoredLogLine[]>([])
  const [loss, setLoss] = useState<LossCounters>({ per_core: {}, http: { dropped_exchanges: 0, truncated_request_bodies: 0, truncated_response_bodies: 0 } })
  const [breakpoints, setBreakpoints] = useState<Breakpoint[]>([])
  const [selected, setSelected] = useState<CoreId>('mam')
  const [drawerOpen, setDrawerOpen] = useState(false)
  const esRef = useRef<EventSource | null>(null)

  const refreshBreakpoints = useCallback(async () => {
    const res = await fetch('/api/v1/observability/breakpoints')
    if (res.ok) setBreakpoints(await res.json())
  }, [])

  useEffect(() => {
    const es = new EventSource('/api/v1/observability/stream')
    esRef.current = es
    es.onopen = () => setConnected(true)
    es.onerror = () => setConnected(false)
    es.addEventListener('observability', e => {
      const msg = JSON.parse((e as MessageEvent).data) as SseMessage
      apply(msg)
    })
    return () => { es.close() }

    function apply(msg: SseMessage) {
      switch (msg.kind) {
        case 'hello': {
          const sm = new Map<CoreId, CoreStatus>()
          for (const [c, s] of msg.data.cores) sm.set(c, s)
          setStatuses(sm)
          const grouped = new Map<CoreId, StoredStepRecord[]>()
          for (const step of msg.data.steps) {
            const arr = grouped.get(step.core) ?? []
            arr.push(step)
            grouped.set(step.core, arr)
          }
          setStepsByCore(grouped)
          setHttp(msg.data.http)
          setLogs(msg.data.logs)
          setLoss(msg.data.loss)
          setBreakpoints(msg.data.active_breakpoints)
          break
        }
        case 'step': {
          const step = msg.data
          setStepsByCore(prev => {
            const next = new Map(prev)
            const arr = [...(next.get(step.core) ?? []), step]
            if (arr.length > 500) arr.shift()
            next.set(step.core, arr)
            return next
          })
          break
        }
        case 'http_exchange': {
          const ex = msg.data
          setHttp(prev => {
            const next = [...prev, ex]
            if (next.length > 500) next.shift()
            return next
          })
          break
        }
        case 'log':
          setLogs(prev => {
            const next = [...prev, msg.data]
            if (next.length > 500) next.shift()
            return next
          })
          break
        case 'core_status':
          setStatuses(prev => {
            const next = new Map(prev)
            next.set(msg.data.core, msg.data.status)
            return next
          })
          break
        case 'evicted':
          // For v1 we just leave the step lists as-is; they're already
          // size-capped client-side.  Future: drop matching IDs.
          break
        case 'loss':
          setLoss(msg.data)
          break
      }
    }
  }, [])

  const visibleSteps = useMemo(() => {
    const arr = stepsByCore.get(selected) ?? []
    return [...arr].reverse()
  }, [stepsByCore, selected])

  // Compute previous state (step before current in the reversed list).
  const prevStates = useMemo(() => {
    const m = new Map<string, unknown>()
    const ordered = [...(stepsByCore.get(selected) ?? [])]
    for (let i = 1; i < ordered.length; i++) {
      m.set(ordered[i].step_id, ordered[i - 1].state_after)
    }
    return m
  }, [stepsByCore, selected])

  return (
    <div className="flex flex-col h-[calc(100vh-3.5rem)]">
      <div className="flex items-center justify-between border-b px-4 py-2">
        <div className="flex items-center gap-3">
          <h1 className="text-lg font-bold">Observability</h1>
          <Badge variant={connected ? 'success' : 'destructive'}>{connected ? 'Live' : 'Disconnected'}</Badge>
        </div>
        <div className="flex gap-2">
          <Button size="sm" variant="outline" onClick={() => { setDrawerOpen(true); void refreshBreakpoints() }}>
            Breakpoints {breakpoints.length > 0 && <span className="ml-1 text-yellow-400">({breakpoints.length})</span>}
          </Button>
        </div>
      </div>
      <div className="flex flex-1 overflow-hidden">
        <div className="w-60 border-r p-3 overflow-auto">
          <CoresRail selected={selected} statuses={statuses} onSelect={setSelected} loss={loss} />
        </div>
        <div className="flex-1 overflow-auto p-3">
          <div className="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-2">
            {selected.toUpperCase()} steps ({visibleSteps.length})
          </div>
          {visibleSteps.length === 0 && (
            <div className="text-sm text-muted-foreground">no steps recorded yet</div>
          )}
          {visibleSteps.map(step => (
            <StepRow key={step.step_id} step={step} prevState={prevStates.get(step.step_id)} />
          ))}
        </div>
      </div>
      <BottomPanel http={http} logs={logs} />
      {drawerOpen && (
        <BreakpointDrawer
          breakpoints={breakpoints}
          onClose={() => setDrawerOpen(false)}
          onChange={() => void refreshBreakpoints()}
        />
      )}
    </div>
  )
}
