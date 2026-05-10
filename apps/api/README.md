# API Environment Variables

| Variable                  | Required | Default                 | Description                                                                               |
| ------------------------- | -------- | ----------------------- | ----------------------------------------------------------------------------------------- |
| `GOOGLE_CREDENTIALS_JSON` | Yes      | —                       | Raw JSON content of your Google OAuth 2.0 client credentials file (web application type). |
| `SESSION_SECRET`          | Yes      | —                       | 32+ byte secret used to encrypt session cookies.                                          |
| `FRONTEND_URL`            | No       | `http://localhost:5173` | URL of the web frontend. Used to redirect users after login.                              |
| `SECURE_COOKIE`           | No       | `false`                 | Set to `true` in production to mark cookies as Secure.                                    |

## Endpoints

| Method | Path                    | Description                               |
| ------ | ----------------------- | ----------------------------------------- |
| GET    | `/health`               | Health check                              |
| GET    | `/greeting/{name}`      | Example greeting endpoint                 |
| GET    | `/auth/google`          | Initiate Google OAuth login               |
| GET    | `/auth/google/callback` | OAuth callback (redirect from Google)     |
| GET    | `/auth/me`              | Returns current user or `null`            |
| POST   | `/auth/logout`          | Clears session cookie                     |
| GET    | `/api/calendar/events`  | List next 10 events from primary calendar |
| POST   | `/api/calendar/events`  | Create a new event in primary calendar    |

## Scopes Requested

- `openid`
- `email`
- `profile`
- `https://www.googleapis.com/auth/calendar`
