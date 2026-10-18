// Pure calendar page helpers: chip color, click-create range, day labels,
// and packing of all-day / timed events into positioned chips.
// No React. Deterministic. Native Date only.

import type { CalendarEvent } from '@/app/types';
import type { AllDayChip } from '../components/AllDayRow';
import { toTimedRange, type DragSlot, type TimedRange } from './calendar-drag';
import type { PositionedEvent } from '../components/EventChip';
import {
  WEEK_DAYS,
  addDays,
  allDaySectionHeight,
  civilDateFromAllDayIso,
  clampMinutesToDay,
  colorForCalendar,
  eventHeightPx,
  eventTopPx,
  isSameDay,
  lastOccupiedCivilDate,
  minutesSinceMidnight,
  packAllDayLanes,
  packDayEvents,
  startOfDay,
} from './week-layout';

/** Mon-based short name for a local date (WEEK_DAYS is Mon→Sun). */
export function dayNameShort(date: Date): string {
  const jsDay = date.getDay(); // 0 = Sun … 6 = Sat
  const monIndex = jsDay === 0 ? 6 : jsDay - 1;
  return WEEK_DAYS[monIndex];
}

/**
 * Click-to-create range from a snapped slot: default 30 min, kept inside the day.
 */
export function clickCreateTimesFromSlot(slot: DragSlot): TimedRange {
  return toTimedRange(slot);
}

/** Keyboard create lands on the next half-hour, not on the 1-minute grid. */
const KEYBOARD_CREATE_STEP_MIN = 30;

/** Create slot on the first visible day when today is off-screen. */
const KEYBOARD_CREATE_FALLBACK_MIN = 9 * 60;

export interface KeyboardCreateRangeInput {
  now: Date;
  visibleStart: Date;
  periodLength: number;
}

/**
 * Timed create slot for the bare `C` shortcut.
 *
 * When the now-line's civil day is visible, start on the next 30-minute
 * boundary (exact boundaries stay put) and reuse `toTimedRange`, so a late
 * ceil such as 23:50 → 24:00 is pulled back to 23:30–24:00 instead of
 * spilling into the next day. Otherwise the first visible day at 09:00.
 * Invalid dates fall back so a malformed clock can never throw or produce
 * NaN instants.
 */
export function keyboardCreateRange({
  now,
  visibleStart,
  periodLength,
}: KeyboardCreateRangeInput): TimedRange {
  if (!Number.isFinite(visibleStart.getTime())) {
    return toTimedRange({
      day: new Date(0),
      minutes: KEYBOARD_CREATE_FALLBACK_MIN,
    });
  }
  if (!Number.isFinite(now.getTime())) {
    return toTimedRange({
      day: visibleStart,
      minutes: KEYBOARD_CREATE_FALLBACK_MIN,
    });
  }

  let todayVisible = false;
  const dayCount = Number.isFinite(periodLength) ? Math.trunc(periodLength) : 0;
  for (let i = 0; i < dayCount; i += 1) {
    if (isSameDay(addDays(visibleStart, i), now)) {
      todayVisible = true;
      break;
    }
  }

  if (!todayVisible) {
    return toTimedRange({
      day: visibleStart,
      minutes: KEYBOARD_CREATE_FALLBACK_MIN,
    });
  }

  // Seconds count: 10:30:00 is exact, 10:30:01 rolls to 11:00.
  const minutes =
    Math.ceil(minutesSinceMidnight(now) / KEYBOARD_CREATE_STEP_MIN) *
    KEYBOARD_CREATE_STEP_MIN;
  return toTimedRange({ day: now, minutes });
}

/** Category color from the API when present; otherwise hash the calendar id. */
export function eventChipColor(event: CalendarEvent): string {
  const fromApi = event.color?.trim();
  if (fromApi) return fromApi;
  return colorForCalendar(event.calendar_id || event.id);
}

/**
 * Pack all-day events into lane chips for the rendered strip window.
 * Returns chips plus the band height driven by the highest occupied lane.
 */
export function buildAllDayChips(
  days: Date[],
  allDayEvents: CalendarEvent[],
  dayCount: number,
): { allDayChips: AllDayChip[]; allDayHeight: number } {
  const origin = days[0];
  if (!origin) {
    return {
      allDayChips: [] as AllDayChip[],
      allDayHeight: allDaySectionHeight(null),
    };
  }

  const msPerDay = 24 * 60 * 60 * 1000;
  const lastIdx = dayCount - 1;
  const inputs: {
    id: string;
    startDay: number;
    endDay: number;
    event: CalendarEvent;
    endMs: number;
  }[] = [];

  for (const event of allDayEvents) {
    let first: Date;
    let last: Date;
    let endMs: number;

    if (event.is_all_day) {
      // Stored as UTC midnight of the civil date — use the ISO prefix, not
      // local Date(iso) which shifts US zones to the previous evening.
      const startCivil = civilDateFromAllDayIso(event.start_time);
      const endCivil = civilDateFromAllDayIso(event.end_time);
      if (!startCivil || !endCivil) continue;
      first = startCivil;
      // Google all-day end is exclusive: last occupied = end − 1 day.
      last =
        endCivil.getTime() > startCivil.getTime()
          ? addDays(endCivil, -1)
          : startCivil;
      // Past at local midnight after the last occupied day, not UTC midnight.
      endMs = addDays(last, 1).getTime();
    } else {
      // Multi-day timed events in the all-day band: local Date conversion.
      const start = new Date(event.start_time);
      const end = new Date(event.end_time);
      first = startOfDay(start);
      last = lastOccupiedCivilDate(start, end);
      endMs = end.getTime();
    }

    let startDay = Math.round((first.getTime() - origin.getTime()) / msPerDay);
    let endDay = Math.round((last.getTime() - origin.getTime()) / msPerDay);

    // No overlap with rendered window [0, dayCount).
    if (endDay < 0 || startDay > lastIdx) continue;
    startDay = Math.max(0, Math.min(lastIdx, startDay));
    endDay = Math.max(0, Math.min(lastIdx, endDay));
    if (endDay < startDay) continue;

    inputs.push({ id: event.id, startDay, endDay, event, endMs });
  }

  const packed = packAllDayLanes(
    inputs.map((i) => ({
      id: i.id,
      startDay: i.startDay,
      endDay: i.endDay,
    })),
  );
  const laneById = new Map(packed.map((p) => [p.id, p.lane]));

  let maxLane: number | null = null;
  const chips: AllDayChip[] = inputs.map((i) => {
    const lane = laneById.get(i.id) ?? 0;
    if (maxLane === null || lane > maxLane) maxLane = lane;
    return {
      id: i.id,
      title: i.event.title,
      startDay: i.startDay,
      endDay: i.endDay,
      lane,
      color: eventChipColor(i.event),
      endMs: i.endMs,
    };
  });

  return {
    allDayChips: chips,
    allDayHeight: allDaySectionHeight(maxLane),
  };
}

/**
 * Position timed events per day column (multi-day events excluded upstream).
 * Map keys are `day.toDateString()`.
 */
export function buildEventsByDay(
  days: Date[],
  timedEvents: CalendarEvent[],
  hourH: number,
): Map<string, PositionedEvent[]> {
  const map = new Map<string, PositionedEvent[]>();

  for (const day of days) {
    const key = day.toDateString();
    const dayItems: {
      event: CalendarEvent;
      startMin: number;
      endMin: number;
    }[] = [];

    for (const event of timedEvents) {
      const start = new Date(event.start_time);
      const end = new Date(event.end_time);
      const clamped = clampMinutesToDay(start, end, day);
      if (!clamped) continue;
      dayItems.push({
        event,
        startMin: clamped.startMin,
        endMin: clamped.endMin,
      });
    }

    const packed = packDayEvents(
      dayItems.map((d) => ({
        id: d.event.id,
        startMin: d.startMin,
        endMin: d.endMin,
      })),
    );
    const packById = new Map(packed.map((p) => [p.id, p]));

    const positioned: PositionedEvent[] = dayItems.map((d) => {
      const pack = packById.get(d.event.id)!;
      return {
        event: d.event,
        startMin: d.startMin,
        endMin: d.endMin,
        top: eventTopPx(d.startMin, hourH),
        height: eventHeightPx(d.startMin, d.endMin, hourH),
        leftPercent: pack.leftPercent,
        widthPercent: pack.widthPercent,
        layerIndex: pack.layerIndex,
        leftPixels: pack.leftPixels,
        color: eventChipColor(d.event),
      };
    });

    // Paint order only — never a pack tie-break (the pack input above keeps its
    // incoming order). Equal `layerIndex` stacks by DOM order, so a
    // later-starting chip must sit later in the array to cover the
    // CHIP_MIN_H tail of an earlier, shorter chip. Ties keep their order.
    positioned.sort((a, b) =>
      a.startMin !== b.startMin ? a.startMin - b.startMin : 0,
    );

    map.set(key, positioned);
  }

  return map;
}
