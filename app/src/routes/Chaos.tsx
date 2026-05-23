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
    id: 'qbit-connection-refused',
    label: 'qBit Connection Refused',
    description: 'All qBittorrent endpoints return 503 — simulates container crash.',
  },
  {
    id: 'mam-rate-limit',
    label: 'MAM Rate Limit',
    description: 'MAM returns 429 on all endpoints — triggers the rate-limit guard.',
  },
  {
    id: 'mam-not-connectable',
    label: 'MAM Not Connectable',
    description: 'MAM reports connectable: "no" — triggers port-forward loop.',
  },
  {
    id: 'mam-asn-mismatch',
    label: 'MAM ASN Mismatch',
    description: 'MAM reports a different IP than the VPN port files — triggers mismatch handling.',
  },
]

interface VpnState {
  ip: string
  port: number
  healthy: boolean
}

interface Props { chaosUrl: string }

export function Chaos({ chaosUrl }: Props) {
  const { state, log, connected } = useObservations()
  const [active, setActive] = useState<Set<string>>(new Set())
  const [applying, setApplying] = useState(false)
  const bottomRef = useRef<HTMLDivElement>(null)

  // Gluetun state
  const [vpn, setVpn] = useState<VpnState | null>(null)
  const [vpnIp, setVpnIp] = useState('10.8.0.1')
  const [vpnPort, setVpnPort] = useState('51820')
  const [vpnBusy, setVpnBusy] = useState(false)

  // Fetch real active state from chaos controller on mount
  useEffect(() => {
    fetch(`${chaosUrl}/active`)
      .then(r => r.json())
      .then((data: { active: string[] }) => setActive(new Set(data.active)))
      .catch(() => {/* controller unreachable, leave empty */})

    fetch(`${chaosUrl}/gluetun/state`)
      .then(r => r.json())
      .then((data: VpnState) => {
        setVpn(data)
        setVpnIp(data.ip)
        setVpnPort(String(data.port))
      })
      .catch(() => {/* gluetun unreachable */})
  }, [chaosUrl])

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

  async function setVpnFiles() {
    const port = parseInt(vpnPort, 10)
    if (!vpnIp || isNaN(port)) return
    setVpnBusy(true)
    try {
      const r = await fetch(`${chaosUrl}/gluetun/set-files`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ ip: vpnIp, port }),
      })
      if (r.ok) setVpn({ ip: vpnIp, port, healthy: true })
    } finally {
      setVpnBusy(false)
    }
  }

  async function gluetunHealthDown() {
    setVpnBusy(true)
    try {
      const r = await fetch(`${chaosUrl}/gluetun/health/down`, { method: 'POST' })
      if (r.ok) setVpn(v => v ? { ...v, healthy: false } : v)
    } finally {
      setVpnBusy(false)
    }
  }

  async function gluetunHealthUp() {
    setVpnBusy(true)
    try {
      const r = await fetch(`${chaosUrl}/gluetun/health/up`, { method: 'POST' })
      if (r.ok) {
        const data: VpnState = await r.json()
        setVpn({ ip: data.ip, port: data.port, healthy: true })
      }
    } finally {
      setVpnBusy(false)
    }
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

      {/* Gluetun controls */}
      <div className="flex flex-col gap-3">
        <div className="flex items-center gap-3">
          <p className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">Gluetun (VPN)</p>
          {vpn && (
            <Badge variant={vpn.healthy ? 'success' : 'destructive'}>
              {vpn.healthy ? 'healthy' : 'unhealthy'}
            </Badge>
          )}
          {vpn && <span className="text-xs text-muted-foreground font-mono">{vpn.ip}:{vpn.port}</span>}
        </div>

        {/* Port file update */}
        <div className="rounded-lg border p-4 flex flex-col gap-3">
          <p className="text-xs font-medium text-muted-foreground">VPN Port Files</p>
          <div className="flex items-center gap-2">
            <input
              type="text"
              value={vpnIp}
              onChange={e => setVpnIp(e.target.value)}
              placeholder="IP (e.g. 10.8.0.1)"
              className="h-8 w-36 rounded border bg-background px-2 text-xs font-mono"
            />
            <input
              type="number"
              value={vpnPort}
              onChange={e => setVpnPort(e.target.value)}
              placeholder="Port"
              className="h-8 w-24 rounded border bg-background px-2 text-xs font-mono"
            />
            <Button size="sm" onClick={setVpnFiles} disabled={vpnBusy}>
              Set Files
            </Button>
          </div>
          <p className="text-xs text-muted-foreground">
            Writes ip + port to VPN files. Triggers <code>PortFileReadResult</code> via the file watcher.
          </p>
        </div>

        {/* Health toggle */}
        <div className="rounded-lg border p-4 flex flex-col gap-3">
          <p className="text-xs font-medium text-muted-foreground">Healthcheck</p>
          <div className="flex items-center gap-2">
            <Button
              variant="destructive"
              size="sm"
              onClick={gluetunHealthDown}
              disabled={vpnBusy || vpn?.healthy === false}
            >
              Gluetun Down
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={gluetunHealthUp}
              disabled={vpnBusy || vpn?.healthy === true}
            >
              Gluetun Up
            </Button>
          </div>
          <p className="text-xs text-muted-foreground">
            <strong>Down</strong>: clears the port file so the Docker healthcheck fails →{' '}
            <code>DockerGluetunDied</code>.{' '}
            <strong>Up</strong>: restores the last ip/port → <code>DockerGluetunHealthy</code>.
          </p>
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
