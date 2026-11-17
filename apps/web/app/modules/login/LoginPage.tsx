import { useState } from 'react';
import { Loader2 } from 'lucide-react';
import { useAuth } from '@/lib/auth';
import { API_BASE_URL, ApiError } from '@/lib/api';
import {
  useProvidersQuery,
  useStandaloneLogin,
  useStandaloneRegister,
} from '@/app/queries/auth';
import { browserTimeZone, resolveLoginOptions } from './login-options';

type Mode = 'signin' | 'register';

const INPUT_CLASS =
  'w-full rounded-xl border border-input bg-background px-4 py-2.5 text-foreground placeholder:text-muted-foreground/60 focus:border-primary focus:outline-none focus:ring-2 focus:ring-primary/20 transition-all text-sm disabled:opacity-60';

function errorText(error: unknown): string {
  if (error instanceof ApiError) return error.message;
  return 'Something went wrong. Please try again.';
}

export function LoginPage() {
  const { user, isLoading } = useAuth();
  const providersQuery = useProvidersQuery();
  const login = useStandaloneLogin();
  const register = useStandaloneRegister();

  const [mode, setMode] = useState<Mode>('signin');
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [displayName, setDisplayName] = useState('');

  // The provider list is the server's answer for *this* host; on a failed
  // read we behave like the pre-standalone page (Google only) so a working
  // Google deployment cannot be locked out by a providers hiccup.
  const options = resolveLoginOptions(
    providersQuery.data,
    providersQuery.error,
  );

  const active = mode === 'signin' ? login : register;
  const submitting = active.isPending;

  const switchMode = (next: Mode) => {
    if (next === mode) return;
    setMode(next);
    // Each mutation owns its error; never show a stale one for the other form.
    login.reset();
    register.reset();
  };

  const handleSubmit = async (event: React.FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const trimmedUsername = username.trim();
    if (!trimmedUsername || !password) return;

    try {
      if (mode === 'signin') {
        await login.mutateAsync({ username: trimmedUsername, password });
      } else {
        const timeZone = browserTimeZone();
        await register.mutateAsync({
          username: trimmedUsername,
          password,
          ...(displayName.trim() ? { name: displayName.trim() } : {}),
          ...(timeZone ? { time_zone: timeZone } : {}),
        });
      }
      // Full navigation, same as the Google flow: every query cache (and the
      // background observers) starts fresh for the new session.
      window.location.href = '/';
    } catch {
      // The mutation's error renders below; nothing to do here.
    }
  };

  if (isLoading || providersQuery.isPending) {
    return (
      <div className="min-h-screen bg-cream flex items-center justify-center">
        <Loader2
          className="h-6 w-6 animate-spin text-muted-foreground"
          aria-label="Loading"
        />
      </div>
    );
  }

  if (user) {
    window.location.href = '/';
    return null;
  }

  return (
    <div className="min-h-screen bg-cream flex items-center justify-center px-4">
      <div className="bg-card rounded-2xl p-8 sm:p-12 shadow-sm border border-border max-w-md w-full text-center">
        <div className="mb-8">
          <h1 className="font-heading text-4xl font-bold text-primary mb-3">
            Sanctuary
          </h1>
          <p className="text-muted-foreground">
            Your personal life organization app
          </p>
        </div>

        <div className="space-y-6">
          {options.google && (
            <>
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
            </>
          )}

          {options.google && options.standalone && (
            <div className="flex items-center gap-3 text-xs text-muted-foreground">
              <span className="h-px flex-1 bg-border" />
              or
              <span className="h-px flex-1 bg-border" />
            </div>
          )}

          {options.standalone && (
            <form className="space-y-4 text-left" onSubmit={handleSubmit}>
              <div
                className="inline-flex w-full rounded-lg border border-input bg-muted/40 p-0.5 text-sm"
                role="tablist"
                aria-label="Sign-in method"
              >
                <button
                  type="button"
                  role="tab"
                  aria-selected={mode === 'signin'}
                  onClick={() => switchMode('signin')}
                  className={`flex-1 rounded-md px-3 py-1.5 font-medium transition-colors ${
                    mode === 'signin'
                      ? 'bg-card text-foreground shadow-sm'
                      : 'text-muted-foreground hover:text-foreground'
                  }`}
                >
                  Sign in
                </button>
                <button
                  type="button"
                  role="tab"
                  aria-selected={mode === 'register'}
                  onClick={() => switchMode('register')}
                  className={`flex-1 rounded-md px-3 py-1.5 font-medium transition-colors ${
                    mode === 'register'
                      ? 'bg-card text-foreground shadow-sm'
                      : 'text-muted-foreground hover:text-foreground'
                  }`}
                >
                  Create account
                </button>
              </div>

              {mode === 'register' && (
                <div className="space-y-1">
                  <label
                    htmlFor="login-name"
                    className="text-sm font-medium text-foreground"
                  >
                    Display name{' '}
                    <span className="text-muted-foreground font-normal">
                      (optional)
                    </span>
                  </label>
                  <input
                    id="login-name"
                    type="text"
                    autoComplete="name"
                    maxLength={80}
                    value={displayName}
                    onChange={(e) => setDisplayName(e.target.value)}
                    className={INPUT_CLASS}
                    disabled={submitting}
                  />
                </div>
              )}

              <div className="space-y-1">
                <label
                  htmlFor="login-username"
                  className="text-sm font-medium text-foreground"
                >
                  Username
                </label>
                <input
                  id="login-username"
                  type="text"
                  autoComplete="username"
                  autoCapitalize="none"
                  spellCheck={false}
                  required
                  minLength={3}
                  maxLength={32}
                  value={username}
                  onChange={(e) => setUsername(e.target.value)}
                  className={INPUT_CLASS}
                  disabled={submitting}
                />
              </div>

              <div className="space-y-1">
                <label
                  htmlFor="login-password"
                  className="text-sm font-medium text-foreground"
                >
                  Password
                </label>
                <input
                  id="login-password"
                  type="password"
                  autoComplete={
                    mode === 'signin' ? 'current-password' : 'new-password'
                  }
                  required
                  minLength={mode === 'register' ? 8 : undefined}
                  maxLength={128}
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                  className={INPUT_CLASS}
                  disabled={submitting}
                />
              </div>

              {active.error && (
                <p role="alert" className="text-sm text-destructive">
                  {errorText(active.error)}
                </p>
              )}

              <button
                type="submit"
                disabled={submitting}
                className="inline-flex items-center justify-center w-full h-11 px-8 rounded-md bg-primary text-primary-foreground hover:bg-primary/90 text-sm font-medium transition-colors disabled:pointer-events-none disabled:opacity-60"
              >
                {submitting && (
                  <Loader2 className="h-4 w-4 mr-2 animate-spin" />
                )}
                {mode === 'signin' ? 'Sign in' : 'Create account'}
              </button>

              <p className="text-xs text-muted-foreground">
                Local accounts sign in with a username and password and keep
                their data on this server — they do not use Google.
              </p>
            </form>
          )}

          {options.fallbackNotice && (
            <p className="text-xs text-muted-foreground">
              {options.fallbackNotice}
            </p>
          )}

          {!options.google && !options.standalone && (
            <div className="rounded-xl border border-border bg-muted/40 p-4 text-sm text-muted-foreground">
              No sign-in method configured. Ask the administrator to set up
              Google sign-in or enable local accounts.
            </div>
          )}
        </div>

        <p className="mt-8 text-xs text-muted-foreground">
          By signing in, you agree to our Terms of Service and Privacy Policy
        </p>
      </div>
    </div>
  );
}
