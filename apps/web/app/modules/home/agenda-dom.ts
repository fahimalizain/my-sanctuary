// The DOM ids HomePage's scroll/Show lookups and the day/warehouse sections
// share: each stream day and each backlog day is painted under one.

import {
  BACKLOG_DAY_ID_PREFIX,
  DAY_ID_PREFIX,
  WAITING_DAY_ID_PREFIX,
} from './agenda-helpers';

export function dayElementId(date: string): string {
  return `${DAY_ID_PREFIX}${date}`;
}

export function waitingDayElementId(date: string): string {
  return `${WAITING_DAY_ID_PREFIX}${date}`;
}

export function backlogDayElementId(date: string): string {
  return `${BACKLOG_DAY_ID_PREFIX}${date}`;
}
