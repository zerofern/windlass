import { createContext } from 'react'
import type { Observation, SystemState } from '@/types/api'

export interface ObservationsValue {
  state: SystemState | null
  log: Observation[]
  connected: boolean
  debugMode: boolean
  clearLog: () => void
}

export const ObservationsContext = createContext<ObservationsValue>({
  state: null,
  log: [],
  connected: false,
  debugMode: false,
  clearLog: () => {},
})
