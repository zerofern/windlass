import { useObservations } from '@/hooks/useObservations'
import { StateDisplay } from '@/components/StateDisplay'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'

export function Dashboard() {
  const { state, connected } = useObservations()

  async function handleReset() {
    await fetch('/api/v1/operator/reset', { method: 'POST' })
  }

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <h1 className="text-2xl font-bold">Dashboard</h1>
          <Badge variant={connected ? 'success' : 'destructive'}>
            {connected ? 'Live' : 'Disconnected'}
          </Badge>
        </div>
        <Button variant="destructive" onClick={handleReset}>
          Reset
        </Button>
      </div>

      {state ? (
        <StateDisplay state={state} />
      ) : (
        <p className="text-muted-foreground">Connecting to Windlass…</p>
      )}
    </div>
  )
}
