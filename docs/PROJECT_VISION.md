# Sanctuary — Project Vision

## Core Concept

Sanctuary is a life-organisation app for people juggling multiple domains (family, work, personal startup, fitness, etc.). It helps you prioritise, time-block, and stay focused — with **Google Calendar as the source of truth** for all scheduling.

---

## Architecture

### Streams

A **Stream** represents a major area of your life:

- Family
- Work
- Personal Startup
- Fitness
- (any other domain you define)

Each stream has its own set of tasks.

### Tasks

Tasks live **inside** a stream. Each task has:

- Title / description
- Estimated duration
- Priority
- Optional: due date, recurrence, notes

### Time Blocking (Home Page)

The **Home page** is a **single-day timeline view** with a **non-linear, content-density-driven layout**:

```
┌──────────────────────────────────────────────┐
│  My Streams ▼              │  [Focus Timer]   │
│                            │                  │
│  9 AM ┌──────────────────┐ │  25:00 timer     │
│       │ Work Block         │ │  [Start Focus]   │
│       │ 9 AM - 12 PM       │ │                  │
│  10   │ ┌────────────────┐ │ │  Sessions: 3     │
│       │ │ Review Q3 Rpt  │ │ │                  │
│  11   │ │ Team Standup   │ │ │                  │
│       │ │ Update Dash... │ │ │                  │
│  12   │ │ Design API...  │ │ │                  │
│       │ │ ... 4 more     │ │ │                  │
│       │ └────────────────┘ │ │                  │
│       └──────────────────┘ │                  │
│       ┌──────────────────┐ │                  │
│  2 PM │ Gym Time           │ │                  │
│       │ 12 PM - 2 PM       │ │                  │
│       │ Strength Training  │ │                  │
│       └──────────────────┘ │                  │
│       ┌──────────────────┐ │                  │
│  4 PM │ Family Dinner    │ │                  │
│       │ 2 PM - 4 PM        │ │                  │
│       │ Pick up grocer...│ │                  │
│       │ Cook dinner        │ │                  │
│       └──────────────────┘ │                  │
│                            │                  │
└──────────────────────────────────────────────┘
```

**Non-linear timeline principles:**

- **Block height driven by task density**, not clock duration
  - Single-task blocks are compact (just stream title + task name)
  - Multi-task blocks expand to show all tasks inline
- **Skewed time axis** — hour labels spaced according to block heights, not uniform 60-minute intervals
- **Each stream has a distinct color** — blocks are color-coded by stream for visual recognition
- **Right panel**: Focus timer (Pomodoro-style) with session tracking
- Tasks displayed with: title, estimated duration, priority indicator

### Task Scheduling Flow

1. Define **Streams**
2. Create **Tasks** inside each stream
3. On the timeline, **block off time** for a stream (e.g. "Family, 6-8 PM")
4. **Drag tasks** from the stream into the blocked time slot
5. Everything syncs to **Google Calendar** as events

### Task Postponement Tracking

Tasks/events that get postponed (rescheduled to another day or week) are tracked:

- Each time a task's date is moved, a **postponement counter** increments
- The original creation date and all reschedule dates are recorded
- Visible on the task card: "Postponed 4 times · Originally created Mar 2"
- Helps identify tasks that keep getting pushed vs. ones that actually get done
- Reschedule reason can be optionally recorded (e.g. "urgent work came up", "too tired")

---

## Consistency & Rules

Sanctuary tracks how consistent you are with your personal commitments by letting you define **custom rules** that are checked against your calendar and task data.

### Custom Rules

A rule is a personal commitment you define with:

- **Name**: e.g. "Wake up early"
- **Condition**: e.g. "Wake up between 5:00 AM and 6:00 AM"
- **Schedule**: e.g. "Every day" or "Weekdays only"
- **Data source**: which calendar event or task to check (e.g. an event titled "Wake up" or "Morning Routine")

### How Rules Are Evaluated

1. You define a rule with a condition and a target event/task
2. Sanctuary scans Google Calendar to check if the event happened within the expected window
3. Results are shown as a **consistency streak** and **percentage**

### Consistency Dashboard

Each rule shows:

- **Streak**: current consecutive days met
- **Longest streak**: all-time best
- **Compliance %**: e.g. "82% this month" (25/30 days)
- **History**: a heatmap or calendar view showing green (met) / red (missed) days
- **Rolling trends**: 7-day, 30-day, 90-day compliance

### Example Rules

| Rule               | Condition                                       | Schedule |
| ------------------ | ----------------------------------------------- | -------- |
| Wake up early      | "Wake up" event starts between 5:00–6:00 AM     | Daily    |
| Morning run        | "Run" or "Morning run" event exists before 8 AM | Weekdays |
| No work after 8 PM | No event tagged "Work" starts after 8:00 PM     | Daily    |
| Read before bed    | "Reading" block exists after 9 PM               | Daily    |
| Gym 3×/week        | At least 3 "Gym/Fitness" events per week        | Weekly   |

---

## Google Calendar Integration

Google Calendar is the **source of truth** for all scheduling:

- Time blocks created in Sanctuary appear as events in Google Calendar
- Events created directly in Google Calendar are pulled into Sanctuary
- Two-way sync keeps everything consistent
- Calendar availability is respected when suggesting time blocks
- All scheduling lives in Google Calendar — Sanctuary is the planning layer on top
- **Event reschedule tracking**: when a user moves an event, Sanctuary detects the date change and increments a postponement counter
- **Rule evaluation**: consistency rules scan Google Calendar history to check if target events met their conditions (e.g. "Wake up" started within the 5–6 AM window)

---

## Screens / Pages

| Screen          | Purpose                                                   |
| --------------- | --------------------------------------------------------- |
| **Login**       | Sign in with Google (only auth method)                    |
| **Home**        | Single-day timeline + focus timer + quotes slideshow      |
| **Streams**     | Manage streams and their tasks                            |
| **Calendar**    | Full week/month view (Google Calendar-powered)            |
| **Consistency** | Custom rules, habit tracking, streaks, compliance history |
| **Settings**    | Profile, preferences, integrations                        |

## Design Language

- **Background**: `#f5f5ef` light cream
- **Primary accent**: `#204f0a` dark green
- **Fonts**: Sen (headings), Kumbh Sans (body)
- **Clean, minimal, warm** aesthetic
