import { useRef, useEffect, useState, useCallback } from 'react'
import { useObservations } from '@/contexts/ObservationsContext'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import type { DebugState, HttpExchange, Observation } from '@/types/api'

// ── helpers ───────────────────────────────────────────────────────────────────

function variantLabel(v: unknown): string {
  if (typeof v === 'string') return v
  if (typeof v === 'object' && v !== null)
    return Object.keys(v as Record<string, unknown>)[0] ?? '?'
  return '?'
}

async function api(path: string, method = 'POST') {
  await fetch(`/api/v1/debug${path}`, { method })
}

// ── sub-components ────────────────────────────────────────────────────────────

function JsonBlock({ value }: { value: unknown }) {
  return (
    <pre className="text-[10px] font-mono whitespace-pre-wrap break-all text-muted-foreground">
      {JSON.stringify(value, null, 2)}
    </pre>
  )
}

function BreakpointList({
  title,
  variants,
  active,
  onToggle,
}: {
  title: string
  variants: string[]
  active: string[]
  onToggle: (v: string, on: boolean) => void
}) {
  return (
    <div className="flex flex-col gap-1">
      <p className="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1">
        {title}
      </p>
      {variants.map(v => {
        const on = active.includes(v)
        return (
          <label key={v} className="flex items-center gap-2 cursor-pointer select-none">
            <input
              type="checkbox"
              checked={on}
              onChange={e => onToggle(v, e.target.checked)}
              className="accent-primary"
            />
            <span className={`text-xs font-mono ${on ? 'text-yellow-500 font-semibold' : 'text-foreground'}`}>
              {v}
            </span>
          </label>
        )
      })}
    </div>
  )
}

function HttpLog({ items }: { items: HttpExchange[] }) {
  const bottomRef = useRef<HTMLDivElement>(null)
  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: 'smooth' })
  }, [items.length])

  return (
    <div className="flex flex-col gap-2 min-w-0">
      <p className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
        HTTP Exchanges <span className="font-normal">({items.length})</span>
      </p>
      <div className="h-[calc(100vh-26rem)] overflow-auto rounded-lg border bg-muted/20 p-2 font-mono text-xs">
        {items.length === 0 && (
          <p className="text-muted-foreground p-2">None yet — enable debug mode to capture HTTP traffic.</p>
        )}
        {items.map((x, i) => (
          <div key={i} className="mb-3 border-b border-muted/40 pb-2">
            <div className="flex items-center gap-2 mb-1">
              <Badge variant="secondary" className="text-[10px] shrink-0">{x.module}</Badge>
              <span className="text-primary font-bold">{x.method}</span>
              <span className="truncate text-muted-foreground">{x.url}</span>
              <Badge
                variant={x.response_status < 400 ? 'success' : 'destructive'}
                className="text-[10px] ml-auto shrink-0"
              >
                {x.response_status}
              </Badge>
            </div>
            {x.request_body && (
              <details className="text-[10px]">
                <summary className="cursor-pointer text-muted-foreground">Request body</summary>
                <JsonBlock value={tryParse(x.request_body)} />
              </details>
            )}
            <details className="text-[10px]">
              <summary className="cursor-pointer text-muted-foreground">Response body</summary>
              <JsonBlock value={tryParse(x.response_body)} />
            </details>
          </div>
        ))}
        <div ref={bottomRef} />
      </div>
    </div>
  )
}

function tryParse(s: string): unknown {
  try { return JSON.parse(s) } catch { return s }
}

// ── main component ────────────────────────────────────────────────────────────

export function Debug() {
  const { log, connected, debugMode } = useObservations()

  const [debugState, setDebugState] = useState<DebugState | null>(null)
  const [eventVariants,  setEventVariants]  = useState<string[]>([])
  const [actionVariants, setActionVariants] = useState<string[]>([])

  // Extract HttpExchange observations from the SSE log
  const httpLog: HttpExchange[] = (log as Observation[])
    .filter((o): o is Extract<Observation, { type: 'HttpExchange' }> => o.type === 'HttpExchange')
    .map(o => o.data)

  const refreshDebugState = useCallback(async () => {
    const res = await fetch('/api/v1/debug')
    if (res.ok) setDebugState(await res.json() as DebugState)
  }, [])

  // Initial load of variants + debug state
  useEffect(() => {
    void refreshDebugState()
    void fetch('/api/v1/debug/events').then(r => r.json()).then(d => setEventVariants(d as string[]))
    void fetch('/api/v1/debug/actions').then(r => r.json()).then(d => setActionVariants(d as string[]))
  }, [refreshDebugState])

  // Refresh debug state on every event/action so pending queues stay current
  useEffect(() => {
    const last = log[log.length - 1]
    if (last?.type === 'EventReceived' || last?.type === 'ActionDispatched') {
      void refreshDebugState()
    }
  }, [log, refreshDebugState])

  async function toggleDebugMode() {
    await api(debugMode ? '/disable' : '/enable')
    await refreshDebugState()
  }

  async function stepEvent() {
    await api('/step/event')
    await refreshDebugState()
  }

  async function stepAction() {
    await api('/step/action')
    await refreshDebugState()
  }

  async function toggleBreakpoint(kind: 'event' | 'action', variant: string, on: boolean) {
    const method = on ? 'POST' : 'DELETE'
    await api(`/breakpoints/${kind}/${encodeURIComponent(variant)}`, method)
    await refreshDebugState()
  }

  const hasPendingEvent   = debugState?.pending_event   != null
  const hasPendingActions = (debugState?.pending_actions?.length ?? 0) > 0

  return (
    <div className="flex flex-col gap-6">
      {/* Header row */}
      <div className="flex items-center justify-between flex-wrap gap-3">
        <div className="flex items-center gap-3">
          <h1 className="text-2xl font-bold">Debug</h1>
          <Badge variant={connected ? 'success' : 'destructive'}>
            {connected ? 'Live' : 'Disconnected'}
          </Badge>
          {debugMode && <Badge variant="secondary">Debug Mode</Badge>}
        </div>
        <div className="flex gap-2">
          <Button
            variant={debugMode ? 'destructive' : 'default'}
            size="sm"
            onClick={toggleDebugMode}
          >
            {debugMode ? 'Disable Debug' : 'Enable Debug'}
          </Button>
        </div>
      </div>

      {/* Main layout: left controls + right http log */}
      <div className="grid grid-cols-[1fr_1fr] gap-6">

        {/* Left column: queue panels + breakpoints */}
        <div className="flex flex-col gap-4">

          {/* Pending Event */}
          <div className="rounded-lg border p-3 flex flex-col gap-2">
            <div className="flex items-center justify-between">
              <p className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
                Pending Event
              </p>
              <Button size="sm" variant="outline" onClick={stepEvent} disabled={!hasPendingEvent}>
                Step Event ▶
              </Button>
            </div>
            {hasPendingEvent
              ? <JsonBlock value={debugState?.pending_event} />
              : <p className="text-xs text-muted-foreground">No event queued.</p>}
          </div>

          {/* Pending Actions */}
          <div className="rounded-lg border p-3 flex flex-col gap-2">
            <div className="flex items-center justify-between">
              <p className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
                Pending Actions <span className="font-normal">({debugState?.pending_actions?.length ?? 0})</span>
              </p>
              <Button size="sm" variant="outline" onClick={stepAction} disabled={!hasPendingActions}>
                Step Action ▶
              </Button>
            </div>
            {!hasPendingActions
              ? <p className="text-xs text-muted-foreground">No actions queued.</p>
              : (debugState?.pending_actions ?? []).map((a, i) => (
                  <div key={i} className="flex items-center gap-2">
                    <Badge variant="default" className="text-[10px] shrink-0">{variantLabel(a)}</Badge>
                    <span className="text-[10px] font-mono text-muted-foreground truncate">
                      {JSON.stringify(a)}
                    </span>
                  </div>
                ))}
          </div>

          {/* Breakpoints */}
          {debugMode && (
            <div className="rounded-lg border p-3 grid grid-cols-2 gap-4">
              <BreakpointList
                title="Event Breakpoints"
                variants={eventVariants}
                active={debugState?.event_breakpoints ?? []}
                onToggle={(v, on) => void toggleBreakpoint('event', v, on)}
              />
              <BreakpointList
                title="Action Breakpoints"
                variants={actionVariants}
                active={debugState?.action_breakpoints ?? []}
                onToggle={(v, on) => void toggleBreakpoint('action', v, on)}
              />
            </div>
          )}
        </div>

        {/* Right column: HTTP exchange log */}
        <HttpLog items={httpLog} />
      </div>
    </div>
  )
}
