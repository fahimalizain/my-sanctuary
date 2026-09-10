// Unit tests for calendar hour-density zoom pure helpers. No React / DOM.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  clampHourHeight,
  hourHAfterZoom,
  hourLabelStep,
  parseStoredHourH,
  scrollDeltaForHourZoom,
  yInHoursArea,
  zoomFactorFromWheel,
} from './calendar-zoom';

// ── clampHourHeight ─────────────────────────────────────────────────────

test('clampHourHeight: 48 stays 48', () => {
  assert.equal(clampHourHeight(48), 48);
});

test('clampHourHeight: below min clamps to 16', () => {
  assert.equal(clampHourHeight(10), 16);
});

test('clampHourHeight: above max clamps to 208', () => {
  assert.equal(clampHourHeight(300), 208);
});

test('clampHourHeight: NaN / Infinity → 48 (base)', () => {
  assert.equal(clampHourHeight(NaN), 48);
  assert.equal(clampHourHeight(Infinity), 48);
  assert.equal(clampHourHeight(-Infinity), 48);
});

// ── hourHAfterZoom ──────────────────────────────────────────────────────

test('hourHAfterZoom: 48 * 2 → 96', () => {
  assert.equal(hourHAfterZoom(48, 2), 96);
});

test('hourHAfterZoom: 48 * 10 clamps to 208', () => {
  assert.equal(hourHAfterZoom(48, 10), 208);
});

test('hourHAfterZoom: 48 * 0.1 clamps to 16', () => {
  assert.equal(hourHAfterZoom(48, 0.1), 16);
});

test('hourHAfterZoom: factor 0 / NaN → clamp(current)', () => {
  assert.equal(hourHAfterZoom(48, 0), 48);
  assert.equal(hourHAfterZoom(10, 0), 16);
  assert.equal(hourHAfterZoom(48, NaN), 48);
  assert.equal(hourHAfterZoom(300, NaN), 208);
});

// ── scrollDeltaForHourZoom ──────────────────────────────────────────────

test('scrollDeltaForHourZoom: zoom in doubles scroll under cursor', () => {
  // old=48, new=96, y=240 → 240 * (2 - 1) = 240
  assert.equal(scrollDeltaForHourZoom(48, 96, 240), 240);
});

test('scrollDeltaForHourZoom: zoom out halves scroll under cursor', () => {
  // old=96, new=48, y=240 → 240 * (0.5 - 1) = -120
  assert.equal(scrollDeltaForHourZoom(96, 48, 240), -120);
});

test('scrollDeltaForHourZoom: bad inputs → 0', () => {
  assert.equal(scrollDeltaForHourZoom(0, 96, 240), 0);
  assert.equal(scrollDeltaForHourZoom(48, 0, 240), 0);
  assert.equal(scrollDeltaForHourZoom(-1, 96, 240), 0);
  assert.equal(scrollDeltaForHourZoom(48, NaN, 240), 0);
  assert.equal(scrollDeltaForHourZoom(48, 96, NaN), 0);
  assert.equal(scrollDeltaForHourZoom(NaN, 96, 240), 0);
});

// ── hourLabelStep ───────────────────────────────────────────────────────

test('hourLabelStep: dense labels at base height', () => {
  assert.equal(hourLabelStep(48), 1);
  assert.equal(hourLabelStep(28), 1);
});

test('hourLabelStep: every 2 hours at medium zoom-out', () => {
  assert.equal(hourLabelStep(20), 2);
  assert.equal(hourLabelStep(27), 2);
});

test('hourLabelStep: every 3 hours when very zoomed out', () => {
  assert.equal(hourLabelStep(16), 3);
  assert.equal(hourLabelStep(19), 3);
});

// ── zoomFactorFromWheel ─────────────────────────────────────────────────

test('zoomFactorFromWheel: negative deltaY zooms in', () => {
  const f = zoomFactorFromWheel(-100, 0);
  assert.ok(f > 1, `expected factor > 1, got ${f}`);
  assert.ok(Math.abs(f - Math.exp(0.3)) < 1e-9);
  // ~1.35
  assert.ok(f > 1.34 && f < 1.36);
});

test('zoomFactorFromWheel: positive deltaY zooms out', () => {
  const f = zoomFactorFromWheel(100, 0);
  assert.ok(f < 1, `expected factor < 1, got ${f}`);
  assert.ok(Math.abs(f - Math.exp(-0.3)) < 1e-9);
  // ~0.74
  assert.ok(f > 0.73 && f < 0.75);
});

test('zoomFactorFromWheel: deltaMode 1 scales by 16 (line)', () => {
  const f = zoomFactorFromWheel(1, 1);
  assert.ok(Math.abs(f - Math.exp(-16 * 0.003)) < 1e-9);
});

test('zoomFactorFromWheel: deltaMode 2 scales by 400 (page)', () => {
  const f = zoomFactorFromWheel(1, 2);
  assert.ok(Math.abs(f - Math.exp(-400 * 0.003)) < 1e-9);
});

// ── parseStoredHourH ────────────────────────────────────────────────────

test('parseStoredHourH: null / empty / non-numeric → null', () => {
  assert.equal(parseStoredHourH(null), null);
  assert.equal(parseStoredHourH(''), null);
  assert.equal(parseStoredHourH('nope'), null);
});

test('parseStoredHourH: valid number is clamped', () => {
  assert.equal(parseStoredHourH('48'), 48);
  assert.equal(parseStoredHourH('10'), 16);
  assert.equal(parseStoredHourH('300'), 208);
});

// ── yInHoursArea ────────────────────────────────────────────────────────

test('yInHoursArea: clientY relative to scrolled hours area', () => {
  // clientY=200, rectTop=100, scrollTop=50, header=40 → 110
  assert.equal(yInHoursArea(200, 100, 50, 40), 110);
});
