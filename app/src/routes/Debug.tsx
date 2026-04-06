import { useRef, useEffect, useState } from 'react'
import { useObservations } from '@/contexts/ObservationsContext'
import { StateDisplay } from '@/components/StateDisplay'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import type { Observation } from '@/types/api'

function labelFor(obs: Observation): string {
  const d = obs.data
  if (typeof d === 'string') return d
  if (typeof d === 'object' && d !== null)
    return Object.keys(d as Record<string, unknown>)[0] ?? '?'
  return obs.type
}

function LogColumn({
  title,
  items,
  badgeVariant,
}: {
  title: string
  items: Observation[]
  badgeVariant: 'warning' | 'default' | 'secondary'
}) {
  const bottomRef = useRef<HTMLDivElement>(null)
  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: 'smooth' })
  }, [items.length])

  return (
    <div className="flex flex-col gap-2 min-w-0">
      <p className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
        {title} <span className="font-normal">({items.length})</span>
      </p>
      <div className="h-[calc(100vh-22rem)] overflow-auto rounded-lg border bg-muted/20 p-2 font-mono text-xs">
        {items.length === 0 && (
          <p className="text-muted-foreground p-2">None yet…</p>
        )}
        {items.map((obs, i) => (
          <div key={i} className="flex gap-2 py-0.5 hover:bg-muted/30 rounded px-1">
            <Badge variant={badgeVariant} className="shrink-0 text-[10px]">
              {labelFor(obs)}
            </Badge>
            <span className="truncate text-muted-foreground text-[10px]">
              {JSON.stringify(obs.data)}
            </span>
          </div>
        ))}
        <div ref={bottomRef} />
      </div>
    </div>
  )
}

export function Debug() {
  const { state, log, connected } = useObservations()
  const [frozen, setFrozen] = useState(false)

  const events  = log.filter(o => o.type === 'EventReceived')
  const actions = log.filter(o => o.type === 'ActionDispatched')

  async function freeze() {
    await fetch('/api/v1/operator/freeze', { method: 'POST' })
    setFrozen(true)
  }

  async function unfreeze() {
    await fetch('/api/v1/operator/unfreeze', { method: 'POST' })
    setFrozen(false)
  }

  return (
    <div className="flex flex-col gap-6">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <h1 className="text-2xl font-bold">Debug</h1>
          <Badge variant={connected ? 'success' : 'destructive'}>
            {connected ? 'Live' : 'Disconnected'}
          </Badge>
          {frozen && <Badge variant="warning">Frozen</Badge>}
        </div>
        <div className="flex gap-2">
          <Button variant="outline" size="sm" onClick={freeze}  disabled={frozen}>Freeze</Button>
          <Button variant="outline" size="sm" onClick={unfreeze} disabled={!frozen}>Unfreeze</Button>
        </div>
      </div>

      {/* State */}
      {state ? <StateDisplay state={state} /> : (
        <p className="text-muted-foreground text-sm">Connecting…</p>
      )}

      {/* Three-column log */}
      <div className="grid grid-cols-3 gap-4">
        <LogColumn title="Events"  items={events}  badgeVariant="warning" />
        <LogColumn title="Actions" items={actions} badgeVariant="default" />
        <LogColumn title="All"     items={log}     badgeVariant="secondary" />
      </div>
    </div>
  )
}
