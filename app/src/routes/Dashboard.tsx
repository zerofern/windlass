import { useRef, useEffect } from 'react'
import { useObservations } from '@/hooks/useObservations'
import { StateDisplay } from '@/components/StateDisplay'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import type { Observation } from '@/types/api'

function obsLabel(obs: Observation): string {
  if (obs.type === 'EventArrived' || obs.type === 'EventReceived') {
    const d = obs.data
    if (typeof d === 'string') return d
    if (typeof d === 'object' && d !== null) return Object.keys(d as Record<string, unknown>)[0] ?? '?'
  }
  if (obs.type === 'ActionDispatched') {
    const d = obs.data
    if (typeof d === 'string') return d
    if (typeof d === 'object' && d !== null) return Object.keys(d as Record<string, unknown>)[0] ?? '?'
  }
  if (obs.type === 'HttpExchange') return `${obs.data.method} ${obs.data.module}`
  return obs.type
}

function obsBadgeVariant(obs: Observation): 'secondary' | 'warning' | 'default' {
  if (obs.type === 'EventArrived' || obs.type === 'EventReceived') return 'warning'
  if (obs.type === 'ActionDispatched') return 'default'
  return 'secondary'
}

function obsBadgeLabel(obs: Observation): string {
  if (obs.type === 'EventArrived') return 'arrived'
  if (obs.type === 'EventReceived') return 'event'
  if (obs.type === 'ActionDispatched') return 'action'
  if (obs.type === 'HttpExchange') return 'http'
  if (obs.type === 'StateSnapshot') return 'state'
  return 'unknown'
}

export function Dashboard() {
  const { state, log, connected, clearLog } = useObservations()
  const bottomRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: 'smooth' })
  }, [log.length])

  return (
    <div className="flex flex-col gap-6">
      {/* Status */}
      <div className="flex items-center gap-3">
        <h1 className="text-2xl font-bold">Dashboard</h1>
        <Badge variant={connected ? 'success' : 'destructive'}>
          {connected ? 'Live' : 'Disconnected'}
        </Badge>
      </div>

      {state ? <StateDisplay state={state} /> : (
        <p className="text-muted-foreground text-sm">Waiting for first state update…</p>
      )}

      {/* Live Log */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <h2 className="text-sm font-semibold text-muted-foreground uppercase tracking-wider">Live Log</h2>
          <span className="text-xs text-muted-foreground">{log.length} entries</span>
        </div>
        <Button variant="ghost" size="sm" onClick={clearLog}>Clear</Button>
      </div>

      <div className="h-80 overflow-auto rounded-lg border bg-muted/20 p-2 font-mono text-xs">
        {log.length === 0 && (
          <p className="text-muted-foreground p-2">Waiting for events…</p>
        )}
        {log.map((obs, i) => (
          <div key={i} className="flex gap-2 py-0.5 hover:bg-muted/30 rounded px-1">
            <Badge variant={obsBadgeVariant(obs)} className="shrink-0 text-[10px]">
              {obsBadgeLabel(obs)}
            </Badge>
            <span className="truncate text-muted-foreground">{obsLabel(obs)}</span>
          </div>
        ))}
        <div ref={bottomRef} />
      </div>
    </div>
  )
}
