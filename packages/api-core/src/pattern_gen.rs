//! Pure regex "hole" fill + extract helpers.
//!
//! A **hole** is an unbounded repetition of a dot — a `.*` / `.+` HIR
//! [`Repetition`] with `max == None` whose sub-expression is
//! `Dot::AnyCharExceptLF` (default `.`) or `Dot::AnyChar` (`(?s:.)`). Dots
//! inside other constructs — `[.*]`, `\.*`, `. *` — are **not** holes.
//!
//! * [`fill_regex`] fills the first hole with a caller-supplied `X` and emits
//!   a member of `L(pattern)` that always matches.
//! * [`split_hole`] / [`extract_hole`] recover the spans around and inside the
//!   first hole from a matching string — no capture groups.
//! * [`emit_affixes`] emits the canonical prefix/suffix "chrome" around the
//!   first hole (used for empty-title + locked category chrome).

use regex::Regex;
use regex_syntax::hir::{Class, Dot, Hir, HirKind, Literal, Repetition};
use regex_syntax::parse;
use thiserror::Error;

/// Errors produced while filling a pattern hole ([`fill_regex`],
/// [`emit_affixes`]).
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum FillError {
    #[error("invalid regex: {0}")]
    InvalidRegex(String),
    #[error("X contains \\n but the hole is default `.`")]
    NewlineInHole,
    #[error("character class is empty — language is empty")]
    EmptyClass,
    #[error("literal is not valid UTF-8")]
    BadLiteral,
}

/// Errors produced while extracting a pattern hole ([`split_hole`],
/// [`extract_hole`]).
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum ExtractError {
    #[error("invalid regex: {0}")]
    InvalidRegex(String),
    #[error("string is not in L(R)")]
    NoMatch,
    #[error("pattern has no .* / .+ hole")]
    NoHole,
}

/// The actual slices around the first hole when `s` matches `pattern`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoleSplit {
    /// Everything before the span the first hole consumed.
    pub prefix: String,
    /// The span the first hole consumed from `s`.
    pub hole: String,
    /// Everything after the span the first hole consumed.
    pub suffix: String,
}

/// Fill the first `.*` / `.+` hole in `pattern` with `x`.
///
/// If `x` already matches `pattern`, it is returned unchanged (identity).
/// Otherwise the first hole consumes `x` (`\n` is rejected unless the hole is
/// `(?s:.)`), later holes take their minimum (`""` for `.*`, `"x"` for `.+`),
/// and every other repetition emits `min` copies. The result always matches
/// `pattern`.
pub fn fill_regex(pattern: &str, x: &str) -> Result<String, FillError> {
    let compiled = Regex::new(pattern).map_err(|e| FillError::InvalidRegex(e.to_string()))?;
    if compiled.is_match(x) {
        return Ok(x.to_string());
    }

    let hir = parse(pattern).map_err(|e| FillError::InvalidRegex(e.to_string()))?;
    let mut used_hole = false;
    let generated = emit(&hir, x, &mut used_hole)?;
    debug_assert!(compiled.is_match(&generated));
    Ok(generated)
}

/// Emit a minimal member of `L(hir)`, filling the first hole with `x`.
fn emit(hir: &Hir, x: &str, used_hole: &mut bool) -> Result<String, FillError> {
    match hir.kind() {
        HirKind::Empty | HirKind::Look(_) => Ok(String::new()),
        HirKind::Literal(Literal(bytes)) => {
            String::from_utf8(bytes.to_vec()).map_err(|_| FillError::BadLiteral)
        }
        HirKind::Class(class) => emit_class(class),
        HirKind::Capture(cap) => emit(&cap.sub, x, used_hole),
        HirKind::Concat(subs) => {
            let mut out = String::new();
            for sub in subs {
                out.push_str(&emit(sub, x, used_hole)?);
            }
            Ok(out)
        }
        HirKind::Alternation(subs) => emit(&subs[0], x, used_hole),
        HirKind::Repetition(rep) if is_hole(rep) => emit_hole(rep, x, used_hole),
        HirKind::Repetition(rep) => {
            let mut out = String::new();
            for _ in 0..rep.min {
                out.push_str(&emit(&rep.sub, x, used_hole)?);
            }
            Ok(out)
        }
    }
}

/// Whether `rep` is a hole: an unbounded repetition of a dot.
fn is_hole(rep: &Repetition) -> bool {
    rep.max.is_none() && is_dot(&rep.sub)
}

/// Whether `hir` is a bare dot (default `.` or `(?s:.)`).
fn is_dot(hir: &Hir) -> bool {
    *hir == Hir::dot(Dot::AnyCharExceptLF) || *hir == Hir::dot(Dot::AnyChar)
}

/// Whether `hir` is a hole repetition node.
fn hir_is_hole(hir: &Hir) -> bool {
    matches!(hir.kind(), HirKind::Repetition(rep) if is_hole(rep))
}

/// Emit the value a hole contributes: `x` for the first hole, its minimum
/// (`""` / `"x"`) for any later hole.
fn emit_hole(rep: &Repetition, x: &str, used_hole: &mut bool) -> Result<String, FillError> {
    let dot_allows_newline = *rep.sub == Hir::dot(Dot::AnyChar);
    if x.contains('\n') && !dot_allows_newline {
        return Err(FillError::NewlineInHole);
    }
    if !*used_hole {
        *used_hole = true;
        if x.is_empty() && rep.min >= 1 {
            return Ok("x".to_string());
        }
        return Ok(x.to_string());
    }
    if rep.min == 0 {
        Ok(String::new())
    } else {
        Ok("x".to_string())
    }
}

/// Emit the first range start of a character class (always a valid member).
fn emit_class(class: &Class) -> Result<String, FillError> {
    match class {
        Class::Unicode(u) => {
            let ch = u.ranges().first().ok_or(FillError::EmptyClass)?.start();
            Ok(ch.to_string())
        }
        Class::Bytes(b) => {
            let byte = b.ranges().first().ok_or(FillError::EmptyClass)?.start();
            Ok((byte as char).to_string())
        }
    }
}

/// Split `s` at the span the first hole actually consumed.
///
/// `s` must be in `L(pattern)`. The first hole is the first `.*` / `.+`
/// sibling in the flattened HIR. A greedy hole takes the rightmost legal cut,
/// an ungreedy hole the leftmost. Capture groups are never consulted.
pub fn split_hole(pattern: &str, s: &str) -> Result<HoleSplit, ExtractError> {
    let compiled = Regex::new(pattern).map_err(|e| ExtractError::InvalidRegex(e.to_string()))?;
    if !compiled.is_match(s) {
        return Err(ExtractError::NoMatch);
    }

    let hir = parse(pattern).map_err(|e| ExtractError::InvalidRegex(e.to_string()))?;
    let flat = flatten(&hir);
    let idx = flat
        .iter()
        .position(hir_is_hole)
        .ok_or(ExtractError::NoHole)?;

    let greedy = match flat[idx].kind() {
        HirKind::Repetition(rep) => rep.greedy,
        _ => true,
    };
    let min = match flat[idx].kind() {
        HirKind::Repetition(rep) => rep.min as usize,
        _ => 0,
    };

    let prefix_re = compile_anchored_prefix(&nodes_to_pattern(&flat[..idx]))
        .map_err(|e| ExtractError::InvalidRegex(e.to_string()))?;
    let suffix_re = compile_anchored_suffix(&nodes_to_pattern(&flat[idx + 1..]))
        .map_err(|e| ExtractError::InvalidRegex(e.to_string()))?;

    let prefix_end = prefix_re.find(s).ok_or(ExtractError::NoMatch)?.end();

    let mut found = None;
    for j in prefix_end..=s.len() {
        if !s.is_char_boundary(j) {
            continue;
        }
        if j - prefix_end < min {
            continue;
        }
        if suffix_re.is_match(&s[j..]) {
            found = Some(j);
            if !greedy {
                break;
            }
        }
    }

    let j = found.ok_or(ExtractError::NoMatch)?;
    Ok(HoleSplit {
        prefix: s[..prefix_end].to_string(),
        hole: s[prefix_end..j].to_string(),
        suffix: s[j..].to_string(),
    })
}

/// The substring that the first `.*` / `.+` hole consumed from a matching `s`.
pub fn extract_hole(pattern: &str, s: &str) -> Result<String, ExtractError> {
    Ok(split_hole(pattern, s)?.hole)
}

/// Canonical (emit) prefix/suffix around the first hole-bearing pattern.
///
/// `None` if the pattern has no `.*` / `.+` hole. The affixes use minimum
/// fills for every hole (`""` for `.*`, `"x"` for `.+`) — i.e. the "chrome"
/// that wraps a title filled into the first hole.
pub fn emit_affixes(pattern: &str) -> Result<Option<(String, String)>, FillError> {
    let hir = parse(pattern).map_err(|e| FillError::InvalidRegex(e.to_string()))?;
    let flat = flatten(&hir);
    let idx = match flat.iter().position(hir_is_hole) {
        Some(idx) => idx,
        None => return Ok(None),
    };

    // `used_hole = true` disables the `X` fill: every hole in the affixes
    // (they come after the first hole) emits its minimum.
    let mut used = true;
    let prefix = emit_nodes(&flat[..idx], "", &mut used)?;
    let mut used = true;
    let suffix = emit_nodes(&flat[idx + 1..], "", &mut used)?;
    Ok(Some((prefix, suffix)))
}

/// Emit a flat slice of HIR nodes (used by [`emit_affixes`]).
fn emit_nodes(nodes: &[Hir], x: &str, used_hole: &mut bool) -> Result<String, FillError> {
    match nodes {
        [] => Ok(String::new()),
        nodes => emit(&Hir::concat(nodes.to_vec()), x, used_hole),
    }
}

/// Unwrap `Concat` and `Capture` so the first hole is a sibling in a flat list.
fn flatten(hir: &Hir) -> Vec<Hir> {
    match hir.kind() {
        HirKind::Concat(subs) => subs.iter().flat_map(flatten).collect(),
        HirKind::Capture(cap) => flatten(&cap.sub),
        _ => vec![hir.clone()],
    }
}

/// Rebuild regex source for a flat slice of HIR nodes.
fn nodes_to_pattern(nodes: &[Hir]) -> String {
    match nodes {
        [] => String::new(),
        [one] => one.to_string(),
        many => Hir::concat(many.to_vec()).to_string(),
    }
}

/// Compile `\A(?:{src})` so the prefix only matches at the start of `s`.
fn compile_anchored_prefix(src: &str) -> Result<Regex, regex::Error> {
    if src.is_empty() {
        Regex::new(r"\A")
    } else {
        Regex::new(&format!(r"\A(?:{src})"))
    }
}

/// Compile `\A(?:{src})\z` so the suffix matches exactly the tail of `s`.
fn compile_anchored_suffix(src: &str) -> Result<Regex, regex::Error> {
    if src.is_empty() {
        Regex::new(r"\A\z")
    } else {
        Regex::new(&format!(r"\A(?:{src})\z"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The canonical category pattern: a hole followed by literal chrome and a
    /// two-way alternation. Both spellings must be accepted; filling emits the
    /// first alternative.
    const SPICY: &str = r"^.* [|] (Spicy Home|SpicyHome)$";

    #[test]
    fn fill_spicy_home() {
        assert_eq!(
            fill_regex(SPICY, "Review PR").unwrap(),
            "Review PR | Spicy Home"
        );
    }

    #[test]
    fn fill_identity_when_already_matching() {
        let s = "Review PR | SpicyHome";
        assert_eq!(fill_regex(SPICY, s).unwrap(), s);
    }

    #[test]
    fn fill_no_hole_emits_a_member() {
        assert_eq!(fill_regex(r"^Work$", "ignored").unwrap(), "Work");
    }

    #[test]
    fn fill_only_first_hole() {
        assert_eq!(fill_regex(r"^.*Work.*$", "Foo").unwrap(), "FooWork");
    }

    #[test]
    fn char_class_dot_star_is_not_a_hole() {
        assert_eq!(fill_regex(r"^[.*]$", "unused").unwrap(), "*");
    }

    #[test]
    fn escaped_dot_star_is_not_a_hole() {
        assert_eq!(fill_regex(r"^foo\.*bar$", "unused").unwrap(), "foobar");
    }

    #[test]
    fn x_is_a_literal_not_a_regex() {
        assert_eq!(
            fill_regex(r"^.* [|] SpicyHome$", "foo.bar").unwrap(),
            "foo.bar | SpicyHome"
        );
    }

    #[test]
    fn newline_in_hole_is_rejected() {
        assert!(matches!(
            fill_regex("^.*$", "a\nb"),
            Err(FillError::NewlineInHole)
        ));
    }

    #[test]
    fn extract_spicy_home() {
        let split = split_hole(SPICY, "Hello! | SpicyHome").unwrap();
        assert_eq!(split.prefix, "");
        assert_eq!(split.hole, "Hello!");
        assert_eq!(split.suffix, " | SpicyHome");
        assert_eq!(extract_hole(SPICY, "Hello! | SpicyHome").unwrap(), "Hello!");
    }

    #[test]
    fn extract_word_work_bar() {
        assert_eq!(extract_hole(r"^.*Work.*$", "FooWorkBar").unwrap(), "Foo");
    }

    #[test]
    fn extract_no_hole() {
        assert!(matches!(
            extract_hole(r"^Work$", "Work"),
            Err(ExtractError::NoHole)
        ));
    }

    #[test]
    fn extract_non_match() {
        assert!(matches!(
            extract_hole(SPICY, "does not match"),
            Err(ExtractError::NoMatch)
        ));
    }

    #[test]
    fn extract_fill_round_trips() {
        let filled = fill_regex(SPICY, "Hello!").unwrap();
        assert_eq!(extract_hole(SPICY, &filled).unwrap(), "Hello!");
    }

    #[test]
    fn emit_affixes_chrome() {
        assert_eq!(
            emit_affixes(SPICY).unwrap(),
            Some(("".to_string(), " | Spicy Home".to_string()))
        );
        assert_eq!(emit_affixes(r"^Work$").unwrap(), None);
    }

    #[test]
    fn every_successful_fill_matches_the_pattern() {
        let cases: &[(&str, &str)] = &[
            (SPICY, "Review PR"),
            (SPICY, "Review PR | SpicyHome"),
            (r"^Work$", "ignored"),
            (r"^.*Work.*$", "Foo"),
            (r"^[.*]$", "unused"),
            (r"^foo\.*bar$", "unused"),
            (r"^.* [|] SpicyHome$", "foo.bar"),
            (r"^(alpha|beta|gamma)$", "unused"),
        ];
        for &(pattern, x) in cases {
            if let Ok(generated) = fill_regex(pattern, x) {
                let re = Regex::new(pattern).unwrap();
                assert!(re.is_match(&generated), "{pattern} | {x} -> {generated:?}");
            }
        }
    }
}
