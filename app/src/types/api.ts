// ── Primitives ────────────────────────────────────────────────────────────

export type RetryCount = number   // serialises as u8
export type VpnPort = number      // serialises as u16
export type VpnIp = string        // serialises as "1.2.3.4"
export type AuthCookie = "[redacted]"

// ── Enums ─────────────────────────────────────────────────────────────────

export type VpnState =
  | "Stopped"
  | "DumpingLogs"
  | "Starting"
  | "AwaitingTunnel"
  | { Connected: { ip: VpnIp; port: VpnPort } }

export type QbitState =
  | "Offline"
  | { Authenticating: { attempt: RetryCount } }
  | { Authenticated: { cookie: AuthCookie } }
  | { SyncingPort: { attempt: RetryCount; cookie: AuthCookie; target: VpnPort } }
  | { Ready: { port: VpnPort; cookie: AuthCookie } }

export type MamStatus = "Connectable" | "NotConnectable" | "Unreachable"

export type MamState =
  | "Unknown"
  | { SyncPending: { target_ip: VpnIp; target_port: VpnPort } }
  | { Synced: { port: VpnPort; ip: VpnIp } }
  | { AsnBlocked: { ip: VpnIp } }

// ── SystemState ───────────────────────────────────────────────────────────

export interface SystemState {
  vpn: VpnState
  qbit: QbitState
  mam: MamState
  known_torrents: string[]
}

// ── Observations ──────────────────────────────────────────────────────────

// Event and Action have many variants — we display them as raw JSON
export type RustEvent = unknown
export type RustAction = unknown

export interface HttpExchange {
  module: string
  method: string
  url: string
  request_body?: string
  response_status: number
  response_body: string
}

export type Observation =
  | { type: "StateSnapshot"; data: SystemState }
  | { type: "DebugModeChanged"; data: boolean }
  // Legacy variants — no longer sent by the backend but kept so Dashboard/Chaos compile
  | { type: "EventArrived"; data: unknown }
  | { type: "EventReceived"; data: unknown }
  | { type: "ActionDispatched"; data: unknown }
  | { type: "HttpExchange"; data: HttpExchange }

// ── Debug ─────────────────────────────────────────────────────────────────

export type PausedOn =
  | { kind: "Event"; variant: string }
  | { kind: "Action"; variant: string; index: number; of: number }

export interface StoredEvent {
  id: string
  at: string
  arrived_at: string
  variant: string
  payload: unknown
  caused_by_action: string | null
}

export interface ActionEntry {
  id: string
  variant: string
  payload: unknown
  parent_event_id: string
  started_at: string
  completed_at: string | null
  caused_event_id: string | null
  http_exchanges: HttpExchange[]
}

export interface ActiveEvent {
  stored: StoredEvent
  state_before: SystemState
  started_at: string
  actions: ActionEntry[]
  /** Actions waiting to be dispatched — populated before dispatch begins. */
  pending_actions: unknown[]
}

export interface RunningAction {
  id: string
  variant: string
  payload: unknown
  parent_event_id: string
  started_at: string
}

export interface TraceEntry {
  event: StoredEvent
  state_before: SystemState
  state_after: SystemState
  actions: ActionEntry[]
  completed_at: string
}

export interface LogEntry {
  at: string
  level: string
  target: string
  message: string
}

export interface DebugState {
  seq: number
  debug_mode: boolean
  paused_on: PausedOn | null
  event_breakpoints: string[]
  action_breakpoints: string[]
  event_queue: StoredEvent[]
  current_event: ActiveEvent | null
  running_actions: RunningAction[]
  trace: TraceEntry[]
  logs: LogEntry[]
  latest_state: SystemState
}
