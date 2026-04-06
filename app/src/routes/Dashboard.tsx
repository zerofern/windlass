import { useRef, useEffect } from 'react'
import { useObservations } from '@/contexts/ObservationsContext'
import { StateDisplay } from '@/components/StateDisplay'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import type { Observation } from '@/types/api'

function obsLabel(obs: Observation): string {
  if (obs.type === 'StateSnapshot') return 'State'
  if (obs.type === 'EventReceived') {
    const d = obs.data
    if (typeof d === 'string') return d
    if (typeof d === 'object' && d !== null) return Object.keys(d as Record<string, unknown>)[0] ?? '?'
  }
  if (obs.type === 'ActionDispatched') {
    const d = obs.data
    if (typeof d === 'string') return d
    if (typeof d === 'object' && d !== null) return Object.keys(d as Record<string, unknown>)[0] ?? '?'
  }
  return obs.type
}

function obsBadgeVariant(obs: Observation): 'secondary' | 'warning' | 'default' {
  if (obs.type === 'StateSnapshot') return 'secondary'
  if (obs.type === 'EventReceived') return 'warning'
  return 'default'
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
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <h1 className="text-2xl font-bold">Dashboard</h1>
          <Badge variant={connected ? 'success' : 'destructive'}>
            {connected ? 'Live' : 'Disconnected'}
          </Badge>
        </div>
        <Button variant="destructive" size="sm" onClick={() =>
          fetch('/api/v1/operator/reset', { method: 'POST' })
        }>
          Reset
        </Button>
      </div>

      {state ? <StateDisplay state={state} /> : (
        <p className="text-muted-foreground text-sm">Connecting to Windlass…</p>
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
              {obs.type === 'StateSnapshot' ? 'state' : obs.type === 'EventReceived' ? 'event' : 'action'}
            </Badge>
            <span className="truncate text-muted-foreground">{obsLabel(obs)}</span>
          </div>
        ))}
        <div ref={bottomRef} />
      </div>
    </div>
  )
}
