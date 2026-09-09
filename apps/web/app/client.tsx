import ReactDOM from 'react-dom/client';
import { QueryClientProvider } from '@tanstack/react-query';
import { ReactQueryDevtools } from '@tanstack/react-query-devtools';
import { RouterProvider } from '@tanstack/react-router';
import { router } from './router';
import { AuthProvider } from '@/lib/auth';
import { CalendarEventsProvider } from '@/app/modules/calendar/events-context';
import { queryClient } from '@/lib/queryClient';

const rootElement = document.getElementById('root')!;
if (!rootElement.innerHTML) {
  const root = ReactDOM.createRoot(rootElement);
  root.render(
    <QueryClientProvider client={queryClient}>
      <AuthProvider>
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
    </QueryClientProvider>,
  );
}
