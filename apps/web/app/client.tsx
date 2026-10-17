import ReactDOM from 'react-dom/client';
import { QueryClientProvider } from '@tanstack/react-query';
import { ReactQueryDevtools } from '@tanstack/react-query-devtools';
import { RouterProvider } from '@tanstack/react-router';
import { router } from './router';
import { AuthProvider } from '@/lib/auth';
import { BackgroundSync } from '@/app/background-sync/BackgroundSync';
import { CalendarEventsProvider } from '@/app/modules/calendar/events-context';
import { queryClient } from '@/lib/queryClient';
import { ThemeProvider } from '@/lib/theme-context';

const rootElement = document.getElementById('root')!;
if (!rootElement.innerHTML) {
  const root = ReactDOM.createRoot(rootElement);
  root.render(
    <ThemeProvider>
      <QueryClientProvider client={queryClient}>
        <AuthProvider>
          <BackgroundSync />
          <CalendarEventsProvider>
            <RouterProvider router={router} />
          </CalendarEventsProvider>
        </AuthProvider>
        {import.meta.env.DEV ? (
          <ReactQueryDevtools
            initialIsOpen={false}
            buttonPosition="bottom-left"
          />
        ) : null}
      </QueryClientProvider>
    </ThemeProvider>,
  );
}
