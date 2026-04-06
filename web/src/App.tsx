import { Routes, Route, NavLink } from 'react-router-dom'
import { Dashboard } from '@/routes/Dashboard'
import { Log } from '@/routes/Log'

export default function App() {
  return (
    <div className="min-h-screen bg-background">
      <header className="border-b">
        <nav className="container mx-auto flex h-14 items-center gap-6 px-4">
          <span className="font-bold text-lg">⚓ Windlass</span>
          <NavLink
            to="/"
            end
            className={({ isActive }) =>
              `text-sm font-medium transition-colors ${isActive ? 'text-foreground' : 'text-muted-foreground hover:text-foreground'}`
            }
          >
            Dashboard
          </NavLink>
          <NavLink
            to="/log"
            className={({ isActive }) =>
              `text-sm font-medium transition-colors ${isActive ? 'text-foreground' : 'text-muted-foreground hover:text-foreground'}`
            }
          >
            Live Log
          </NavLink>
        </nav>
      </header>
      <main className="container mx-auto px-4 py-6">
        <Routes>
          <Route path="/" element={<Dashboard />} />
          <Route path="/log" element={<Log />} />
        </Routes>
      </main>
    </div>
  )
}
