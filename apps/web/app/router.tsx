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
import { BoardComponent } from './routes/board';
import type { BoardSearch } from '@/app/modules/board';
import { CategoriesComponent } from './routes/categories';
import { CalendarComponent } from './routes/calendar';
import { ConsistencyComponent } from './routes/consistency';
import { RoutinesComponent } from './routes/routines';
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

// ADR 0002 § Filters: the board's filter state lives in the URL, so the
// route validates the search shape itself. All params are optional; missing
// params = all. Only the *shape* is enforced here — unknown category ids are
// ignored by the page, which checks them against the loaded categories.
const boardRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/board',
  component: withAuth(BoardComponent),
  validateSearch: (search: Record<string, unknown>): BoardSearch => ({
    priority:
      search.priority === 'high' ||
      search.priority === 'medium' ||
      search.priority === 'low'
        ? search.priority
        : undefined,
    difficulty:
      search.difficulty === 'easy' ||
      search.difficulty === 'medium' ||
      search.difficulty === 'hard'
        ? search.difficulty
        : undefined,
    // Comma-separated category ids (e.g. "id1,id2"). Repeated params arrive
    // as an array and non-strings are dropped — both fall back to no filter.
    category:
      typeof search.category === 'string' && search.category.length > 0
        ? search.category
        : undefined,
    // Free-text title/description filter. Mirrors `category`: non-strings,
    // '' and whitespace-only all mean "no filter". The raw string is kept
    // (not trimmed) when it has non-whitespace content, so mid-query spaces
    // survive; the matcher trims before comparing.
    search:
      typeof search.search === 'string' && search.search.trim().length > 0
        ? search.search
        : undefined,
  }),
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

// ADR 0004: /routines is deliberately NOT a nav tab — it is linked from Home
// (and Settings), so the floating nav stays at five items.
const routinesRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/routines',
  component: withAuth(RoutinesComponent),
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
  boardRoute,
  categoriesRoute,
  calendarRoute,
  consistencyRoute,
  routinesRoute,
  settingsRoute,
]);

export const router = createRouter({ routeTree });

declare module '@tanstack/react-router' {
  interface Register {
    router: typeof router;
  }
}
