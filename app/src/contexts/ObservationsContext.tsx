import { createContext, useCallback, useContext, useEffect, useRef, useState } from 'react'
import type { Observation, SystemState } from '@/types/api'

const MAX_LOG = 500

interface ObservationsValue {
  state: SystemState | null
  log: Observation[]
  connected: boolean
  debugMode: boolean
  clearLog: () => void
}

const ObservationsContext = createContext<ObservationsValue>({
  state: null,
  log: [],
  connected: false,
  debugMode: false,
  clearLog: () => {},
})

export function ObservationsProvider({ children }: { children: React.ReactNode }) {
  const [state, setState] = useState<SystemState | null>(null)
  const [log, setLog] = useState<Observation[]>([])
  const [connected, setConnected] = useState(false)
  const [debugMode, setDebugMode] = useState(false)
  const clearLog = useCallback(() => setLog([]), [])

  // Keep a ref so the SSE handler always sees the latest log length without re-subscribing
  const logLenRef = useRef(0)
  logLenRef.current = log.length

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

export function useObservations() {
  return useContext(ObservationsContext)
}
