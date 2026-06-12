import { type ClassValue, clsx } from 'clsx'
import { twMerge } from 'tailwind-merge'

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs))
}

/**
 * fetch() that fails loudly: a non-2xx response throws with the
 * status line instead of surfacing as a confusing JSON parse error
 * (an axum 500 body is plain text, not JSON).
 */
export async function fetchJson<T>(url: string, init?: RequestInit): Promise<T> {
  const r = await fetch(url, init)
  if (!r.ok) {
    throw new Error(`${r.status} ${r.statusText}`)
  }
  return r.json() as Promise<T>
}
