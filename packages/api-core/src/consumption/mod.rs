//! The 14-day consumption fold: descriptive platform totals for the
//! consistency page.
//!
//! Nothing here scores, judges, or targets anything. The report answers one
//! question — where the last fourteen civil days of logged time went, by
//! platform — and leaves every verdict to [`crate::rules`].
//!
//! # Gross time, not the digest's share
//!
//! Every number is an integer number of **gross logged seconds**: an event's
//! own duration, credited once. The digest deliberately splits overlapping
//! events and shares the overlap, so these totals are larger than the sum of
//! `daily_category_minutes` and will **not** match The Record's category
//! ranking. That is intended: this page asks what the diary contained, not how
//! much of the day it owned.
//!
//! # A fixed slug grammar
//!
//! Classification is by the leaf category's **slug** — never its UUID, title,
//! or calendar summary. `movies` → series, `gaming` → gaming,
//! `social-media-youtube` → social, and `social` + a WhatsApp title →
//! messaging. These four slugs are a fixed personal grammar: renaming a slug
//! silently empties that group, and that trade is accepted here.
//!
//! # Layout
//!
//! * [`assemble`] — the pure fold: events and categories in,
//!   [`ConsumptionReport`] out.
//! * [`service`] — the async entry point the Worker calls.

pub mod assemble;
pub mod service;

#[cfg(test)]
mod tests;

pub use assemble::{
    assemble_consumption, ConsumptionGroupTotal, ConsumptionReport, ConsumptionRow,
};
pub use service::consumption_report;
