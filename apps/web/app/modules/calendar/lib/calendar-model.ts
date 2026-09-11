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
  lastOccupiedCivilDate,
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
  }[] = [];

  for (const event of allDayEvents) {
    let first: Date;
    let last: Date;

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
    } else {
      // Multi-day timed events in the all-day band: local Date conversion.
      const start = new Date(event.start_time);
      const end = new Date(event.end_time);
      first = startOfDay(start);
      last = lastOccupiedCivilDate(start, end);
    }

    let startDay = Math.round((first.getTime() - origin.getTime()) / msPerDay);
    let endDay = Math.round((last.getTime() - origin.getTime()) / msPerDay);

    // No overlap with rendered window [0, dayCount).
    if (endDay < 0 || startDay > lastIdx) continue;
    startDay = Math.max(0, Math.min(lastIdx, startDay));
    endDay = Math.max(0, Math.min(lastIdx, endDay));
    if (endDay < startDay) continue;

    inputs.push({ id: event.id, startDay, endDay, event });
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

    map.set(key, positioned);
  }

  return map;
}
