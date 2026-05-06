import { createRootRoute, createRoute, createRouter } from '@tanstack/react-router'
import { RootComponent } from './routes/__root'
import { HomeComponent } from './routes/index'
import { LoginComponent } from './routes/login'
import { StreamsComponent } from './routes/streams'
import { CalendarComponent } from './routes/calendar'
import { ConsistencyComponent } from './routes/consistency'
import { SettingsComponent } from './routes/settings'

const rootRoute = createRootRoute({
  component: RootComponent,
})

const indexRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/',
  component: HomeComponent,
})

const loginRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/login',
  component: LoginComponent,
})

const streamsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/streams',
  component: StreamsComponent,
})

const calendarRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/calendar',
  component: CalendarComponent,
})

const consistencyRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/consistency',
  component: ConsistencyComponent,
})

const settingsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/settings',
  component: SettingsComponent,
})

const routeTree = rootRoute.addChildren([
  indexRoute,
  loginRoute,
  streamsRoute,
  calendarRoute,
  consistencyRoute,
  settingsRoute,
])

export const router = createRouter({ routeTree })

declare module '@tanstack/react-router' {
  interface Register {
    router: typeof router
  }
}
