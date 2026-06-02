import { Routes, Route, NavLink } from 'react-router-dom'
import { Dashboard } from '@/routes/Dashboard'
import { Observability } from '@/routes/Observability'
import { Chaos } from '@/routes/Chaos'
import { Download } from '@/routes/Download'
import { DownloadQueue } from '@/routes/DownloadQueue'
import { EventLog } from '@/routes/EventLog'
import { Notifications } from '@/routes/Notifications'
import { TorrentMonitor } from '@/routes/TorrentMonitor'
import { useConfig } from '@/hooks/useConfig'
import { ObservationsProvider } from '@/contexts/ObservationsContext'

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
  const config = useConfig()

  return (
    <ObservationsProvider>
      <div className="min-h-screen bg-background">
        <header className="border-b">
          <nav className="container mx-auto flex h-14 items-center gap-6 px-4">
            <span className="font-bold text-lg tracking-tight">⚓ Windlass</span>
            <NavItem to="/" end label="Dashboard" />
            <NavItem to="/download" label="Download" />
            <NavItem to="/torrents" label="Torrent Monitor" />
            <NavItem to="/queue" label="Queue" />
            <NavItem to="/events" label="Events" />
            <NavItem to="/observability" label="Observability" />
            <NavItem to="/notifications" label="Notifications" />
            {config.chaos_url && <NavItem to="/chaos" label="Chaos" />}
          </nav>
        </header>
        <main className="container mx-auto px-4 py-6">
          <Routes>
            <Route path="/" element={<Dashboard />} />
            <Route path="/download" element={<Download />} />
            <Route path="/torrents" element={<TorrentMonitor />} />
            <Route path="/queue" element={<DownloadQueue />} />
            <Route path="/events" element={<EventLog />} />
            <Route path="/observability" element={<Observability />} />
            <Route path="/notifications" element={<Notifications />} />
            {config.chaos_url && (
              <Route path="/chaos" element={<Chaos chaosUrl={config.chaos_url} />} />
            )}
          </Routes>
        </main>
      </div>
    </ObservationsProvider>
  )
}
