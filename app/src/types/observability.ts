// SSE wire shape for /api/v1/observability/stream.  Mirrors
// windlass-observability's SseMessage / supporting types.

export type CoreId = 'vpn' | 'qbit' | 'mam' | 'db' | 'disk' | 'docker' | 'domain'

export const ALL_CORES: readonly CoreId[] = [
  'vpn',
  'qbit',
  'mam',
  'db',
  'disk',
  'docker',
  'domain',
] as const

export type CoreStatus =
  | { state: 'running' }
  | { state: 'pause_requested' }
  | {
      state: 'parked_at_event'
      variant: string
      since: string
    }
  | {
      state: 'parked_at_outcome'
      source_variant: string
      since: string
    }
  | {
      state: 'parked_at_http'
      method: string
      url: string
      since: string
    }
  | { state: 'stepping' }

export type StepKind = { kind: 'event' } | { kind: 'command'; response: 'sent' | 'receiver_dropped' }

export type StoredExternalCause =
  | { source: 'timer'; name: string }
  | { source: 'file_watcher'; path: string }
  | { source: 'docker_event'; kind: string }
  | { source: 'manual_command' }
  | { source: 'init' }
  | { source: 'unknown' }

export type StoredEventCause =
  | { kind: 'action'; id: string }
  | { kind: 'publish'; id: string }
  | { kind: 'external'; source: StoredExternalCause['source'] } & StoredExternalCause

export interface StoredAction {
  action_id: string
  variant: string
  payload: unknown
}

export interface StoredPublish {
  publish_id: string
  topic: string
  variant: string
  payload: unknown
}

export interface StoredStepRecord {
  step_id: string
  core: CoreId
  recorded_at: string
  duration_ms: number
  kind: StepKind
  event_variant: string
  event: unknown
  event_cause: StoredEventCause
  state_after: unknown
  actions: StoredAction[]
  publishes: StoredPublish[]
}

export type BodyCapture =
  | { kind: 'inline'; value: unknown }
  | { kind: 'text'; value: string }
  | { kind: 'bytes'; value: number }
  | {
      kind: 'truncated'
      body_kind: 'json' | 'text' | 'form' | 'binary'
      captured: unknown
      original_len: number
    }
  | { kind: 'none' }

export interface StoredHttpExchange {
  exchange_id: string
  action_id: string | null
  core: CoreId
  at: string
  method: string
  url: string
  request_body: BodyCapture
  response_status: number
  response_body: BodyCapture
  duration_ms: number
}

export interface StoredLogLine {
  at: string
  level: string
  target: string
  message: string
}

export interface EvictedIds {
  step_ids: string[]
  action_ids: string[]
  publish_ids: string[]
  reveal_ids: string[]
}

export interface CoreCounters {
  dropped_steps: number
  truncated_bodies: number
  reservation_failures: number
}

export interface HttpCounters {
  dropped_exchanges: number
  truncated_request_bodies: number
  truncated_response_bodies: number
}

export interface LossCounters {
  per_core: Partial<Record<CoreId, CoreCounters>>
  http: HttpCounters
}

export type Breakpoint =
  | { kind: 'event_variant'; variant: string }
  | { kind: 'action_variant'; variant: string }
  | { kind: 'publish_variant'; variant: string }
  | { kind: 'http_url_pattern'; pattern: string }

export interface HelloSnapshot {
  protocol_version: number
  cores: [CoreId, CoreStatus][]
  steps: StoredStepRecord[]
  http: StoredHttpExchange[]
  logs: StoredLogLine[]
  loss: LossCounters
  active_breakpoints: Breakpoint[]
}

// Externally-tagged-with-content shape: { kind, data } per
// `tag = "kind", content = "data"` on the Rust SseMessage enum.
export type SseMessage =
  | { kind: 'hello'; data: HelloSnapshot }
  | { kind: 'step'; data: StoredStepRecord }
  | { kind: 'http_exchange'; data: StoredHttpExchange }
  | { kind: 'log'; data: StoredLogLine }
  | { kind: 'core_status'; data: { core: CoreId; status: CoreStatus } }
  | { kind: 'evicted'; data: EvictedIds }
  | { kind: 'loss'; data: LossCounters }
