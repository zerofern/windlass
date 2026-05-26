import { useCallback, useEffect, useState } from 'react'
import { Badge } from '@/components/ui/badge'
import { useObservations } from '@/hooks/useObservations'

interface QueueEntry {
  id: number
  mam_id: number
  title: string | null
  status: string
  created_at: string
  updated_at: string
}

function statusVariant(s: string): string {
  switch (s) {
    case 'pending':     return 'bg-gray-400 text-white'
    case 'downloading': return 'bg-blue-600 text-white'
    case 'seeding':     return 'bg-green-600 text-white'
    case 'satisfied':   return 'bg-green-300 text-green-900'
    case 'failed':      return 'bg-red-600 text-white'
    case 'blacklisted': return 'bg-gray-700 text-white'
    default:            return 'bg-gray-400 text-white'
  }
}

export function DownloadQueue() {
  const [queue, setQueue] = useState<QueueEntry[]>([])
  const [error, setError] = useState('')
  const { log } = useObservations()

  const fetchQueue = useCallback(() => {
    fetch('/api/v1/download-queue')
      .then(r => r.json())
      .then((data: QueueEntry[]) => {
        setQueue(data)
        setError('')
      })
      .catch(() => setError('Failed to load download queue'))
  }, [])

  useEffect(() => {
    fetchQueue()
  }, [fetchQueue, log.length])

  return (
    <div className="space-y-4">
      <h1 className="text-2xl font-bold">Download Queue</h1>
      {error && <p className="text-red-500 text-sm">{error}</p>}
      {queue.length === 0 && !error && (
        <p className="text-muted-foreground">No downloads queued.</p>
      )}
      {queue.length > 0 && (
        <div className="overflow-x-auto">
          <table className="w-full text-sm border-collapse">
            <thead>
              <tr className="border-b text-left text-muted-foreground">
                <th className="py-2 pr-4">ID</th>
                <th className="py-2 pr-4">MAM ID</th>
                <th className="py-2 pr-4">Title</th>
                <th className="py-2 pr-4">Status</th>
                <th className="py-2 pr-4">Queued</th>
                <th className="py-2 pr-4">Updated</th>
              </tr>
            </thead>
            <tbody>
              {queue.map(e => (
                <tr key={e.id} className="border-b hover:bg-muted/40">
                  <td className="py-2 pr-4 font-mono text-xs">{e.id}</td>
                  <td className="py-2 pr-4">{e.mam_id}</td>
                  <td className="py-2 pr-4">{e.title ?? '—'}</td>
                  <td className="py-2 pr-4">
                    <Badge className={statusVariant(e.status)}>{e.status}</Badge>
                  </td>
                  <td className="py-2 pr-4 text-xs text-muted-foreground">{e.created_at}</td>
                  <td className="py-2 pr-4 text-xs text-muted-foreground">{e.updated_at}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  )
}
