import { useContext } from 'react'
import { ObservationsContext } from '@/contexts/observations-context'

export function useObservations() {
  return useContext(ObservationsContext)
}
