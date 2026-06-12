import { Routes, Route, NavLink, Navigate } from 'react-router-dom'
import { Observability } from '@/routes/Observability'
import { Download } from '@/routes/Download'
import { DownloadQueue } from '@/routes/DownloadQueue'
import { EventLog } from '@/routes/EventLog'
import { Notifications } from '@/routes/Notifications'
import { TorrentMonitor } from '@/routes/TorrentMonitor'

function NavItem({ to, end, label }: { to: string; end?: boolean; label: string }) {
  return (
    <NavLink
      to={to}
      end={end}
      className={({ isActive }) =>
        `text-sm font-medium transition-colors ${isActive ? 'text-foreground' : 'text-muted-foreground hover:text-foreground'}`
      }
    >
      {label}
    </NavLink>
  )
}

export default function App() {
  return (
    <div className="min-h-screen bg-background">
      <header className="border-b">
        <nav className="container mx-auto flex h-14 items-center gap-6 px-4">
          <span className="font-bold text-lg tracking-tight">⚓ Windlass</span>
          <NavItem to="/download" label="Download" />
          <NavItem to="/torrents" label="Torrents" />
          <NavItem to="/queue" label="Queue" />
          <NavItem to="/events" label="Events" />
          <NavItem to="/observability" label="Observability" />
          <NavItem to="/notifications" label="Notifications" />
        </nav>
      </header>
      <main className="container mx-auto px-4 py-6">
        <Routes>
          <Route path="/" element={<Navigate to="/observability" replace />} />
          <Route path="/download" element={<Download />} />
          <Route path="/torrents" element={<TorrentMonitor />} />
          <Route path="/queue" element={<DownloadQueue />} />
          <Route path="/events" element={<EventLog />} />
          <Route path="/observability" element={<Observability />} />
          <Route path="/notifications" element={<Notifications />} />
        </Routes>
      </main>
    </div>
  )
}
