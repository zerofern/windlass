// ── Primitives ────────────────────────────────────────────────────────────

export type RetryCount = number   // serialises as u8
export type VpnPort = number      // serialises as u16
export type VpnIp = string        // serialises as "1.2.3.4"
export type AuthCookie = "[redacted]"

// ── Enums ─────────────────────────────────────────────────────────────────

export type RunMode =
  | "Active"
  | { Fatal: { reason: string } }

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
  run_mode: RunMode
  hard_recoveries: RetryCount
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
  | { type: "EventReceived"; data: RustEvent }
  | { type: "ActionDispatched"; data: RustAction }
  | { type: "HttpExchange"; data: HttpExchange }

// ── Debug ─────────────────────────────────────────────────────────────────

export interface DebugState {
  frozen: boolean
  debug_mode: boolean
  pending_event: unknown | null
  pending_actions: unknown[]
  event_breakpoints: string[]
  action_breakpoints: string[]
}
