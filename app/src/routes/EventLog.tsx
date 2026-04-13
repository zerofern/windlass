import { useState, useEffect } from 'react'

interface EventEntry {
  id: number
  source: string
  action: string
  book_id: number | null
  detail: string | null
  created_at: string
}

export function EventLog() {
  const [events, setEvents] = useState<EventEntry[]>([])
  const [error, setError] = useState('')

  const fetchEvents = () => {
    fetch('/api/v1/events?limit=100')
      .then(r => r.json())
      .then(setEvents)
      .catch(() => setError('Failed to load events'))
  }

  useEffect(() => {
    fetchEvents()
    const id = setInterval(fetchEvents, 30_000)
    return () => clearInterval(id)
  }, [])

  return (
    <div className="space-y-4">
      <h1 className="text-2xl font-bold">Event Log</h1>
      {error && <p className="text-red-500 text-sm">{error}</p>}
      {events.length === 0 && !error && (
        <p className="text-muted-foreground">No events recorded yet.</p>
      )}
      {events.length > 0 && (
        <div className="overflow-x-auto">
          <table className="w-full text-sm border-collapse">
            <thead>
              <tr className="border-b text-left text-muted-foreground">
                <th className="py-2 pr-4">Time</th>
                <th className="py-2 pr-4">Source</th>
                <th className="py-2 pr-4">Action</th>
                <th className="py-2 pr-4">Detail</th>
              </tr>
            </thead>
            <tbody>
              {events.map(e => (
                <tr key={e.id} className="border-b hover:bg-muted/40">
                  <td className="py-2 pr-4 text-xs text-muted-foreground whitespace-nowrap">
                    {e.created_at}
                  </td>
                  <td className="py-2 pr-4 font-mono text-xs">{e.source}</td>
                  <td className="py-2 pr-4 font-mono text-xs">{e.action}</td>
                  <td className="py-2 pr-4 text-xs text-muted-foreground">
                    {e.detail ?? '—'}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  )
}
