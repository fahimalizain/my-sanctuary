// Unit tests for calendar hour-density zoom pure helpers. No React / DOM.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  anchorHoursAt,
  clampHourHeight,
  hourHAfterZoom,
  hourLabelStep,
  parseStoredHourH,
  pinchDistance,
  pinchMidpointY,
  pinchScale,
  scrollTopForAnchor,
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

// ── anchorHoursAt ───────────────────────────────────────────────────────

test('anchorHoursAt: time under the pointer at the painted hourH', () => {
  // clientY=200, rectTop=100, scrollTop=50, header=40 → y=110; 110/48 hours
  assert.equal(anchorHoursAt(200, 100, 50, 40, 48), 110 / 48);
  // Same point at double the scale → half the hours.
  assert.equal(anchorHoursAt(200, 100, 50, 40, 96), 110 / 96);
});

test('anchorHoursAt: above the hours area is a negative time', () => {
  // Pointer over the sticky headers/all-day band: y=110-140=-30.
  assert.equal(anchorHoursAt(110, 100, 0, 40, 48), -30 / 48);
});

test('anchorHoursAt: bad inputs → 0', () => {
  assert.equal(anchorHoursAt(200, 100, 50, 40, 0), 0);
  assert.equal(anchorHoursAt(200, 100, 50, 40, -48), 0);
  assert.equal(anchorHoursAt(200, 100, 50, 40, NaN), 0);
  assert.equal(anchorHoursAt(NaN, 100, 50, 40, 48), 0);
  assert.equal(anchorHoursAt(200, NaN, 50, 40, 48), 0);
  assert.equal(anchorHoursAt(200, 100, NaN, 40, 48), 0);
  assert.equal(anchorHoursAt(200, 100, 50, NaN, 48), 0);
  assert.equal(anchorHoursAt(200, 100, Infinity, 40, 48), 0);
});

// ── scrollTopForAnchor ──────────────────────────────────────────────────

test('scrollTopForAnchor: keeps the anchored time under the same offset', () => {
  // Zoom 48 → 96 under a pointer 100px into the scroller: the anchored time
  // scales with the grid, so scrollTop grows with it.
  // 40 + (110/48) * 96 - 100 = 160
  assert.equal(scrollTopForAnchor(110 / 48, 96, 40, 100), 160);
  // Round trip: same pointer, new hourH, scrollTop from the helper → same time.
  const scrollTop = scrollTopForAnchor(110 / 48, 96, 40, 100);
  assert.equal(anchorHoursAt(200, 100, scrollTop, 40, 96), 110 / 48);
});

test('scrollTopForAnchor: clamps at 0', () => {
  // Zoomed out far above the anchor: the browser cannot scroll past the top.
  assert.equal(scrollTopForAnchor(0, 48, 40, 200), 0);
});

test('scrollTopForAnchor: bad inputs → 0', () => {
  assert.equal(scrollTopForAnchor(NaN, 48, 40, 100), 0);
  assert.equal(scrollTopForAnchor(1, NaN, 40, 100), 0);
  assert.equal(scrollTopForAnchor(1, 48, NaN, 100), 0);
  assert.equal(scrollTopForAnchor(1, 48, 40, NaN), 0);
  assert.equal(scrollTopForAnchor(1, Infinity, 40, 100), 0);
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

// ── pinchDistance ───────────────────────────────────────────────────────

test('pinchDistance: 3-4-5 triangle', () => {
  assert.equal(pinchDistance({ x: 0, y: 0 }, { x: 3, y: 4 }), 5);
});

test('pinchDistance: non-finite coords → 0', () => {
  assert.equal(pinchDistance({ x: NaN, y: 0 }, { x: 3, y: 4 }), 0);
  assert.equal(pinchDistance({ x: 0, y: 0 }, { x: Infinity, y: 4 }), 0);
});

// ── pinchMidpointY ──────────────────────────────────────────────────────

test('pinchMidpointY: average of two Ys', () => {
  assert.equal(pinchMidpointY({ y: 100 }, { y: 200 }), 150);
});

test('pinchMidpointY: non-finite → 0', () => {
  assert.equal(pinchMidpointY({ y: NaN }, { y: 200 }), 0);
  assert.equal(pinchMidpointY({ y: 100 }, { y: Infinity }), 0);
});

// ── pinchScale ──────────────────────────────────────────────────────────

test('pinchScale: 100→200 = 2; 100→50 = 0.5', () => {
  assert.equal(pinchScale(100, 200), 2);
  assert.equal(pinchScale(100, 50), 0.5);
});

test('pinchScale: origin 0 / negative / NaN → 1', () => {
  assert.equal(pinchScale(0, 100), 1);
  assert.equal(pinchScale(-10, 100), 1);
  assert.equal(pinchScale(NaN, 100), 1);
  assert.equal(pinchScale(100, 0), 1);
  assert.equal(pinchScale(100, -5), 1);
  assert.equal(pinchScale(100, NaN), 1);
});
