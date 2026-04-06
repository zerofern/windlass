import { useState, useRef, useEffect } from 'react'
import { useObservations } from '@/contexts/ObservationsContext'
import { StateDisplay } from '@/components/StateDisplay'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'

interface Fault {
  id: string
  label: string
  description: string
}

const FAULTS: Fault[] = [
  {
    id: 'qbit-auth-fail',
    label: 'qBit Auth Fail',
    description: 'qBittorrent login returns "Fails." — triggers auth retry loop.',
  },
  {
    id: 'mam-rate-limit',
    label: 'MAM Rate Limit',
    description: 'MAM returns 429 on all endpoints — triggers the rate-limit guard.',
  },
]

interface Props { chaosUrl: string }

export function Chaos({ chaosUrl }: Props) {
  const { state, log, connected } = useObservations()
  const [active, setActive] = useState<Set<string>>(new Set())
  const [applying, setApplying] = useState(false)
  const bottomRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: 'smooth' })
  }, [log.length])

  async function applyFaultSet(next: Set<string>) {
    setApplying(true)
    try {
      await fetch(`${chaosUrl}/reset`, { method: 'POST' })
      for (const id of next) {
        await fetch(`${chaosUrl}/scenario/${id}`, { method: 'POST' })
      }
      setActive(next)
    } finally {
      setApplying(false)
    }
  }

  async function toggle(id: string, checked: boolean) {
    const next = new Set(active)
    if (checked) { next.add(id) } else { next.delete(id) }
    await applyFaultSet(next)
  }

  async function resetAll() {
    await applyFaultSet(new Set())
  }

  return (
    <div className="flex flex-col gap-6">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <h1 className="text-2xl font-bold">Chaos</h1>
          <Badge variant={connected ? 'success' : 'destructive'}>
            {connected ? 'Live' : 'Disconnected'}
          </Badge>
          {active.size > 0 && (
            <Badge variant="destructive">{active.size} fault{active.size > 1 ? 's' : ''} active</Badge>
          )}
        </div>
        <Button variant="outline" size="sm" onClick={resetAll} disabled={applying || active.size === 0}>
          Reset All
        </Button>
      </div>

      <div className="grid grid-cols-1 gap-6 lg:grid-cols-2">
        {/* Fault toggles */}
        <div className="flex flex-col gap-3">
          <p className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">Faults</p>
          {FAULTS.map(fault => {
            const isActive = active.has(fault.id)
            return (
              <label
                key={fault.id}
                className={`flex cursor-pointer items-start gap-3 rounded-lg border p-4 transition-colors ${
                  isActive ? 'border-destructive/50 bg-destructive/10' : 'hover:bg-muted/30'
                } ${applying ? 'opacity-50 pointer-events-none' : ''}`}
              >
                <input
                  type="checkbox"
                  checked={isActive}
                  onChange={e => toggle(fault.id, e.target.checked)}
                  className="mt-0.5 accent-red-500"
                />
                <div>
                  <p className="text-sm font-medium">{fault.label}</p>
                  <p className="text-xs text-muted-foreground mt-0.5">{fault.description}</p>
                </div>
              </label>
            )
          })}
        </div>

        {/* Current state */}
        <div className="flex flex-col gap-3">
          <p className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">Current State</p>
          {state ? <StateDisplay state={state} compact /> : (
            <p className="text-muted-foreground text-sm">Connecting…</p>
          )}
        </div>
      </div>

      {/* Live log */}
      <div className="flex items-center gap-2">
        <p className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">Live Log</p>
        <span className="text-xs text-muted-foreground">{log.length} entries</span>
      </div>
      <div className="h-64 overflow-auto rounded-lg border bg-muted/20 p-2 font-mono text-xs">
        {log.length === 0 && <p className="text-muted-foreground p-2">Waiting…</p>}
        {log.map((obs, i) => (
          <div key={i} className="flex gap-2 py-0.5 hover:bg-muted/30 rounded px-1">
            <Badge
              variant={obs.type === 'EventReceived' ? 'warning' : obs.type === 'StateSnapshot' ? 'secondary' : 'default'}
              className="shrink-0 text-[10px]"
            >
              {obs.type === 'StateSnapshot' ? 'state' : obs.type === 'EventReceived' ? 'event' : 'action'}
            </Badge>
            <span className="truncate text-muted-foreground">
              {JSON.stringify(obs.data)}
            </span>
          </div>
        ))}
        <div ref={bottomRef} />
      </div>
    </div>
  )
}
