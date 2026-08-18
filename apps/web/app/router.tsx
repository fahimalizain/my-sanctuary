import type { ComponentType } from 'react';
import {
  createRootRoute,
  createRoute,
  createRouter,
} from '@tanstack/react-router';
import { RootComponent } from './routes/__root';
import { HomeComponent } from './routes/index';
import { LoginComponent } from './routes/login';
import { ListsComponent } from './routes/lists';
import { CategoriesComponent } from './routes/categories';
import { CalendarComponent } from './routes/calendar';
import { ConsistencyComponent } from './routes/consistency';
import { SettingsComponent } from './routes/settings';
import { AuthGuard } from './components/AuthGuard';

const rootRoute = createRootRoute({
  component: RootComponent,
});

function withAuth(Component: ComponentType) {
  return function ProtectedRoute() {
    return (
      <AuthGuard>
        <Component />
      </AuthGuard>
    );
  };
}

const indexRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/',
  component: withAuth(HomeComponent),
});

const loginRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/login',
  component: LoginComponent,
});

const listsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/lists',
  component: withAuth(ListsComponent),
});

const categoriesRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/categories',
  component: withAuth(CategoriesComponent),
});

const calendarRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/calendar',
  component: withAuth(CalendarComponent),
});

const consistencyRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/consistency',
  component: withAuth(ConsistencyComponent),
});

const settingsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/settings',
  component: withAuth(SettingsComponent),
});

const routeTree = rootRoute.addChildren([
  indexRoute,
  loginRoute,
  listsRoute,
  categoriesRoute,
  calendarRoute,
  consistencyRoute,
  settingsRoute,
]);

export const router = createRouter({ routeTree });

declare module '@tanstack/react-router' {
  interface Register {
    router: typeof router;
  }
}
