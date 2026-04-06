import { useEffect, useRef } from 'react'
import { useObservations } from '@/hooks/useObservations'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import type { Observation } from '@/types/api'

function obsVariant(obs: Observation): 'secondary' | 'warning' | 'default' {
  if (obs.type === 'StateSnapshot') return 'secondary'
  if (obs.type === 'EventReceived') return 'warning'
  return 'default'
}

function obsLabel(obs: Observation): string {
  if (obs.type === 'StateSnapshot') return 'State'
  if (obs.type === 'EventReceived') {
    const d = obs.data
    if (typeof d === 'string') return `Event: ${d}`
    if (typeof d === 'object' && d !== null) return `Event: ${Object.keys(d as Record<string, unknown>)[0] ?? '?'}`
  }
  if (obs.type === 'ActionDispatched') {
    const d = obs.data
    if (typeof d === 'string') return `Action: ${d}`
    if (typeof d === 'object' && d !== null) return `Action: ${Object.keys(d as Record<string, unknown>)[0] ?? '?'}`
  }
  return obs.type
}

export function Log() {
  const { log, connected, clearLog } = useObservations()
  const bottomRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: 'smooth' })
  }, [log.length])

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <h1 className="text-2xl font-bold">Live Log</h1>
          <Badge variant={connected ? 'success' : 'destructive'}>
            {connected ? 'Live' : 'Disconnected'}
          </Badge>
          <span className="text-sm text-muted-foreground">{log.length} entries</span>
        </div>
        <Button variant="outline" size="sm" onClick={clearLog}>Clear</Button>
      </div>

      <div className="h-[calc(100vh-12rem)] overflow-auto rounded-lg border bg-muted/30 p-2 font-mono text-xs">
        {log.length === 0 && (
          <p className="text-muted-foreground p-2">Waiting for events…</p>
        )}
        {log.map((obs, i) => (
          <div key={i} className="flex gap-2 py-0.5 hover:bg-muted/50 rounded px-1">
            <Badge variant={obsVariant(obs)} className="shrink-0 text-[10px]">
              {obsLabel(obs)}
            </Badge>
            <span className="truncate text-muted-foreground">
              {obs.type === 'StateSnapshot'
                ? `vpn=${JSON.stringify((obs.data).vpn)} qbit=${JSON.stringify((obs.data).qbit)}`
                : JSON.stringify(obs.data)}
            </span>
          </div>
        ))}
        <div ref={bottomRef} />
      </div>
    </div>
  )
}
