import { useEffect, useState, useCallback } from 'react'
import type { Observation, SystemState } from '@/types/api'

const MAX_LOG = 500

export function useObservations() {
  const [state, setState] = useState<SystemState | null>(null)
  const [log, setLog] = useState<Observation[]>([])
  const [connected, setConnected] = useState(false)

  const clearLog = useCallback(() => setLog([]), [])

  useEffect(() => {
    const es = new EventSource('/api/v1/stream')

    es.addEventListener('observation', (e: MessageEvent) => {
      const obs = JSON.parse(e.data as string) as Observation
      if (obs.type === 'StateSnapshot') {
        setState(obs.data)
      }
      setLog(prev => [...prev.slice(-(MAX_LOG - 1)), obs])
    })

    es.onopen = () => setConnected(true)
    es.onerror = () => setConnected(false)

    return () => es.close()
  }, [])

  return { state, log, connected, clearLog }
}
