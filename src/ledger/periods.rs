//! Arithmétique de calendrier partagée (historique, fréquence des trades).
//!
//! Évite que `trades.rs` dépende de `history.rs` juste pour `week_end`, et
//! remplace les boucles « mois par mois » / « semaine par semaine » qui
//! étaient dupliquées dans `history.rs` et `trades.rs`.

use chrono::{Datelike, Duration, NaiveDate, Weekday};

/// Dimanche de la semaine ISO contenant `date` (lundi = début de semaine).
pub fn week_end(date: NaiveDate) -> NaiveDate {
    let offset = (Weekday::Sun.num_days_from_monday() as i64
        - date.weekday().num_days_from_monday() as i64
        + 7)
        % 7;
    date + Duration::days(offset)
}

/// Premier lundi du mois `year`-`month`.
pub fn first_monday(year: i32, month: u32) -> NaiveDate {
    let first = NaiveDate::from_ymd_opt(year, month, 1).expect("mois valide");
    let offset = (7 - first.weekday().num_days_from_monday() as i64) % 7;
    first + Duration::days(offset)
}

pub fn is_first_monday(date: NaiveDate) -> bool {
    date.weekday() == Weekday::Mon && date.day() <= 7
}

/// Couples (année, mois) du mois de `start` à celui de `end`, bornes incluses.
/// Vide si `start` est dans un mois postérieur à `end`.
pub fn months_between(start: NaiveDate, end: NaiveDate) -> impl Iterator<Item = (i32, u32)> {
    let last = (end.year(), end.month());
    let first = (start.year(), start.month());
    std::iter::successors((first <= last).then_some(first), move |&(y, m)| {
        let next = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
        (next <= last).then_some(next)
    })
}

/// Dimanches de fin de semaine, de la semaine de `start` à l'infini
/// (à borner avec `take_while`).
pub fn sundays_from(start: NaiveDate) -> impl Iterator<Item = NaiveDate> {
    std::iter::successors(Some(week_end(start)), |d| Some(*d + Duration::weeks(1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn week_end_maps_any_weekday_to_its_sunday() {
        assert_eq!(week_end(d(2026, 10, 7)), d(2026, 10, 11)); // mercredi
        assert_eq!(week_end(d(2026, 10, 5)), d(2026, 10, 11)); // lundi
        assert_eq!(week_end(d(2026, 10, 11)), d(2026, 10, 11)); // dimanche
        assert_eq!(week_end(d(2026, 12, 31)), d(2027, 1, 3)); // passage d'année
    }

    #[test]
    fn first_monday_known_months() {
        assert_eq!(first_monday(2026, 10), d(2026, 10, 5)); // 1er = jeudi
        assert_eq!(first_monday(2024, 1), d(2024, 1, 1)); // 1er = lundi
        assert_eq!(first_monday(2026, 2), d(2026, 2, 2)); // 1er = dimanche
        assert_eq!(first_monday(2025, 12), d(2025, 12, 1)); // 1er = lundi
    }

    #[test]
    fn first_monday_is_always_a_monday_within_the_first_seven_days() {
        for year in 2020..=2035 {
            for month in 1..=12 {
                let fm = first_monday(year, month);
                assert_eq!(fm.weekday(), Weekday::Mon, "{year}-{month}");
                assert!(fm.day() <= 7, "{year}-{month}");
                assert_eq!((fm.year(), fm.month()), (year, month));
            }
        }
    }

    #[test]
    fn is_first_monday_distinguishes_mondays_and_other_days() {
        assert!(is_first_monday(d(2026, 10, 5)));
        assert!(!is_first_monday(d(2026, 10, 12))); // 2e lundi
        assert!(!is_first_monday(d(2026, 10, 4))); // dimanche
    }

    #[test]
    fn months_between_is_inclusive_and_crosses_years() {
        let months: Vec<_> = months_between(d(2025, 11, 20), d(2026, 2, 3)).collect();
        assert_eq!(months, vec![(2025, 11), (2025, 12), (2026, 1), (2026, 2)]);
    }

    #[test]
    fn months_between_same_month_is_one_and_reversed_is_empty() {
        assert_eq!(months_between(d(2026, 3, 1), d(2026, 3, 31)).collect::<Vec<_>>(), vec![(2026, 3)]);
        assert_eq!(months_between(d(2026, 4, 1), d(2026, 3, 31)).count(), 0);
    }

    #[test]
    fn sundays_from_starts_with_the_sunday_of_the_start_week() {
        let sundays: Vec<_> = sundays_from(d(2026, 10, 7)).take(3).collect(); // mercredi
        assert_eq!(sundays, vec![d(2026, 10, 11), d(2026, 10, 18), d(2026, 10, 25)]);
        assert_eq!(sundays_from(d(2026, 10, 11)).next(), Some(d(2026, 10, 11))); // déjà dimanche
    }

    #[test]
    fn sundays_from_matches_the_historical_week_by_week_loop() {
        let (start, today) = (d(2025, 12, 17), d(2026, 2, 20));
        let mut expected = Vec::new();
        let mut cursor = week_end(start);
        while cursor <= week_end(today) {
            expected.push(cursor);
            cursor += Duration::weeks(1);
        }
        let got: Vec<_> = sundays_from(start).take_while(|d| *d <= week_end(today)).collect();
        assert_eq!(got, expected);
    }
}
