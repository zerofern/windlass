import { useEffect, useState } from 'react'

interface AppConfig {
  chaos_url: string | null
}

export function useConfig(): AppConfig {
  const [config, setConfig] = useState<AppConfig>({ chaos_url: null })

  useEffect(() => {
    fetch('/api/v1/config')
      .then(r => r.json())
      .then(setConfig)
      .catch(() => {/* stay with defaults */})
  }, [])

  return config
}
