// Notion Calendar event chip colors: a self-contained CIECAM02 JCh port that
// derives washed chip tones from whatever hex the caller passes.
// Viewing conditions match d3-cam02's defaults (D65, La = 64/π/5, Yb = 20).

interface Rgb {
  r: number;
  g: number;
  b: number;
}

interface Xyz {
  x: number;
  y: number;
  z: number;
}

interface Lms {
  l: number;
  m: number;
  s: number;
}

interface Hpe {
  lh: number;
  mh: number;
  sh: number;
}

interface Jch {
  J: number;
  C: number;
  h: number;
}

// ── Viewing conditions ──────────────────────────────────────────────────

const D65 = { X: 95.047, Y: 100, Z: 108.883 } as const;
const LA = 64 / Math.PI / 5;
const YB = 20;
const SURROUND_C = 0.69;
const SURROUND_NC = 1;
const F = 1;

const N = YB / D65.Y;
const Z = 1.48 + Math.sqrt(N);
const FL_T = 1 / (5 * LA + 1);
const FL =
  0.2 * FL_T ** 4 * (5 * LA) + 0.1 * (1 - FL_T ** 4) ** 2 * (5 * LA) ** (1 / 3);
const NBB = 0.725 * (1 / N) ** 0.2;
const NCB = NBB;
const D = F * (1 - (1 / 3.6) * Math.exp((-LA - 42) / 92));

/** sRGB (0..1) → linear before the XYZ matrix. */
function srgbToLinear(c: number): number {
  return c > 0.04045 ? ((c + 0.055) / 1.055) ** 2.4 : c / 12.92;
}

/** Linear (0..1) → sRGB 0..255. */
function linearToSrgb(c: number): number {
  return 255 * (c <= 0.0031308 ? 12.92 * c : 1.055 * c ** (1 / 2.4) - 0.055);
}

function srgbToXyz(r: number, g: number, b: number): Xyz {
  const R = srgbToLinear(r / 255);
  const G = srgbToLinear(g / 255);
  const B = srgbToLinear(b / 255);
  return {
    x: (R * 0.4124 + G * 0.3576 + B * 0.1805) * 100,
    y: (R * 0.2126 + G * 0.7152 + B * 0.0722) * 100,
    z: (R * 0.0193 + G * 0.1192 + B * 0.9505) * 100,
  };
}

function xyzToSrgb(x: number, y: number, z: number): Rgb {
  const X = x / 100;
  const Y = y / 100;
  const Zc = z / 100;
  const r = X * 3.2404542 + Y * -1.5371385 - Zc * 0.4985314;
  const g = X * -0.969266 + Y * 1.8760108 + Zc * 0.041556;
  const b = X * 0.0556434 + Y * -0.2040259 + Zc * 1.0572252;
  return { r: linearToSrgb(r), g: linearToSrgb(g), b: linearToSrgb(b) };
}

/** XYZ → CAM02 chromatic-adaptation LMS. */
function xyzToCat02(x: number, y: number, z: number): Lms {
  return {
    l: 0.7328 * x + 0.4296 * y - 0.1624 * z,
    m: -0.7036 * x + 1.6975 * y + 0.0061 * z,
    s: 0.003 * x + 0.0136 * y + 0.9834 * z,
  };
}

/** CAM02 cone responses → HPE (Hunt-Pointer-Estevez) responses. */
function lmsToHpe(l: number, m: number, s: number): Hpe {
  return {
    lh: 0.7409792 * l + 0.218025 * m + 0.0410058 * s,
    mh: 0.2853532 * l + 0.6242014 * m + 0.0904454 * s,
    sh: -0.009628 * l - 0.005698 * m + 1.015326 * s,
  };
}

/** CAT02 LMS → XYZ. */
function cat02ToXyz(l: number, m: number, s: number): Xyz {
  return {
    x: 1.096124 * l - 0.278869 * m + 0.182745 * s,
    y: 0.454369 * l + 0.473533 * m + 0.072098 * s,
    z: -0.009628 * l - 0.005698 * m + 1.015326 * s,
  };
}

/** HPE responses → XYZ. */
function hpeToXyz(l: number, m: number, s: number): Xyz {
  return {
    x: 1.910197 * l - 1.112124 * m + 0.201908 * s,
    y: 0.37095 * l + 0.629054 * m - 0.000008 * s,
    z: s,
  };
}

/** CAM02 nonlinear compression. */
function cam02Compress(value: number, fl: number): number {
  const n = ((fl * value) / 100) ** 0.42;
  return (400 * n) / (27.13 + n) + 0.1;
}

/** Inverse CAM02 compression. */
function cam02Expand(value: number, fl: number): number {
  return (
    (100 / fl) *
    ((27.13 * Math.abs(value - 0.1)) / (400 - Math.abs(value - 0.1))) **
      (1 / 0.42)
  );
}

/** Achromatic response + opponent axes → compressed cone responses. */
function labToLmsPrime(achromatic: number, a: number, b: number): Lms {
  const aw = achromatic / NBB + 0.305;
  return {
    l: 0.32787 * aw + 0.32145 * a + 0.20527 * b,
    m: 0.32787 * aw - 0.63507 * a - 0.18603 * b,
    s: 0.32787 * aw - 0.15681 * a - 4.49038 * b,
  };
}

/** Von Kries chromatic adaptation of one cone response to the white point. */
function adaptToWhite(value: number, whiteLms: number): number {
  return value * ((D65.Y * D) / whiteLms + (1 - D));
}

const D65_LMS = xyzToCat02(D65.X, D65.Y, D65.Z);

const ACHROMATIC_RESPONSE_TO_WHITE = (() => {
  const hpe = lmsToHpe(
    adaptToWhite(D65_LMS.l, D65_LMS.l),
    adaptToWhite(D65_LMS.m, D65_LMS.m),
    adaptToWhite(D65_LMS.s, D65_LMS.s),
  );
  const l = cam02Compress(hpe.lh, FL);
  const m = cam02Compress(hpe.mh, FL);
  const s = cam02Compress(hpe.sh, FL);
  return (2 * l + m + 0.05 * s - 0.305) * NBB;
})();

// ── CAM02 LMS → JCh ─────────────────────────────────────────────────────

function cat02ToJch(l: number, m: number, s: number): Jch {
  const hpe = lmsToHpe(
    adaptToWhite(l, D65_LMS.l),
    adaptToWhite(m, D65_LMS.m),
    adaptToWhite(s, D65_LMS.s),
  );
  const lp = cam02Compress(hpe.lh, FL);
  const mp = cam02Compress(hpe.mh, FL);
  const sp = cam02Compress(hpe.sh, FL);
  const a = lp - (12 * mp) / 11 + sp / 11;
  const b = (1 / 9) * (lp + mp - 2 * sp);
  let h = (180 / Math.PI) * Math.atan2(b, a);
  if (h < 0) h += 360;
  const J =
    100 *
    (((2 * lp + mp + 0.05 * sp - 0.305) * NBB) /
      ACHROMATIC_RESPONSE_TO_WHITE) **
      (SURROUND_C * Z);
  const g = 0.25 * (Math.cos((h * Math.PI) / 180 + 2) + 3.8);
  const C =
    (((5e4 / 13) * SURROUND_NC * NCB * g * Math.sqrt(a * a + b * b)) /
      (lp + mp + (21 / 20) * sp)) **
      0.9 *
    Math.sqrt(J / 100) *
    (1.64 - 0.29 ** N) ** 0.73;
  return { J, C, h };
}

/** `#rgb` / `#rrggbb` → 0..255 channels. Invalid → null. */
function parseHexRgb(hex: string): Rgb | null {
  const h = hex.replace('#', '');
  const full =
    h.length === 3
      ? h
          .split('')
          .map((c) => c + c)
          .join('')
      : h;
  if (!/^[0-9a-fA-F]{6}$/.test(full)) return null;
  const n = parseInt(full, 16);
  return {
    r: (n >> 16) & 255,
    g: (n >> 8) & 255,
    b: n & 255,
  };
}

/** Canonical lowercase `#rrggbb` cache key. Invalid → '#000000'. */
function normalizeHex(hex: string): string {
  const rgb = parseHexRgb(hex.trim());
  if (!rgb) return '#000000';
  const n = (rgb.r << 16) | (rgb.g << 8) | rgb.b;
  return `#${n.toString(16).padStart(6, '0')}`;
}

function hexToJch(hex: string): Jch {
  const rgb = parseHexRgb(hex) ?? { r: 0, g: 0, b: 0 };
  const xyz = srgbToXyz(rgb.r, rgb.g, rgb.b);
  const lms = xyzToCat02(xyz.x, xyz.y, xyz.z);
  return cat02ToJch(lms.l, lms.m, lms.s);
}

// ── JCh → CSS rgb ───────────────────────────────────────────────────────

/** d3's channel clamp: NaN rounds to 0 rather than leaking into CSS. */
function clampChannel(value: number): number {
  return Math.max(0, Math.min(255, Math.round(value) || 0));
}

function formatRgb(rgb: Rgb): string {
  return `rgb(${clampChannel(rgb.r)}, ${clampChannel(rgb.g)}, ${clampChannel(rgb.b)})`;
}

function jchToRgb(J: number, C: number, h: number): Rgb {
  const j = Number.isFinite(J) ? J : 0;
  let c = Number.isFinite(C) && C >= 0 ? C : 0;
  let hue = Number.isFinite(h) ? h : 0;
  // A NaN hue cannot be inverted; land on the achromatic axis instead.
  if (!Number.isFinite(h)) {
    c = 0;
    hue = 0;
  }

  const t =
    (c / (Math.sqrt(j / 100) * (1.64 - 0.29 ** N) ** 0.73)) ** (1 / 0.9);
  const g = 0.25 * (Math.cos((hue * Math.PI) / 180 + 2) + 3.8);
  const achromatic =
    (j / 100) ** (1 / (SURROUND_C * Z)) * ACHROMATIC_RESPONSE_TO_WHITE;
  const o = ((5e4 / 13) * SURROUND_NC * NCB * g) / t;
  const s = achromatic / NBB + 0.305;
  const ce = 21 / 20;
  const rad = (hue * Math.PI) / 180;
  let a: number;
  let b: number;
  if (Math.abs(Math.sin(rad)) >= Math.abs(Math.cos(rad))) {
    const p4 = o / Math.sin(rad);
    b =
      (s * (2 + ce) * (460 / 1403)) /
      (p4 +
        (2 + ce) * (220 / 1403) * (Math.cos(rad) / Math.sin(rad)) -
        27 / 1403 +
        (6300 / 1403) * ce);
    a = b * (Math.cos(rad) / Math.sin(rad));
  } else {
    const p5 = o / Math.cos(rad);
    a =
      (s * (2 + ce) * (460 / 1403)) /
      (p5 +
        (2 + ce) * (220 / 1403) -
        (27 / 1403 - (6300 / 1403) * ce) * (Math.sin(rad) / Math.cos(rad)));
    b = a * (Math.sin(rad) / Math.cos(rad));
  }

  const lmsPrime = labToLmsPrime(achromatic, a, b);
  const expandedXyz = hpeToXyz(
    cam02Expand(lmsPrime.l, FL),
    cam02Expand(lmsPrime.m, FL),
    cam02Expand(lmsPrime.s, FL),
  );
  const adapted = xyzToCat02(expandedXyz.x, expandedXyz.y, expandedXyz.z);
  const xyz = cat02ToXyz(
    adapted.l / ((D65.Y * D) / D65_LMS.l + (1 - D)),
    adapted.m / ((D65.Y * D) / D65_LMS.m + (1 - D)),
    adapted.s / ((D65.Y * D) / D65_LMS.s + (1 - D)),
  );
  return xyzToSrgb(xyz.x, xyz.y, xyz.z);
}

/** J may exceed 100 (dark primary passes J * 1.03); the channel clamp fixes overflow. */
function jchCss(J: number, C: number, h: number): string {
  return formatRgb(jchToRgb(J, C, h));
}

// ── Accent ink ──────────────────────────────────────────────────────────

/** sRGB channel 0..1 → linear contribution for relative luminance. */
function srgbToLinearLuminance(c: number): number {
  return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
}

/** Relative luminance of a `rgb(r, g, b)` string produced above. */
function cssLuminance(css: string): number {
  const match = /^rgb\((\d+), (\d+), (\d+)\)$/.exec(css);
  if (!match) return 0;
  const r = srgbToLinearLuminance(Number(match[1]) / 255);
  const g = srgbToLinearLuminance(Number(match[2]) / 255);
  const b = srgbToLinearLuminance(Number(match[3]) / 255);
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

// ── Theme derivation ────────────────────────────────────────────────────

export interface NotionTone {
  backgroundPrimary: string;
  backgroundPrimaryDimmed: string;
  backgroundSecondary: string;
  backgroundSecondaryDimmed: string;
  foregroundPrimary: string;
  foregroundPrimaryDimmed: string;
  foregroundSecondary: string;
  foregroundSecondaryDimmed: string;
  accentInk: '#fafafa' | '#1a1a1a';
}

export interface NotionEventTheme {
  light: NotionTone;
  dark: NotionTone;
}

export interface NotionEventCssVars {
  '--ev-fill': string;
  '--ev-fill-dark': string;
  '--ev-accent': string;
  '--ev-accent-dark': string;
  '--ev-accent-dim': string;
  '--ev-accent-dim-dark': string;
  '--ev-title': string;
  '--ev-title-dark': string;
  '--ev-title-dim': string;
  '--ev-title-dim-dark': string;
  '--ev-time': string;
  '--ev-time-dark': string;
  '--ev-time-dim': string;
  '--ev-time-dim-dark': string;
  '--ev-ink': string;
  '--ev-ink-dark': string;
}

function toneFromJch(jch: Jch, dark: boolean): NotionTone {
  const chromatic = jch.C >= 5;
  const backgroundPrimary = dark
    ? jchCss(jch.J * 1.03, jch.C, jch.h)
    : jchCss(jch.J * 0.97, jch.C * 1.1, jch.h);
  // Light dims are ours, not Notion's (84 / 60 / 70 washed past chips out):
  // past light chips sit just a step below active ones.
  const backgroundPrimaryDimmed = dark
    ? jchCss(25, jch.C * 0.5, jch.h)
    : jchCss(65, jch.C * 0.85, jch.h);
  const backgroundSecondary = jchCss(dark ? 20 : 95, jch.C * 0.4, jch.h);
  const foregroundPrimary = dark
    ? jchCss(100, chromatic ? 30 : jch.C, jch.h)
    : jchCss(30, chromatic ? 70 : jch.C, jch.h);
  const foregroundPrimaryDimmed = dark
    ? jchCss(45, chromatic ? 15 : jch.C, jch.h)
    : jchCss(40, chromatic ? 55 : jch.C, jch.h);
  const foregroundSecondary = dark
    ? jchCss(55, chromatic ? 35 : jch.C, jch.h)
    : jchCss(35, chromatic ? 60 : jch.C, jch.h);
  const foregroundSecondaryDimmed = dark
    ? jchCss(35, chromatic ? 20 : jch.C, jch.h)
    : jchCss(48, chromatic ? 50 : jch.C, jch.h);
  return {
    backgroundPrimary,
    backgroundPrimaryDimmed,
    backgroundSecondary,
    backgroundSecondaryDimmed: backgroundSecondary,
    foregroundPrimary,
    foregroundPrimaryDimmed,
    foregroundSecondary,
    foregroundSecondaryDimmed,
    accentInk: cssLuminance(backgroundPrimary) < 0.55 ? '#fafafa' : '#1a1a1a',
  };
}

const themeCache = new Map<string, NotionEventTheme>();
const cssVarsCache = new Map<string, NotionEventCssVars>();

/** Both themes. Cached by normalized hex, so the same input returns the same object. */
export function notionEventTheme(hex: string): NotionEventTheme {
  const key = normalizeHex(hex);
  const cached = themeCache.get(key);
  if (cached) return cached;
  const jch = hexToJch(key);
  const theme: NotionEventTheme = {
    light: toneFromJch(jch, false),
    dark: toneFromJch(jch, true),
  };
  themeCache.set(key, theme);
  return theme;
}

/** Vars for both themes. Cached alongside the theme (same hex → same object). */
export function notionEventCssVars(hex: string): NotionEventCssVars {
  const key = normalizeHex(hex);
  const cached = cssVarsCache.get(key);
  if (cached) return cached;
  const { light, dark } = notionEventTheme(key);
  const vars: NotionEventCssVars = {
    '--ev-fill': light.backgroundSecondary,
    '--ev-fill-dark': dark.backgroundSecondary,
    '--ev-accent': light.backgroundPrimary,
    '--ev-accent-dark': dark.backgroundPrimary,
    '--ev-accent-dim': light.backgroundPrimaryDimmed,
    '--ev-accent-dim-dark': dark.backgroundPrimaryDimmed,
    '--ev-title': light.foregroundPrimary,
    '--ev-title-dark': dark.foregroundPrimary,
    '--ev-title-dim': light.foregroundPrimaryDimmed,
    '--ev-title-dim-dark': dark.foregroundPrimaryDimmed,
    '--ev-time': light.foregroundSecondary,
    '--ev-time-dark': dark.foregroundSecondary,
    '--ev-time-dim': light.foregroundSecondaryDimmed,
    '--ev-time-dim-dark': dark.foregroundSecondaryDimmed,
    '--ev-ink': light.accentInk,
    '--ev-ink-dark': dark.accentInk,
  };
  cssVarsCache.set(key, vars);
  return vars;
}

/** True iff both instants are finite and endMs <= nowMs. */
export function isEventPast(endMs: number, nowMs: number): boolean {
  return Number.isFinite(endMs) && Number.isFinite(nowMs) && endMs <= nowMs;
}
