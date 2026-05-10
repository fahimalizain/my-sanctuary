import ReactDOM from 'react-dom/client';
import { RouterProvider } from '@tanstack/react-router';
import { router } from './router';
import { AuthProvider } from '@/lib/auth';

const rootElement = document.getElementById('root')!;
if (!rootElement.innerHTML) {
  const root = ReactDOM.createRoot(rootElement);
  root.render(
    <AuthProvider>
      <RouterProvider router={router} />
    </AuthProvider>,
  );
}
