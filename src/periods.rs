//! Arithmétique de calendrier partagée (historique, fréquence des trades).
//!
//! Extrait de `history.rs` : `trades.rs` n'a plus à dépendre de `history.rs`
//! juste pour `week_end`, et les boucles « mois par mois » (dupliquées dans
//! `history.rs` et `trades.rs`) deviennent un simple itérateur.
//!
//! Branchement (lib.rs : `pub mod periods;`) :
//!
//! history.rs
//! ```ignore
//! use crate::periods::{first_monday, is_first_monday, months_between, sundays_from, week_end};
//!
//! fn weekly_dates(start: NaiveDate, today: NaiveDate) -> Vec<NaiveDate> {
//!     sundays_from(start).take_while(|d| *d < today).collect()
//! }
//!
//! fn monthly_dates(start: NaiveDate, today: NaiveDate) -> Vec<NaiveDate> {
//!     months_between(start, today)
//!         .map(|(y, m)| first_monday(y, m))
//!         .filter(|d| *d >= start && *d < today)
//!         .collect()
//! }
//! ```
//! (supprimer `week_end`, `first_monday`, `is_first_monday` de history.rs ;
//! les tests existants continuent de les voir via `use super::*`).
//!
//! trades.rs : `use crate::periods::{months_between, sundays_from, week_end};`

use chrono::{Datelike, Duration, NaiveDate, Weekday};

/// Dimanche de la semaine ISO contenant `date` (lundi = début de semaine).
pub fn week_end(date: NaiveDate) -> NaiveDate {
    let offset = (Weekday::Sun.num_days_from_monday() as i64
        - date.weekday().num_days_from_monday() as i64
        + 7)