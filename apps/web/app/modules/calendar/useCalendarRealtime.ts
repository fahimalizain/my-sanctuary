import { useEffect } from 'react';
import { useQueryClient, type QueryClient } from '@tanstack/react-query';
import { useAuth } from '@/lib/auth';
import {
  calendarCatchupQueryKeys,
  parseRealtimeMessage,
  shouldInvalidateCalendarOnMessage,
  shouldInvalidateCalendarOnOpen,
} from './lib/realtime';

const PING_INTERVAL_MS = 25_000;
const BACKOFF_INITIAL_MS = 1_000;
const BACKOFF_MAX_MS = 30_000;

// Cancel-then-invalidate so the latest same-key GET wins over an in-flight older one.
export async function catchUpCalendarQueries(
  queryClient: QueryClient,
): Promise<void> {
  const { events, calendars } = calendarCatchupQueryKeys();
  await queryClient.cancelQueries({ queryKey: events });
  await Promise.all([
    queryClient.invalidateQueries({ queryKey: events }),
    queryClient.invalidateQueries({ queryKey: calendars }),
  ]);
}

/**
 * Same-origin WebSocket to `/api/realtime` while the user is logged in.
 * On open/reconnect and on `calendar.changed`, catch up calendar queries
 * (cancel in-flight events GETs, then invalidate events + calendars).
 * Optimistic overlays in CalendarEventsProvider still win for in-flight edits.
 */
export function useCalendarRealtime(): void {
  const { user, isLoading } = useAuth();
  const queryClient = useQueryClient();

  useEffect(() => {
    if (isLoading || !user) {
      return;
    }

    let stopped = false;
    let ws: WebSocket | null = null;
    let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
    let pingTimer: ReturnType<typeof setInterval> | null = null;
    let backoffMs = BACKOFF_INITIAL_MS;

    const clearPing = () => {
      if (pingTimer !== null) {
        clearInterval(pingTimer);
        pingTimer = null;
      }
    };

    const clearReconnect = () => {
      if (reconnectTimer !== null) {
        clearTimeout(reconnectTimer);
        reconnectTimer = null;
      }
    };

    const scheduleReconnect = () => {
      if (stopped) return;
      clearReconnect();
      const delay = backoffMs;
      backoffMs = Math.min(backoffMs * 2, BACKOFF_MAX_MS);
      reconnectTimer = setTimeout(() => {
        reconnectTimer = null;
        connect();
      }, delay);
    };

    const connect = () => {
      if (stopped) return;

      const protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
      const url = `${protocol}//${location.host}/api/realtime`;

      try {
        ws = new WebSocket(url);
      } catch {
        scheduleReconnect();
        return;
      }

      ws.onopen = () => {
        backoffMs = BACKOFF_INITIAL_MS;
        clearPing();
        pingTimer = setInterval(() => {
          if (ws?.readyState === WebSocket.OPEN) {
            ws.send('ping');
          }
        }, PING_INTERVAL_MS);
        if (shouldInvalidateCalendarOnOpen()) {
          void catchUpCalendarQueries(queryClient);
        }
      };

      ws.onmessage = (event: MessageEvent) => {
        if (typeof event.data !== 'string') return;
        // Server auto-replies "pong" to "ping"; ignore non-JSON frames.
        const msg = parseRealtimeMessage(event.data);
        if (shouldInvalidateCalendarOnMessage(msg)) {
          void catchUpCalendarQueries(queryClient);
        }
      };

      ws.onclose = () => {
        clearPing();
        ws = null;
        if (!stopped) {
          scheduleReconnect();
        }
      };

      ws.onerror = () => {
        // Browsers fire onclose after onerror; close to ensure cleanup path.
        ws?.close();
      };
    };

    connect();

    return () => {
      stopped = true;
      clearReconnect();
      clearPing();
      if (ws) {
        ws.onopen = null;
        ws.onmessage = null;
        ws.onclose = null;
        ws.onerror = null;
        ws.close();
        ws = null;
      }
    };
  }, [user, isLoading, queryClient]);
}
