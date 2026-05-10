import { useAuth } from '@/lib/auth';
import { API_BASE_URL } from '@/lib/api';

export function LoginPage() {
  const { user, isLoading } = useAuth();

  if (isLoading) {
    return (
      <div className="min-h-screen bg-cream flex items-center justify-center">
        <p className="text-muted-foreground">Loading...</p>
      </div>
    );
  }

  if (user) {
    window.location.href = '/';
    return null;
  }

  return (
    <div className="min-h-screen bg-cream flex items-center justify-center">
      <div className="bg-card rounded-2xl p-12 shadow-sm border border-border max-w-md w-full text-center">
        <div className="mb-8">
          <h1 className="font-heading text-4xl font-bold text-primary mb-3">
            Sanctuary
          </h1>
          <p className="text-muted-foreground">
            Your personal life organization app
          </p>
        </div>

        <div className="space-y-6">
          <p className="text-sm text-muted-foreground">
            Sign in with your Google account to get started
          </p>

          <a
            href={`${API_BASE_URL}/auth/google`}
            className="inline-flex items-center justify-center w-full h-11 px-8 rounded-md border border-input bg-card text-foreground hover:bg-muted text-sm font-medium transition-colors"
          >
            <svg className="h-5 w-5 mr-2" viewBox="0 0 24 24">
              <path
                fill="#4285F4"
                d="M22.56 12.25c0-.78-.07-1.53-.2-2.25H12v4.26h5.92c-.26 1.37-1.04 2.53-2.21 3.31v2.77h3.57c2.08-1.92 3.28-4.74 3.28-8.09z"
              />
              <path
                fill="#34A853"
                d="M12 23c2.97 0 5.46-.98 7.28-2.66l-3.57-2.77c-.98.66-2.23 1.06-3.71 1.06-2.86 0-5.29-1.93-6.16-4.53H2.18v2.84C3.99 20.53 7.7 23 12 23z"
              />
              <path
                fill="#FBBC05"
                d="M5.84 14.09c-.22-.66-.35-1.36-.35-2.09s.13-1.43.35-2.09V7.07H2.18C1.43 8.55 1 10.22 1 12s.43 3.45 1.18 4.93l2.85-2.22.81-.62z"
              />
              <path
                fill="#EA4335"
                d="M12 5.38c1.62 0 3.06.56 4.21 1.64l3.15-3.15C17.45 2.09 14.97 1 12 1 7.7 1 3.99 3.47 2.18 7.07l3.66 2.84c.87-2.6 3.3-4.53 6.16-4.53z"
              />
            </svg>
            Continue with Google
          </a>
        </div>

        <p className="mt-8 text-xs text-muted-foreground">
          By signing in, you agree to our Terms of Service and Privacy Policy
        </p>
      </div>
    </div>
  );
}
