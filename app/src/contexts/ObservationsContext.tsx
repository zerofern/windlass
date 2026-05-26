import { useCallback, useEffect, useState } from 'react'
import type { Observation, SystemState } from '@/types/api'
import { ObservationsContext } from '@/contexts/observations-context'

const MAX_LOG = 500

export function ObservationsProvider({ children }: { children: React.ReactNode }) {
  const [state, setState] = useState<SystemState | null>(null)
  const [log, setLog] = useState<Observation[]>([])
  const [connected, setConnected] = useState(false)
  const [debugMode, setDebugMode] = useState(false)
  const clearLog = useCallback(() => setLog([]), [])

  useEffect(() => {
    const es = new EventSource('/api/v1/stream')

    es.addEventListener('observation', (e: MessageEvent) => {
      const obs = JSON.parse(e.data as string) as Observation
      if (obs.type === 'StateSnapshot') {
        setState(obs.data)
      } else if (obs.type === 'DebugModeChanged') {
        setDebugMode(obs.data)
        return // don't add debug mode changes to the log
      }
      setLog(prev => [...prev.slice(-(MAX_LOG - 1)), obs])
    })

    es.onopen = () => setConnected(true)
    es.onerror = () => setConnected(false)

    return () => es.close()
  }, [])

  return (
    <ObservationsContext.Provider value={{ state, log, connected, debugMode, clearLog }}>
      {children}
    </ObservationsContext.Provider>
  )
}
