import { useState } from 'react'

export function Download() {
  const [input, setInput] = useState('')
  const [status, setStatus] = useState<'idle' | 'loading' | 'success' | 'error'>('idle')
  const [message, setMessage] = useState('')

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault()
    const trimmed = input.trim()
    if (!trimmed) return

    setStatus('loading')
    setMessage('')

    try {
      const res = await fetch('/api/v1/download/add', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ mam_url: trimmed }),
      })

      if (res.status === 202) {
        setStatus('success')
        setMessage('Download queued — torrent will appear in qBit shortly.')
        setInput('')
      } else if (res.status === 400) {
        setStatus('error')
        setMessage('Invalid MAM URL or torrent ID. Paste a full URL or enter a numeric ID.')
      } else {
        setStatus('error')
        setMessage(`Unexpected response: ${res.status}`)
      }
    } catch {
      setStatus('error')
      setMessage('Network error — could not reach Windlass.')
    }
  }

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-2xl font-bold tracking-tight">Download</h1>
        <p className="text-muted-foreground mt-1">
          Paste a MAM torrent URL or enter a numeric torrent ID to add it to qBittorrent.
        </p>
      </div>

      <form onSubmit={handleSubmit} className="flex gap-2 max-w-xl">
        <input
          type="text"
          value={input}
          onChange={e => setInput(e.target.value)}
          placeholder="https://www.myanonamouse.net/t/12345  or  12345"
          className="flex-1 rounded-md border border-input bg-background px-3 py-2 text-sm shadow-sm placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring"
          disabled={status === 'loading'}
        />
        <button
          type="submit"
          disabled={status === 'loading' || !input.trim()}
          className="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground shadow hover:bg-primary/90 disabled:opacity-50"
        >
          {status === 'loading' ? 'Queuing…' : 'Download'}
        </button>
      </form>

      {status === 'success' && (
        <p className="text-sm text-green-600 dark:text-green-400">{message}</p>
      )}
      {status === 'error' && (
        <p className="text-sm text-red-600 dark:text-red-400">{message}</p>
      )}
    </div>
  )
}
