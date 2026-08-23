import React, { createContext, useContext } from 'react';
import type { AuthUser } from './api';
import { useLogout, useMeQuery } from '@/app/queries/auth';

export type User = AuthUser;

interface AuthContextType {
  user: User | null;
  isLoading: boolean;
  logout: () => Promise<void>;
}

const AuthContext = createContext<AuthContextType>({
  user: null,
  isLoading: true,
  logout: async () => {},
});

export function AuthProvider({ children }: { children: React.ReactNode }) {
  const meQuery = useMeQuery();
  const logoutMutation = useLogout();
  const user = meQuery.data?.user ?? null;
  const isLoading = meQuery.isLoading;

  const logout = async () => {
    try {
      await logoutMutation.mutateAsync();
    } catch {
      // onSettled already cleared
    }
  };

  return (
    <AuthContext.Provider value={{ user, isLoading, logout }}>
      {children}
    </AuthContext.Provider>
  );
}

export function useAuth() {
  return useContext(AuthContext);
}
