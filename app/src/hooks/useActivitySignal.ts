import { useEffect, useState } from 'react'

/**
 * Subscribes to /api/v1/observability/stream and exposes a monotonic
 * `tick` counter that increments on every received SSE message.
 *
 * Pages that previously depended on the legacy `useObservations().log`
 * length as a refetch trigger now depend on `tick` instead.  The hook
 * never holds the actual messages — that's the observability page's
 * job — so it stays cheap to mount on every refreshable list view.
 */
export function useActivitySignal(): { tick: number; connected: boolean } {
  const [tick, setTick] = useState(0)
  const [connected, setConnected] = useState(false)

  useEffect(() => {
    const es = new EventSource('/api/v1/observability/stream')
    es.onopen = () => setConnected(true)
    es.onerror = () => setConnected(false)
    es.addEventListener('observability', () => {
      setTick(t => t + 1)
    })
    return () => { es.close() }
  }, [])

  return { tick, connected }
}
