//! Google Calendar event-label hex handling.
//!
//! Strict CSS hex parsing and canonicalization ([`parse_hex_rgb`],
//! [`canonicalize_hex`], [`is_event_label_hex`]) plus
//! [`snap_to_event_label_hex`], which maps any hex onto the 24 default
//! event-label hexes Google seeds on owned calendars
//! ([`GOOGLE_EVENT_LABEL_COLORS`]).
//!
//! That palette is a third set — it matches neither `colors.get` `.event`
//! (11) nor `.calendar` (24). The snap is chroma-first CIE76 ΔE in CIE Lab
//! (D65): sources below [`NEUTRAL_CHROMA_THRESHOLD`] chroma snap only among
//! [`GOOGLE_EVENT_LABEL_NEUTRALS`], so near-neutral colors stay neutral
//! instead of landing on a saturated label. Pure std-only Rust — no I/O, no
//! `worker` dependency — so it unit-tests natively (`cargo test -p api-core`)
//! and stays allocation-light on `wasm32-unknown-unknown`.

use thiserror::Error;

/// Errors produced while parsing a hex color.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HexColorError {
    /// The string is not a strict `#rgb` / `#rrggbb` hex color.
    #[error("color must be #rgb or #rrggbb")]
    Invalid,
}

/// Parses a strict CSS hex into an `(r, g, b)` tuple.
///
/// Optional surrounding whitespace is trimmed; then a required `#`, then 3 or
/// 6 hex digits (case-insensitive). `#rgb` expands by doubling nibbles
/// (`#abc` == `#aabbcc`). Everything else is rejected — missing `#`, 4, 5, 7
/// or 8 digits, `rgb()`, color names.
pub fn parse_hex_rgb(hex: &str) -> Result<(u8, u8, u8), HexColorError> {
    let digits = hex
        .trim()
        .strip_prefix('#')
        .ok_or(HexColorError::Invalid)?;
    let bytes = digits.as_bytes();
    match bytes.len() {
        3 => {
            let r = hex_nibble(bytes[0])?;
            let g = hex_nibble(bytes[1])?;
            let b = hex_nibble(bytes[2])?;
            Ok((r * 17, g * 17, b * 17))
        }
        6 => {
            let r = (hex_nibble(bytes[0])? << 4) | hex_nibble(bytes[1])?;
            let g = (hex_nibble(bytes[2])? << 4) | hex_nibble(bytes[3])?;
            let b = (hex_nibble(bytes[4])? << 4) | hex_nibble(bytes[5])?;
            Ok((r, g, b))
        }
        _ => Err(HexColorError::Invalid),
    }
}

/// Decodes one ASCII hex digit (`0-9`, `a-f`, `A-F`).
fn hex_nibble(byte: u8) -> Result<u8, HexColorError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(HexColorError::Invalid),
    }
}

/// The 24 default Google event-label backgrounds, as seeded on owned
/// calendars.
///
/// Order is stable (scan order for ties). Names are comments only.
///
/// Source of truth: the `eventLabels` Google seeds on every owned calendar
/// (verified live). These hexes are a third set — they match neither
/// `colors.get` `.event` (11) nor `.calendar` (24). All hexes are stored and
/// returned as lowercase `#rrggbb`.
pub const GOOGLE_EVENT_LABEL_COLORS: [&str; 24] = [
    "#009688", // eucalyptus
    "#039be5", // peacock
    "#0b8043", // basil
    "#33b679", // sage
    "#3f51b5", // blueberry
    "#4285f4", // cobalt
    "#616161", // graphite
    "#795548", // cocoa
    "#7986cb", // lavender
    "#7cb342", // pistachio
    "#8e24aa", // grape
    "#9e69af", // amethyst
    "#a79b8e", // birch
    "#ad1457", // radicchio
    "#b39ddb", // wisteria
    "#c0ca33", // avocado
    "#d50000", // tomato
    "#d81b60", // cherry blossom
    "#e4c441", // citron
    "#e67c73", // flamingo
    "#ef6c00", // pumpkin
    "#f09300", // mango
    "#f4511e", // tangerine
    "#f6bf26", // banana
];

/// Neutral subset used when source chroma is below the threshold.
pub const GOOGLE_EVENT_LABEL_NEUTRALS: [&str; 3] = [
    "#616161", // graphite
    "#795548", // cocoa
    "#a79b8e", // birch
];

/// Category/list create-dialog default (peacock).
pub const DEFAULT_EVENT_LABEL_COLOR: &str = "#039be5";

/// C* below this → snap only among [`GOOGLE_EVENT_LABEL_NEUTRALS`].
pub const NEUTRAL_CHROMA_THRESHOLD: f64 = 15.0;

/// Canonicalizes a hex color to lowercase `#rrggbb` (shorthand expands).
///
/// Surrounding whitespace is trimmed and parsing goes through
/// [`parse_hex_rgb`], so `#abc`, `#ABC`, and `"  #AbC  "` all become
/// `"#aabbcc"`.
pub fn canonicalize_hex(hex: &str) -> Result<String, HexColorError> {
    let (r, g, b) = parse_hex_rgb(hex)?;
    Ok(format!("#{r:02x}{g:02x}{b:02x}"))
}

/// True iff `hex` canonicalizes to one of [`GOOGLE_EVENT_LABEL_COLORS`].
///
/// Case and surrounding whitespace are ignored (the hex is canonicalized
/// first); any hex not among the 24 — even if it parses — is `false`.
pub fn is_event_label_hex(hex: &str) -> bool {
    canonicalize_hex(hex)
        .is_ok_and(|c| GOOGLE_EVENT_LABEL_COLORS.contains(&c.as_str()))
}

/// Snaps a hex color to the nearest [`GOOGLE_EVENT_LABEL_COLORS`] entry.
///
/// Chroma-first CIE76 ΔE in CIE Lab (D65), pure std `f64`:
///
/// 1. canonicalize (invalid hex → [`HexColorError::Invalid`]);
/// 2. if the canonical hex is already one of the 24, return it (identity);
/// 3. convert source sRGB → CIE Lab (D65); `C* = hypot(a*, b*)`;
/// 4. if `C* < NEUTRAL_CHROMA_THRESHOLD`, candidates are only
///    [`GOOGLE_EVENT_LABEL_NEUTRALS`];
/// 5. pick the minimum CIE76 ΔE (`sqrt(dL² + da² + db²)`); strictly-smaller
///    wins, ties keep the earlier palette/neutral scan order.
///
/// The motivating case: `#535050` (charcoal, C*≈1.3) is ΔE76 ≈ 7.0 from
/// graphite `#616161` — RGB-nearest against the old 11-color palette maps it
/// to green (id 10), but the chroma-first snap keeps it neutral.
pub fn snap_to_event_label_hex(hex: &str) -> Result<String, HexColorError> {
    let canonical = canonicalize_hex(hex)?;
    if GOOGLE_EVENT_LABEL_COLORS.contains(&canonical.as_str()) {
        return Ok(canonical);
    }
    let (r, g, b) = parse_hex_rgb(&canonical).expect("canonical hex parses");
    let (l, a_star, b_star) = rgb_to_lab(r, g, b);
    let chroma = a_star.hypot(b_star);
    let candidates: &[&str] = if chroma < NEUTRAL_CHROMA_THRESHOLD {
        &GOOGLE_EVENT_LABEL_NEUTRALS
    } else {
        &GOOGLE_EVENT_LABEL_COLORS
    };
    let mut best = candidates[0];
    let mut best_dist = f64::INFINITY;
    for &candidate in candidates {
        // Palette hexes are statically valid `#rrggbb`.
        let (cr, cg, cb) = parse_hex_rgb(candidate).expect("palette hexes are valid");
        let (cl, ca, cb_star) = rgb_to_lab(cr, cg, cb);
        let dl = l - cl;
        let da = a_star - ca;
        let db = b_star - cb_star;
        let dist = (dl * dl + da * da + db * db).sqrt();
        if dist < best_dist {
            best_dist = dist;
            best = candidate;
        }
    }
    Ok(best.to_string())
}

/// Converts one sRGB channel (u8) to linear light.
fn srgb_to_linear(channel: u8) -> f64 {
    let c = channel as f64 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// CIE Lab `f(t)` helper: cube root above `(6/29)^3`, linear tail below.
fn lab_f(t: f64) -> f64 {
    const DELTA: f64 = 6.0 / 29.0;
    const DELTA_CUBED: f64 = DELTA * DELTA * DELTA;
    if t > DELTA_CUBED {
        t.powf(1.0 / 3.0)
    } else {
        t / (3.0 * DELTA * DELTA) + 4.0 / 29.0
    }
}

/// Converts an sRGB `(r, g, b)` tuple to CIE Lab under the D65 white point.
fn rgb_to_lab(r: u8, g: u8, b: u8) -> (f64, f64, f64) {
    let r = srgb_to_linear(r);
    let g = srgb_to_linear(g);
    let b = srgb_to_linear(b);
    // sRGB → XYZ (D65), IEC 61966-2-1 matrix.
    let x = 0.4124564 * r + 0.3575761 * g + 0.1804375 * b;
    let y = 0.2126729 * r + 0.7151522 * g + 0.0721750 * b;
    let z = 0.0193339 * r + 0.1191920 * g + 0.9503041 * b;
    // D65 white point: Xn = 0.95047, Yn = 1.0, Zn = 1.08883.
    let fx = lab_f(x / 0.95047);
    let fy = lab_f(y / 1.0);
    let fz = lab_f(z / 1.08883);
    let l_star = 116.0 * fy - 16.0;
    let a_star = 500.0 * (fx - fy);
    let b_star = 200.0 * (fy - fz);
    (l_star, a_star, b_star)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shorthand_expands_by_doubling_nibbles() {
        assert_eq!(parse_hex_rgb("#abc").unwrap(), parse_hex_rgb("#aabbcc").unwrap());
        assert_eq!(parse_hex_rgb("#abc").unwrap(), (0xaa, 0xbb, 0xcc));
        assert_eq!(parse_hex_rgb("#fff").unwrap(), (0xff, 0xff, 0xff));
        assert_eq!(parse_hex_rgb("#000").unwrap(), (0x00, 0x00, 0x00));
    }

    #[test]
    fn surrounding_whitespace_is_trimmed() {
        assert_eq!(parse_hex_rgb("  #2a5c8a  ").unwrap(), (0x2a, 0x5c, 0x8a));
    }

    #[test]
    fn rejects_non_hex_strings() {
        for bad in [
            "",
            "2a5c8a",      // missing #
            "#",           // no digits
            "#gg0000",     // non-hex digit
            "#12345",      // 5 digits
            "#1234567",    // 7 digits
            "#12345678",   // 8 digits
            "rgb(1,2,3)",  // rgb() form
            "blue",        // color name
            "#ffff",       // 4 digits
        ] {
            assert_eq!(parse_hex_rgb(bad), Err(HexColorError::Invalid), "{bad:?}");
        }
    }

    #[test]
    fn error_displays_a_helpful_message() {
        assert_eq!(
            HexColorError::Invalid.to_string(),
            "color must be #rgb or #rrggbb"
        );
    }

    // --- GOOGLE_EVENT_LABEL_COLORS + chroma-first snap ---

    #[test]
    fn each_event_label_snaps_to_itself_in_any_case_and_whitespace() {
        for hex in GOOGLE_EVENT_LABEL_COLORS {
            assert_eq!(snap_to_event_label_hex(hex).unwrap(), hex, "{hex}");
            let upper = format!("#{}", hex[1..].to_uppercase());
            assert_eq!(snap_to_event_label_hex(&upper).unwrap(), hex, "{upper}");
            let padded = format!("  {hex}  ");
            assert_eq!(snap_to_event_label_hex(&padded).unwrap(), hex, "{padded:?}");
        }
    }

    #[test]
    fn is_event_label_hex_matches_exact_palette_membership() {
        for hex in GOOGLE_EVENT_LABEL_COLORS {
            assert!(is_event_label_hex(hex), "{hex}");
            let upper = format!("#{}", hex[1..].to_uppercase());
            assert!(is_event_label_hex(&upper), "{upper}");
            let padded = format!("  {hex}  ");
            assert!(is_event_label_hex(&padded), "{padded:?}");
        }
        // Valid hexes that are not among the 24 event labels.
        for not_label in ["#535050", "#e1e1e1", "#2a5c8a", "", "535050"] {
            assert!(!is_event_label_hex(not_label), "{not_label:?}");
        }
    }

    #[test]
    fn canonicalize_expands_shorthand_and_trims() {
        assert_eq!(canonicalize_hex("#ABC").unwrap(), "#aabbcc");
        assert_eq!(canonicalize_hex("#abc").unwrap(), "#aabbcc");
        assert_eq!(canonicalize_hex("  #616161  ").unwrap(), "#616161");
    }

    #[test]
    fn divine_premiere_charcoal_snaps_to_graphite_not_basil() {
        // Motivating case: RGB-nearest against the old 11-color palette maps
        // #535050 to green (id 10); the chroma-first snap must keep neutrals
        // neutral. ΔE76(#535050, #616161) ≈ 7.0, C* ≈ 1.3 < threshold.
        assert_eq!(snap_to_event_label_hex("#535050").unwrap(), "#616161");
    }

    #[test]
    fn old_event_gray_snaps_to_birch() {
        // #e1e1e1 is the old colors.get event gray, C* ≈ 0 → neutral
        // candidates only; birch (#a79b8e) wins on ΔE76.
        assert_eq!(snap_to_event_label_hex("#e1e1e1").unwrap(), "#a79b8e");
    }

    #[test]
    fn achromatic_extremes_stay_neutral() {
        assert_eq!(snap_to_event_label_hex("#000000").unwrap(), "#616161");
        assert_eq!(snap_to_event_label_hex("#ffffff").unwrap(), "#a79b8e");
        assert_eq!(snap_to_event_label_hex("#3a3a3a").unwrap(), "#616161");
    }

    #[test]
    fn chromatic_blue_snaps_to_lavender_not_a_neutral() {
        // C* ≈ 30.5 ≥ threshold → full 24 palette; lavender (#7986cb) is the
        // ΔE76 winner, so chromatic sources do not fall back to neutrals.
        assert_eq!(snap_to_event_label_hex("#2a5c8a").unwrap(), "#7986cb");
    }

    #[test]
    fn uppercase_palette_hex_snaps_to_itself() {
        // #4285F4 is in the palette, so identity wins before any Lab math.
        assert_eq!(snap_to_event_label_hex("#4285F4").unwrap(), "#4285f4");
    }

    #[test]
    fn snap_rejects_non_hex_strings() {
        for bad in [
            "",
            "2a5c8a",      // missing #
            "#",           // no digits
            "#gg0000",     // non-hex digit
            "#12345",      // 5 digits
            "#1234567",    // 7 digits
            "#12345678",   // 8 digits
            "rgb(1,2,3)",  // rgb() form
            "blue",        // color name
            "#ffff",       // 4 digits
        ] {
            assert_eq!(
                snap_to_event_label_hex(bad),
                Err(HexColorError::Invalid),
                "{bad:?}"
            );
            assert_eq!(
                canonicalize_hex(bad),
                Err(HexColorError::Invalid),
                "{bad:?}"
            );
        }
    }
}
