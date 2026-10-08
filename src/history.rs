//! Historique mensuel de valorisation du portefeuille.
//!
//! Principe : un snapshot par mois, pris le PREMIER LUNDI du mois (fin de
//! journée, 23:59:59 UTC). Les mois passés sont immuables : on ne
//! (re)calcule jamais une date déjà présente dans history.json. Chaque
//! `cargo run` ne backfille que les premiers lundis manquants, et
//! seulement ceux dont la journée est terminée (date < aujourd'hui).
//!
//! Les anciennes entrées qui ne sont pas un premier lundi (ancien format
//! hebdomadaire "dimanche") sont purgées automatiquement au chargement.
//!
//! Gestion jours de fermeture / crypto vs stock : entièrement déléguée à
//! `historical_price_eur` (via `portfolio_snapshot_at`).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::Result;
use chrono::{Datelike, Duration, NaiveDate, TimeZone, Utc, Weekday};
use serde::{Deserialize, Serialize};

use crate::ledger::cost_basis::compute_fifo;
use crate::ledger::portfolio::{portfolio_snapshot_at, AssetSnapshot};
use crate::store::serialize::TxStore;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// Premier lundi du mois, format "YYYY-MM-DD".
    /// `alias` : lit encore les anciens fichiers (ils sont ensuite purgés).
    #[serde(alias = "week_end")]
    pub date: String,
    pub total_value_eur: f64,
    pub total_cost_basis_eur: f64,
    pub total_pnl_eur: f64,
}

/// Dimanche de la semaine ISO contenant `date` (lundi = début de semaine).
/// Conservée : utilisée par trades.rs pour la fréquence hebdomadaire.
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

fn is_first_monday(date: NaiveDate) -> bool {
    date.weekday() == Weekday::Mon && date.day() <= 7
}

/// Premiers lundis à snapshotter : un par mois entre le mois de `start` et
/// celui de `today`, en excluant ceux antérieurs à `start` (portefeuille
/// vide) et ceux qui ne sont pas strictement passés (`>= today`).
pub fn target_dates(start: NaiveDate, today: NaiveDate) -> Vec<NaiveDate> {
    let mut out = Vec::new();
    let (mut y, mut m) = (start.year(), start.month());

    while (y, m) <= (today.year(), today.month()) {
        let target = first_monday(y, m);
        if target >= start && target < today {
            out.push(target);
        }
        if m == 12 {
            y += 1;
            m = 1;
        } else {
            m += 1;
        }
    }
    out
}

/// Supprime les entrées dont la date n'est pas un premier lundi (ou n'est
/// pas parsable). Renvoie le nombre d'entrées purgées.
fn retain_first_mondays(history: &mut Vec<HistoryEntry>) -> usize {
    let before = history.len();
    history.retain(|e| NaiveDate::parse_from_str(&e.date, "%Y-%m-%d").map_or(false, is_first_monday));
    before - history.len()
}

/// Remplace les prix à 0.0 (échec API) par le dernier prix valide connu,
/// met la mémoire à jour avec les prix valides, et renvoie la valeur
/// totale recalculée.
fn patch_prices(assets: &mut [AssetSnapshot], last_known: &mut HashMap<String, f64>) -> f64 {
    let mut total = 0.0;
    for asset in assets.iter_mut() {
        if asset.price_eur <= 0.0 {
            if let Some(&last_price) = last_known.get(&asset.symbol) {
                asset.price_eur = last_price;
                asset.value_eur = asset.quantity * last_price;
            } else {
                eprintln!("  [WARN history] prix manquant pour {}", asset.symbol);
            }
        } else {
            last_known.insert(asset.symbol.clone(), asset.price_eur);
        }
        total += asset.value_eur;
    }
    total
}

fn earliest_tx_date(tx_store: &TxStore) -> Option<NaiveDate> {
    tx_store.transactions.iter().map(|tx| tx.time.date_naive()).min()
}

/// Backfille (de façon incrémentale) l'historique mensuel dans `path`.
/// Réexécutable sans risque : les dates déjà présentes ne sont jamais
/// retouchées.
pub fn record_monthly_history(tx_store: &TxStore, path: &Path) -> Result<()> {
    let mut history: Vec<HistoryEntry> = if path.exists() {
        let raw = std::fs::read_to_string(path)?;
        serde_json::from_str(&raw).unwrap_or_default()
    } else {
        Vec::new()
    };

    let purged = retain_first_mondays(&mut history);
    let already_done: HashSet<String> = history.iter().map(|e| e.date.clone()).collect();

    let Some(start_date) = earliest_tx_date(tx_store) else {
        return Ok(()); // aucune transaction, rien à backfiller
    };

    let today = Utc::now().date_naive();

    // Mémoire des derniers prix valides (pallie les trous de l'API Yahoo).
    // Les dates sont traitées dans l'ordre chronologique.
    let mut last_known_prices: HashMap<String, f64> = HashMap::new();
    let mut computed = 0;

    for target in target_dates(start_date, today) {
        let key = target.format("%Y-%m-%d").to_string();
        if already_done.contains(&key) {
            continue;
        }

        // 23:59:59 UTC le lundi, pour inclure toutes les tx du jour.
        let at = Utc.from_utc_datetime(&target.and_hms_opt(23, 59, 59).unwrap());

        let mut snapshot = portfolio_snapshot_at(tx_store, Some(at));
        let cost_basis = compute_fifo(tx_store, Some(at))?;

        snapshot.total_value_eur = patch_prices(&mut snapshot.assets, &mut last_known_prices);

        let total_cost_basis_eur: f64 =
            snapshot.assets.iter().map(|a| cost_basis.open_cost_basis(&a.symbol)).sum();

        if computed == 0 {
            println!("=== BACKFILL HISTORIQUE MENSUEL (1er lundi) ===");
        }
        println!("  {key} : {:.2} EUR", snapshot.total_value_eur);

        history.push(HistoryEntry {
            date: key,
            total_value_eur: snapshot.total_value_eur,
            total_cost_basis_eur,
            total_pnl_eur: snapshot.total_value_eur - total_cost_basis_eur,
        });
        computed += 1;
    }

    if computed > 0 || purged > 0 {
        history.sort_by(|a, b| a.date.cmp(&b.date));
        std::fs::write(path, serde_json::to_string_pretty(&history)?)?;
        println!("({computed} mois ajouté(s), {purged} ancienne(s) entrée(s) hebdo purgée(s))");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::AssetKind;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn entry(date: &str) -> HistoryEntry {
        HistoryEntry { date: date.to_string(), total_value_eur: 1.0, total_cost_basis_eur: 1.0, total_pnl_eur: 0.0 }
    }

    fn asset(symbol: &str, quantity: f64, price_eur: f64) -> AssetSnapshot {
        AssetSnapshot {
            symbol: symbol.to_string(),
            quantity,
            price_eur,
            value_eur: quantity * price_eur,
            kind: AssetKind::Crypto,
            ticker: None,
        }
    }

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    // --- week_end (utilisée par trades.rs) ---

    #[test]
    fn week_end_maps_any_weekday_to_its_sunday() {
        assert_eq!(week_end(d(2026, 10, 7)), d(2026, 10, 11)); // mercredi
        assert_eq!(week_end(d(2026, 10, 5)), d(2026, 10, 11)); // lundi
        assert_eq!(week_end(d(2026, 10, 11)), d(2026, 10, 11)); // dimanche
    }

    // --- first_monday ---

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

    // --- target_dates ---

    #[test]
    fn target_dates_skips_first_monday_before_start() {
        // Le 1er lundi de janvier (5/1) précède le début (15/1) -> ignoré.
        let dates = target_dates(d(2026, 1, 15), d(2026, 4, 10));
        assert_eq!(dates, vec![d(2026, 2, 2), d(2026, 3, 2), d(2026, 4, 6)]);
    }

    #[test]
    fn target_dates_excludes_today_and_future() {
        // today == 1er lundi de février -> pas encore une journée complète.
        let dates = target_dates(d(2026, 1, 1), d(2026, 2, 2));
        assert_eq!(dates, vec![d(2026, 1, 5)]);
    }

    #[test]
    fn target_dates_handles_year_rollover() {
        let dates = target_dates(d(2025, 11, 1), d(2026, 2, 10));
        assert_eq!(dates, vec![d(2025, 11, 3), d(2025, 12, 1), d(2026, 1, 5), d(2026, 2, 2)]);
    }

    #[test]
    fn target_dates_empty_when_nothing_is_complete_yet() {
        assert!(target_dates(d(2026, 10, 6), d(2026, 10, 8)).is_empty());
    }

    #[test]
    fn target_dates_are_sorted_unique_first_mondays() {
        let dates = target_dates(d(2021, 3, 17), d(2026, 10, 8));
        assert!(dates.windows(2).all(|w| w[0] < w[1]));
        assert!(dates.iter().all(|&date| is_first_monday(date)));
    }

    // --- retain_first_mondays ---

    #[test]
    fn retain_first_mondays_purges_old_weekly_sundays_and_garbage() {
        let mut history = vec![
            entry("2026-10-04"), // dimanche (ancien format)
            entry("2026-10-05"), // 1er lundi -> gardé
            entry("2026-10-12"), // 2e lundi -> purgé
            entry("pas-une-date"),
            entry("2026-11-02"), // 1er lundi -> gardé
        ];
        let purged = retain_first_mondays(&mut history);

        assert_eq!(purged, 3);
        let dates: Vec<&str> = history.iter().map(|e| e.date.as_str()).collect();
        assert_eq!(dates, vec!["2026-10-05", "2026-11-02"]);
    }

    // --- patch_prices ---

    #[test]
    fn patch_prices_uses_last_known_price_when_price_is_zero() {
        let mut last_known = HashMap::new();
        last_known.insert("BTC".to_string(), 50_000.0);

        let mut assets = vec![asset("BTC", 2.0, 0.0)];
        let total = patch_prices(&mut assets, &mut last_known);

        assert!(approx(assets[0].price_eur, 50_000.0));
        assert!(approx(assets[0].value_eur, 100_000.0));
        assert!(approx(total, 100_000.0));
    }

    #[test]
    fn patch_prices_remembers_valid_prices() {
        let mut last_known = HashMap::new();
        let mut assets = vec![asset("ETH", 10.0, 2_000.0)];
        let total = patch_prices(&mut assets, &mut last_known);

        assert!(approx(total, 20_000.0));
        assert!(approx(last_known["ETH"], 2_000.0));
    }

    #[test]
    fn patch_prices_keeps_zero_value_when_no_fallback_exists() {
        let mut last_known = HashMap::new();
        let mut assets = vec![asset("NEW", 5.0, 0.0), asset("BTC", 1.0, 100.0)];
        let total = patch_prices(&mut assets, &mut last_known);

        assert!(approx(assets[0].value_eur, 0.0));
        assert!(approx(total, 100.0));
        assert!(!last_known.contains_key("NEW"));
    }

    #[test]
    fn patch_prices_carries_over_between_successive_months() {
        let mut last_known = HashMap::new();

        let mut month1 = vec![asset("BTC", 1.0, 40_000.0)];
        patch_prices(&mut month1, &mut last_known);

        let mut month2 = vec![asset("BTC", 1.0, 0.0)]; // Yahoo/Binance a bugué
        let total2 = patch_prices(&mut month2, &mut last_known);

        assert!(approx(total2, 40_000.0));
    }

    // --- sérialisation ---

    #[test]
    fn history_entry_reads_legacy_week_end_field() {
        let json = r#"{"week_end":"2026-10-04","total_value_eur":1.0,"total_cost_basis_eur":1.0,"total_pnl_eur":0.0}"#;
        let e: HistoryEntry = serde_json::from_str(json).unwrap();
        assert_eq!(e.date, "2026-10-04");
    }

    #[test]
    fn history_entry_serializes_as_date() {
        let json = serde_json::to_string(&entry("2026-10-05")).unwrap();
        assert!(json.contains("\"date\":\"2026-10-05\""));
        assert!(!json.contains("week_end"));
    }
}
