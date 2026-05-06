import { Outlet, Link, useLocation } from '@tanstack/react-router'
import '../styles/globals.css'
import { Home, LayoutGrid, CalendarDays, Target, Settings } from 'lucide-react'
import { cn } from '@/lib/utils'

function Navigation() {
  const location = useLocation()
  
  const navItems = [
    { path: '/', label: 'Home', icon: Home },
    { path: '/streams', label: 'Streams', icon: LayoutGrid },
    { path: '/calendar', label: 'Calendar', icon: CalendarDays },
    { path: '/consistency', label: 'Consistency', icon: Target },
    { path: '/settings', label: 'Settings', icon: Settings },
  ]

  // Don't show nav on login page
  if (location.pathname === '/login') {
    return null
  }

  return (
    <nav className="fixed bottom-6 left-1/2 -translate-x-1/2 bg-white rounded-full shadow-lg px-2 py-2 z-50">
      <div className="flex items-center gap-1">
        {navItems.map((item) => {
          const Icon = item.icon
          const isActive = location.pathname === item.path
          
          return (
            <Link
              key={item.path}
              to={item.path}
              className={cn(
                'flex items-center gap-2 px-4 py-2 rounded-full transition-all',
                isActive 
                  ? 'bg-sanctuary-green text-white' 
                  : 'text-muted-foreground hover:bg-gray-100'
              )}
            >
              <Icon className="h-5 w-5" />
              {isActive && (
                <span className="text-sm font-medium">{item.label}</span>
              )}
            </Link>
          )
        })}
      </div>
    </nav>
  )
}

export function RootComponent() {
  return (
    <>
      <Outlet />
      <Navigation />
    </>
  )
}
