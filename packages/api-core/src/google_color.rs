//! Google Calendar event color mapping.
//!
//! Maps a CSS hex color to the nearest Google Calendar event `colorId`
//! (`"1"`..=`"11"`) by squared Euclidean RGB distance against the compiled-in
//! `colors.get` **event.background** palette. Pure std-only Rust — no I/O, no
//! `worker` dependency — so it unit-tests natively (`cargo test -p api-core`)
//! and stays allocation-light on `wasm32-unknown-unknown`.
//!
//! The math mirrors the journal's `closestGoogleColorId` (`mcp-gcal-journal`
//! `src/auth/client.ts`): `dist = (r1-r2)^2 + (g1-g2)^2 + (b1-b2)^2`, no sqrt,
//! no Lab. Squares are computed in `i32` so `u8` differences cannot overflow.

use thiserror::Error;

/// Google event color backgrounds, id 1..=11, in scan order.
///
/// Source of truth: Google Calendar API `colors.get` `event.background`
/// values (the actual event fills — not the darker UI picker names). The
/// journal reads the same hexes from `colorDef.background`. Scan order is
/// id 1 then 2 … 11, and the first minimum distance wins, so ties resolve
/// to the lowest id.
pub const GOOGLE_EVENT_COLORS: [(u8, &str); 11] = [
    (1, "#a4bdfc"),
    (2, "#7ae7bf"),
    (3, "#dbadff"),
    (4, "#ff887c"),
    (5, "#fbd75b"),
    (6, "#ffb878"),
    (7, "#46d6db"),
    (8, "#e1e1e1"),
    (9, "#5484ed"),
    (10, "#51b749"),
    (11, "#dc2127"),
];

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

/// Nearest Google event colorId (`"1"`..=`"11"`) for a hex color.
///
/// Distance is squared Euclidean RGB, computed in `i32` so the squares cannot
/// overflow `u8`: `dist = (r1-r2)^2 + (g1-g2)^2 + (b1-b2)^2`. The palette is
/// scanned in id order and only a *strictly* smaller distance replaces the
/// current best, so ties keep the lowest id. Returns
/// [`HexColorError::Invalid`] if the hex does not parse.
pub fn closest_google_color_id(hex: &str) -> Result<String, HexColorError> {
    let (r1, g1, b1) = parse_hex_rgb(hex)?;
    let mut best_id = GOOGLE_EVENT_COLORS[0].0;
    let mut best_dist = i32::MAX;
    for &(id, bg) in &GOOGLE_EVENT_COLORS {
        // Palette hexes are statically valid `#rrggbb`.
        let (r2, g2, b2) = parse_hex_rgb(bg).expect("palette hexes are valid");
        let dr = r1 as i32 - r2 as i32;
        let dg = g1 as i32 - g2 as i32;
        let db = b1 as i32 - b2 as i32;
        let dist = dr * dr + dg * dg + db * db;
        if dist < best_dist {
            best_dist = dist;
            best_id = id;
        }
    }
    Ok(best_id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_palette_hex_maps_to_its_own_id() {
        for (id, bg) in GOOGLE_EVENT_COLORS {
            // Distance 0 to itself; a later id at distance 0 would be a tie
            // broken by scan order, so this also proves first-min wins.
            assert_eq!(closest_google_color_id(bg).unwrap(), id.to_string(), "{bg}");
        }
    }

    #[test]
    fn palette_scan_is_case_insensitive() {
        assert_eq!(closest_google_color_id("#A4BDFC").unwrap(), "1");
        assert_eq!(closest_google_color_id("#DC2127").unwrap(), "11");
    }

    #[test]
    fn journals_work_blue_maps_to_id_9() {
        // mcp-gcal-journal's Work blue.
        assert_eq!(closest_google_color_id("#4285F4").unwrap(), "9");
        // Sanctuary's Work seed color.
        assert_eq!(closest_google_color_id("#2a5c8a").unwrap(), "9");
    }

    #[test]
    fn shorthand_expands_by_doubling_nibbles() {
        assert_eq!(parse_hex_rgb("#abc").unwrap(), parse_hex_rgb("#aabbcc").unwrap());
        assert_eq!(parse_hex_rgb("#abc").unwrap(), (0xaa, 0xbb, 0xcc));
        assert_eq!(
            closest_google_color_id("#abc").unwrap(),
            closest_google_color_id("#aabbcc").unwrap()
        );
        assert_eq!(parse_hex_rgb("#fff").unwrap(), (0xff, 0xff, 0xff));
        assert_eq!(parse_hex_rgb("#000").unwrap(), (0x00, 0x00, 0x00));
    }

    #[test]
    fn surrounding_whitespace_is_trimmed() {
        assert_eq!(parse_hex_rgb("  #2a5c8a  ").unwrap(), (0x2a, 0x5c, 0x8a));
        assert_eq!(closest_google_color_id("  #2a5c8a  ").unwrap(), "9");
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
            assert_eq!(
                closest_google_color_id(bad),
                Err(HexColorError::Invalid),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn error_displays_a_helpful_message() {
        assert_eq!(
            HexColorError::Invalid.to_string(),
            "color must be #rgb or #rrggbb"
        );
    }

    #[test]
    fn extremes_parse_and_land_on_some_palette_id() {
        for hex in ["#000", "#000000", "#fff", "#ffffff"] {
            let id = closest_google_color_id(hex).unwrap();
            assert!(
                (1..=11).contains(&id.parse::<u8>().unwrap()),
                "{hex} -> {id}"
            );
        }
    }
}
