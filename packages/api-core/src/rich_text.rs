//! Markdown → calendar plain text.
//!
//! Task and routine descriptions are stored as Markdown (ADR 0015), but a
//! Google Calendar `description` is consumed by Google, Notion Calendar, Apple
//! Calendar, Outlook, and anything else subscribed to the calendar. Markdown
//! syntax such as `**` or `#` is noise there, so the one-shot event created by
//! a task/routine start carries a readable plain-text rendering instead.
//!
//! This is deliberately a *rendering*, not a parser of record: it preserves
//! the author's words, line breaks, list markers, and link targets, and drops
//! presentation-only markers. Raw HTML in the Markdown is treated as text —
//! this module never emits HTML.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

/// Upper bound for a stored description, in UTF-8 bytes. Generous for prose,
/// small enough to keep D1 rows and editor payloads bounded.
pub const MAX_DESCRIPTION_BYTES: usize = 16 * 1024;

/// `true` when `description` is within [`MAX_DESCRIPTION_BYTES`].
pub fn description_is_within_limit(description: &str) -> bool {
    description.len() <= MAX_DESCRIPTION_BYTES
}

/// The only Markdown extensions descriptions use. GFM tables are deliberately
/// absent: a description is prose, and a table pasted into a calendar body is
/// unreadable in every client anyway.
fn options() -> Options {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options
}

enum ListState {
    Bullet,
    Ordered { next: u64 },
}

struct LinkState {
    destination: String,
    label: String,
}

/// Renders Markdown to the plain text written into a Google event body.
///
/// - Paragraphs/headings/code blocks are separated by one blank line.
/// - Bullet and ordered lists keep `- ` / `1. ` markers, with two-space
///   indentation per nesting level.
/// - Links become `label (url)`; a label equal to the URL stays just the URL.
/// - Task-list markers become `[ ] ` / `[x] `.
/// - Emphasis/code markers are dropped; soft and hard breaks become `\n`.
pub fn markdown_to_calendar_text(markdown: &str) -> String {
    let mut out = String::new();
    let mut lists: Vec<ListState> = Vec::new();
    let mut link: Option<LinkState> = None;

    for event in Parser::new_ext(markdown, options()) {
        match event {
            Event::Start(Tag::Paragraph) => {
                if lists.is_empty() {
                    push_blank_line(&mut out);
                }
            }
            Event::End(TagEnd::Paragraph) => {
                if lists.is_empty() {
                    push_blank_line(&mut out);
                } else {
                    push_line_end(&mut out);
                }
            }
            Event::Start(Tag::Heading { .. }) => push_blank_line(&mut out),
            Event::End(TagEnd::Heading(_)) => push_blank_line(&mut out),
            Event::Start(Tag::BlockQuote(_)) => push_blank_line(&mut out),
            Event::End(TagEnd::BlockQuote(_)) => push_blank_line(&mut out),
            Event::Start(Tag::CodeBlock(_)) => push_blank_line(&mut out),
            Event::End(TagEnd::CodeBlock) => push_blank_line(&mut out),
            Event::Start(Tag::List(start)) => {
                if lists.is_empty() {
                    push_blank_line(&mut out);
                } else {
                    // A nested list can start the item itself (`- - child`).
                    // The parent marker has no text of its own; drop it rather
                    // than emitting a bare `-` line.
                    strip_dangling_item_marker(&mut out);
                }
                lists.push(match start {
                    Some(next) => ListState::Ordered { next },
                    None => ListState::Bullet,
                });
            }
            Event::End(TagEnd::List(_)) => {
                lists.pop();
                if lists.is_empty() {
                    push_blank_line(&mut out);
                }
            }
            Event::Start(Tag::Item) => {
                // Tight lists emit items back-to-back with no paragraph end;
                // a nested list must still start on its own line.
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                let depth = lists.len().saturating_sub(1);
                for _ in 0..depth {
                    out.push_str("  ");
                }
                match lists.last_mut() {
                    Some(ListState::Bullet) => out.push_str("- "),
                    Some(ListState::Ordered { next }) => {
                        out.push_str(&format!("{next}. "));
                        *next += 1;
                    }
                    None => {}
                }
            }
            Event::End(TagEnd::Item) => push_line_end(&mut out),
            Event::Start(Tag::Link { dest_url, .. }) => {
                link = Some(LinkState {
                    destination: dest_url.to_string(),
                    label: String::new(),
                });
            }
            Event::End(TagEnd::Link) => {
                if let Some(link) = link.take() {
                    let label = link.label.trim();
                    if label.is_empty() || label == link.destination {
                        out.push_str(&link.destination);
                    } else {
                        out.push_str(label);
                        out.push_str(" (");
                        out.push_str(&link.destination);
                        out.push(')');
                    }
                }
            }
            Event::Text(text) => push_text(&mut out, &mut link, &text),
            Event::Code(text) => push_text(&mut out, &mut link, &text),
            Event::SoftBreak | Event::HardBreak => out.push('\n'),
            Event::Rule => {
                push_blank_line(&mut out);
                out.push_str("---");
                push_blank_line(&mut out);
            }
            Event::TaskListMarker(checked) => {
                out.push_str(if checked { "[x] " } else { "[ ] " });
            }
            Event::Html(html) | Event::InlineHtml(html) => {
                // Raw HTML is text here: this function never emits markup.
                push_text(&mut out, &mut link, &html);
            }
            Event::Start(Tag::Image { .. }) | Event::End(TagEnd::Image) => {
                // Images have no place in a calendar body; keep only alt text
                // (which arrives as `Text` between the tags).
            }
            _ => {}
        }
    }

    normalize(out)
}

fn push_text(out: &mut String, link: &mut Option<LinkState>, text: &str) {
    match link {
        Some(link) => link.label.push_str(text),
        None => out.push_str(text),
    }
}

/// Ensures a blank line between block-level elements without stacking them.
fn push_blank_line(out: &mut String) {
    if out.is_empty() {
        return;
    }
    while out.ends_with(' ') || out.ends_with('\t') {
        out.pop();
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    if !out.ends_with("\n\n") {
        out.push('\n');
    }
}

/// Removes a just-written list marker when the item turns out to contain only
/// a nested list (`- - child`). Leaves real content untouched.
fn strip_dangling_item_marker(out: &mut String) {
    let line_start = out.rfind('\n').map(|index| index + 1).unwrap_or(0);
    let line = &out[line_start..];
    let trimmed = line.trim_start();
    let is_bullet = matches!(trimmed, "- " | "* " | "+ ");
    let is_ordered = trimmed.ends_with(". ")
        && trimmed[..trimmed.len() - 2]
            .chars()
            .all(|c| c.is_ascii_digit());
    if is_bullet || is_ordered {
        out.truncate(line_start);
    }
}

/// Ends the current line (list items, paragraphs inside lists).
fn push_line_end(out: &mut String) {
    while out.ends_with(' ') || out.ends_with('\t') {
        out.pop();
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
}

/// Trims trailing block breaks and collapses accidental 3+ newlines.
fn normalize(mut out: String) -> String {
    while out.ends_with('\n') || out.ends_with(' ') || out.ends_with('\t') {
        out.pop();
    }
    while out.contains("\n\n\n") {
        out = out.replace("\n\n\n", "\n\n");
    }
    let lines: Vec<&str> = out.lines().map(str::trim_end).collect();
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::markdown_to_calendar_text;

    #[test]
    fn blank_markdown_renders_empty() {
        assert_eq!(markdown_to_calendar_text(""), "");
        assert_eq!(markdown_to_calendar_text("   \n\n "), "");
    }

    #[test]
    fn paragraphs_and_breaks_survive() {
        assert_eq!(
            markdown_to_calendar_text("First line\nsecond line\n\nA new paragraph."),
            "First line\nsecond line\n\nA new paragraph."
        );
    }

    #[test]
    fn emphasis_and_inline_code_markers_are_dropped() {
        assert_eq!(
            markdown_to_calendar_text("**Bold**, *italic*, ~~strike~~, and `code`."),
            "Bold, italic, strike, and code."
        );
    }

    #[test]
    fn headings_become_plain_lines() {
        assert_eq!(
            markdown_to_calendar_text("# Warm up\n\nThen run."),
            "Warm up\n\nThen run."
        );
    }

    #[test]
    fn bullet_and_ordered_lists_keep_markers() {
        assert_eq!(
            markdown_to_calendar_text("- one\n- two\n\n1. first\n2. second"),
            "- one\n- two\n\n1. first\n2. second"
        );
    }

    #[test]
    fn nested_lists_are_indented() {
        assert_eq!(
            markdown_to_calendar_text("- one\n  - child\n- two"),
            "- one\n  - child\n- two"
        );
    }

    #[test]
    fn an_item_that_starts_with_a_nested_list_has_no_dangling_marker() {
        assert_eq!(
            markdown_to_calendar_text("- step one\n- - step two"),
            "- step one\n  - step two"
        );
    }

    #[test]
    fn links_become_label_and_url() {
        assert_eq!(
            markdown_to_calendar_text("See [the plan](https://example.com/plan)."),
            "See the plan (https://example.com/plan)."
        );
        assert_eq!(
            markdown_to_calendar_text("<https://example.com>"),
            "https://example.com"
        );
    }

    #[test]
    fn code_blocks_keep_their_text() {
        assert_eq!(
            markdown_to_calendar_text("```\nlet x = 1;\n```\n\nDone."),
            "let x = 1;\n\nDone."
        );
    }

    #[test]
    fn task_lists_keep_checkboxes() {
        assert_eq!(
            markdown_to_calendar_text("- [ ] call\n- [x] done"),
            "- [ ] call\n- [x] done"
        );
    }

    #[test]
    fn raw_html_is_text_never_markup() {
        assert_eq!(
            markdown_to_calendar_text("<b>not bold</b>"),
            "<b>not bold</b>"
        );
    }

    #[test]
    fn rules_become_a_plain_separator() {
        assert_eq!(markdown_to_calendar_text("a\n\n---\n\nb"), "a\n\n---\n\nb");
    }
}
