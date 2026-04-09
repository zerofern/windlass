import type { VpnState, QbitState, MamState, SystemState } from '@/types/api'
import { Badge } from '@/components/ui/badge'
import { Card, CardHeader, CardTitle, CardContent } from '@/components/ui/card'

function vpnLabel(s: VpnState): string {
  if (s === 'Stopped') return 'Stopped'
  if (s === 'DumpingLogs') return 'Dumping Logs'
  if (s === 'Starting') return 'Starting'
  if (s === 'AwaitingTunnel') return 'Awaiting Tunnel'
  if ('Connected' in s) return `Connected ${s.Connected.ip}:${s.Connected.port}`
  return 'Unknown'
}

function vpnVariant(s: VpnState): 'success' | 'warning' | 'destructive' | 'secondary' {
  if (typeof s === 'object' && 'Connected' in s) return 'success'
  if (s === 'Starting' || s === 'AwaitingTunnel') return 'warning'
  if (s === 'Stopped') return 'destructive'
  return 'secondary'
}

function qbitLabel(s: QbitState): string {
  if (s === 'Offline') return 'Offline'
  if (typeof s === 'object') {
    if ('Authenticating' in s) return `Authenticating #${s.Authenticating.attempt}`
    if ('Authenticated' in s) return 'Authenticated'
    if ('SyncingPort' in s) return `Syncing Port :${s.SyncingPort.target}`
    if ('Ready' in s) return `Ready :${s.Ready.port}`
  }
  return 'Unknown'
}

function qbitVariant(s: QbitState): 'success' | 'warning' | 'destructive' | 'secondary' {
  if (typeof s === 'object' && 'Ready' in s) return 'success'
  if (typeof s === 'object' && ('Authenticating' in s || 'SyncingPort' in s || 'Authenticated' in s)) return 'warning'
  if (s === 'Offline') return 'destructive'
  return 'secondary'
}

function mamLabel(s: MamState): string {
  if (s === 'Unknown') return 'Unknown'
  if (typeof s === 'object') {
    if ('SyncPending' in s) return `Sync Pending → ${s.SyncPending.target_ip}`
    if ('Synced' in s) return `Synced ${s.Synced.ip}:${s.Synced.port}`
    if ('AsnBlocked' in s) return `ASN Blocked (${s.AsnBlocked.ip})`
  }
  return 'Unknown'
}

function mamVariant(s: MamState): 'success' | 'warning' | 'destructive' | 'secondary' {
  if (typeof s === 'object' && 'Synced' in s) return 'success'
  if (typeof s === 'object' && 'SyncPending' in s) return 'warning'
  if (typeof s === 'object' && 'AsnBlocked' in s) return 'destructive'
  return 'secondary'
}

interface Props { state: SystemState; compact?: boolean }

export function StateDisplay({ state, compact }: Props) {
  const grid = compact
    ? 'grid grid-cols-2 gap-2'
    : 'grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3'
  return (
    <div className={grid}>
      <Card>
        <CardHeader><CardTitle>System</CardTitle></CardHeader>
        <CardContent className="space-y-2">
          <div className="flex items-center justify-between">
            <span className="text-sm text-muted-foreground">Known torrents</span>
            <span className="text-sm font-mono">{state.known_torrents.length}</span>
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardHeader><CardTitle>VPN</CardTitle></CardHeader>
        <CardContent>
          <Badge variant={vpnVariant(state.vpn)}>{vpnLabel(state.vpn)}</Badge>
        </CardContent>
      </Card>

      <Card>
        <CardHeader><CardTitle>qBittorrent</CardTitle></CardHeader>
        <CardContent>
          <Badge variant={qbitVariant(state.qbit)}>{qbitLabel(state.qbit)}</Badge>
        </CardContent>
      </Card>

      <Card>
        <CardHeader><CardTitle>MAM</CardTitle></CardHeader>
        <CardContent>
          <Badge variant={mamVariant(state.mam)}>{mamLabel(state.mam)}</Badge>
        </CardContent>
      </Card>
    </div>
  )
}
