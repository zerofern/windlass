import { useCallback, useEffect, useState } from 'react'
import { Badge } from '@/components/ui/badge'
import { useActivitySignal } from '@/hooks/useActivitySignal'
import { fetchJson } from '@/lib/utils'

interface Torrent {
  hash: string
  name: string
  title: string | null
  mam_id: number | null
  state: string
  seeding_time_secs: number
  downloaded_bytes: number
  hnr_satisfied: boolean
  hnr_hours_remaining: number
  added_at: string
  seen_at: string
}

function stateBadge(state: string) {
  const s = state.toLowerCase()
  if (s === 'uploading' || s === 'forceduploading' || s === 'forcedupload') {
    return <Badge className="bg-green-600 text-white">Seeding</Badge>
  }
  if (s.includes('downloading') && !s.includes('stalled')) {
    return <Badge className="bg-blue-600 text-white">Downloading</Badge>
  }
  if (s.includes('paused')) {
    return <Badge className="bg-yellow-500 text-white">Paused</Badge>
  }
  if (s.includes('stalled')) {
    return <Badge className="bg-orange-500 text-white">Stalled</Badge>
  }
  if (s === 'error') {
    return <Badge variant="destructive">Error</Badge>
  }
  return <Badge variant="secondary">{state}</Badge>
}

function hnrBadge(t: Torrent) {
  if (t.hnr_satisfied) {
    return <Badge className="bg-green-600 text-white">Satisfied</Badge>
  }
  const s = t.state.toLowerCase()
  const atRisk = s.includes('stalled') || s === 'error'
  if (atRisk) {
    return <Badge variant="destructive">At Risk</Badge>
  }
  return (
    <Badge className="bg-amber-500 text-white">
      {t.hnr_hours_remaining}h remaining
    </Badge>
  )
}

function fmtMb(bytes: number) {
  return `${(bytes / 1_048_576).toFixed(1)} MB`
}

export function TorrentMonitor() {
  const [torrents, setTorrents] = useState<Torrent[]>([])
  const [error, setError] = useState('')
  const { tick } = useActivitySignal()

  const fetchTorrents = useCallback(() => {
    fetchJson<Torrent[]>('/api/v1/torrents')
      .then(data => {
        setTorrents(data)
        setError('')
      })
      .catch((e: Error) => setError(`Failed to load torrents: ${e.message}`))
  }, [])

  useEffect(() => {
    fetchTorrents()
  }, [fetchTorrents, tick])

  return (
    <div className="space-y-4">
      <h1 className="text-2xl font-bold">Torrent Monitor</h1>
      {error && <p className="text-red-500 text-sm">{error}</p>}
      {torrents.length === 0 && !error && (
        <p className="text-muted-foreground">No torrents tracked yet.</p>
      )}
      {torrents.length > 0 && (
        <div className="overflow-x-auto">
          <table className="w-full text-sm border-collapse">
            <thead>
              <tr className="border-b text-left text-muted-foreground">
                <th className="py-2 pr-4">Name</th>
                <th className="py-2 pr-4">State</th>
                <th className="py-2 pr-4">Seeded</th>
                <th className="py-2 pr-4">HnR Status</th>
                <th className="py-2 pr-4">Downloaded</th>
              </tr>
            </thead>
            <tbody>
              {torrents.map(t => (
                <tr key={t.hash} className="border-b hover:bg-muted/40">
                  <td className="py-2 pr-4 font-medium">
                    {t.title ?? t.name}
                  </td>
                  <td className="py-2 pr-4">{stateBadge(t.state)}</td>
                  <td className="py-2 pr-4">
                    {Math.floor(t.seeding_time_secs / 3600)}h
                  </td>
                  <td className="py-2 pr-4">{hnrBadge(t)}</td>
                  <td className="py-2 pr-4">{fmtMb(t.downloaded_bytes)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  )
}
