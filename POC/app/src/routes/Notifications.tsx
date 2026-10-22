import { useState, useEffect } from 'react'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { fetchJson } from '@/lib/utils'

interface Alert {
  id: number
  priority: 'info' | 'warning' | 'critical'
  title: string
  body: string
  read: boolean
  created_at: string
}

function priorityVariant(p: string): 'secondary' | 'warning' | 'destructive' {
  if (p === 'critical') return 'destructive'
  if (p === 'warning') return 'warning'
  return 'secondary'
}

export function Notifications() {
  const [alerts, setAlerts] = useState<Alert[]>([])
  const [error, setError] = useState('')

  const fetchAlerts = () => {
    fetchJson<Alert[]>('/api/v1/alerts')
      .then(data => {
        setAlerts(data)
        setError('')
      })
      .catch((e: Error) => setError(`Failed to load alerts: ${e.message}`))
  }

  useEffect(() => { fetchAlerts() }, [])

  const markRead = (id: number) => {
    fetch(`/api/v1/alerts/${id}/read`, { method: 'POST' })
      .then(fetchAlerts)
      .catch((e: Error) => setError(`Failed to mark alert read: ${e.message}`))
  }

  const unreadCount = alerts.filter(a => !a.read).length

  return (
    <div className="space-y-4">
      <div className="flex items-center gap-3">
        <h1 className="text-2xl font-bold">Notifications</h1>
        {unreadCount > 0 && (
          <Badge variant="destructive">{unreadCount} unread</Badge>
        )}
      </div>
      {error && <p className="text-red-500 text-sm">{error}</p>}
      {alerts.length === 0 && !error && (
        <p className="text-muted-foreground">No alerts.</p>
      )}
      {alerts.map(alert => (
        <div
          key={alert.id}
          className={`rounded-lg border p-4 space-y-1 ${!alert.read ? 'bg-muted/40' : ''}`}
        >
          <div className="flex items-center justify-between gap-2">
            <div className="flex items-center gap-2">
              <Badge variant={priorityVariant(alert.priority)}>{alert.priority}</Badge>
              <span className="font-semibold">{alert.title}</span>
            </div>
            {!alert.read && (
              <Button size="sm" variant="outline" onClick={() => markRead(alert.id)}>
                Mark read
              </Button>
            )}
          </div>
          <p className="text-sm text-muted-foreground">{alert.body}</p>
          <p className="text-xs text-muted-foreground">{alert.created_at}</p>
        </div>
      ))}
    </div>
  )
}
