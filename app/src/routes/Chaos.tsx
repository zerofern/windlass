import { useState } from 'react'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Card, CardHeader, CardTitle, CardContent } from '@/components/ui/card'

interface Scenario {
  id: string
  label: string
  description: string
  variant: 'destructive' | 'warning' | 'secondary'
}

const SCENARIOS: Scenario[] = [
  {
    id: 'reset',
    label: 'Reset (Happy Path)',
    description: 'Restore all mocks to their default healthy responses.',
    variant: 'secondary',
  },
  {
    id: 'qbit-auth-fail',
    label: 'qBit Auth Fail',
    description: 'qBittorrent login returns "Fails." — Windlass should retry and eventually alert.',
    variant: 'destructive',
  },
  {
    id: 'mam-rate-limit',
    label: 'MAM Rate Limit',
    description: 'MAM returns 429 on all endpoints — triggers the rate-limit guard.',
    variant: 'warning',
  },
]

type Status = 'idle' | 'loading' | 'ok' | 'error'

interface Props {
  chaosUrl: string
}

export function Chaos({ chaosUrl }: Props) {
  const [statuses, setStatuses] = useState<Record<string, Status>>({})

  async function applyScenario(scenario: Scenario) {
    setStatuses(s => ({ ...s, [scenario.id]: 'loading' }))
    try {
      const url = scenario.id === 'reset'
        ? `${chaosUrl}/reset`
        : `${chaosUrl}/scenario/${scenario.id}`
      const res = await fetch(url, { method: 'POST' })
      setStatuses(s => ({ ...s, [scenario.id]: res.ok ? 'ok' : 'error' }))
    } catch {
      setStatuses(s => ({ ...s, [scenario.id]: 'error' }))
    }
    // Clear status after 3 s
    setTimeout(() => setStatuses(s => ({ ...s, [scenario.id]: 'idle' })), 3000)
  }

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-2xl font-bold">Chaos Controller</h1>
        <p className="text-sm text-muted-foreground mt-1">
          Inject failure scenarios into the mock stack. Changes take effect immediately — watch the Live Log for Windlass's response.
        </p>
      </div>

      <div className="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3">
        {SCENARIOS.map(scenario => {
          const status = statuses[scenario.id] ?? 'idle'
          return (
            <Card key={scenario.id}>
              <CardHeader className="pb-2">
                <div className="flex items-center justify-between">
                  <CardTitle className="text-base">{scenario.label}</CardTitle>
                  <Badge variant={scenario.variant} className="text-[10px]">
                    {scenario.id === 'reset' ? 'reset' : 'fault'}
                  </Badge>
                </div>
                <p className="text-sm text-muted-foreground">{scenario.description}</p>
              </CardHeader>
              <CardContent>
                <Button
                  variant={scenario.variant === 'destructive' ? 'destructive' : 'outline'}
                  size="sm"
                  className="w-full"
                  disabled={status === 'loading'}
                  onClick={() => applyScenario(scenario)}
                >
                  {status === 'loading' && 'Applying…'}
                  {status === 'ok' && '✓ Applied'}
                  {status === 'error' && '✗ Failed'}
                  {status === 'idle' && 'Apply'}
                </Button>
              </CardContent>
            </Card>
          )
        })}
      </div>
    </div>
  )
}
