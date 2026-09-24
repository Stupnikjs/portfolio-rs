//! Historique hebdomadaire de valorisation du portefeuille.
//!
//! Principe : les semaines passées sont immuables (le passé ne change
//! pas), donc on ne (re)calcule jamais une semaine déjà présente dans
//! history.json. Chaque `cargo run` ne backfille que les semaines
//! manquantes entre la dernière semaine connue et la dernière semaine
//! *complète* (la semaine en cours n'est pas backfillée : le
//! dashboard.json du jour sert déjà de point "live" pour elle).
//!
//! Gestion jours de fermeture / crypto vs stock : entièrement déléguée à
//! `historical_price_eur` (déjà utilisée par `portfolio_snapshot_at`) --
//! pour une action, Yahoo renvoie la dernière clôture connue avant la
//! date cible (gère week-ends/jours fériés) ; pour une crypto, Binance
//! est interrogé directement sur cette date (marché ouvert 7j/7).

use std::collections::HashSet;
use std::path::Path;

use anyhow::Result;
use chrono::{Datelike, Duration, NaiveDate, TimeZone, Utc, Weekday};
use serde::{Deserialize, Serialize};

use crate::ledger::cost_basis::compute_fifo;
use crate::ledger::portfolio::portfolio_snapshot_at;
use crate::store::serialize::TxStore;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeeklyHistoryEntry {
    /// Dimanche de fin de semaine ISO, format "YYYY-MM-DD".
    pub week_end: String,
    pub total_value_eur: f64,
    pub total_cost_basis_eur: f64,
    pub total_pnl_eur: f64,
}

/// Dimanche de la semaine ISO contenant `date` (lundi = début de semaine).
pub fn week_end(date: NaiveDate) -> NaiveDate {
    let offset = (Weekday::Sun.num_days_from_monday() as i64
        - date.weekday().num_days_from_monday() as i64
        + 7)
        % 7;
    date + Duration::days(offset)
}

fn earliest_tx_date(tx_store: &TxStore) -> Option<NaiveDate> {
    tx_store.transactions.iter().map(|tx| tx.time.date_naive()).min()
}

/// Backfille (de façon incrémentale) l'historique hebdomadaire dans
/// `path`, à partir de la première transaction connue jusqu'à la
/// dernière semaine *complète*. Réexécutable sans risque : les semaines
/// déjà présentes ne sont jamais retouchées.
pub fn record_weekly_history(tx_store: &TxStore, path: &Path) -> Result<()> {
    let mut history: Vec<WeeklyHistoryEntry> = if path.exists() {
        let raw = std::fs::read_to_string(path)?;
        serde_json::from_str(&raw).unwrap_or_default()
    } else {
        Vec::new()
    };

    let already_done: HashSet<String> = history.iter().map(|e| e.week_end.clone()).collect();

    let Some(start_date) = earliest_tx_date(tx_store) else {
        return Ok(()); // aucune transaction, rien à backfiller
    };

    let today = Utc::now().date_naive();
    // La semaine en cours n'a pas de "clôture" définitive -- on s'arrête
    // à la dernière semaine complète.
    let last_complete_week_end = week_end(today) - Duration::weeks(1);

    if week_end(start_date) > last_complete_week_end {
        return Ok(()); // pas encore une semaine complète d'historique
    }

    let mut cursor = week_end(start_date);
    let mut computed = 0;

    while cursor <= last_complete_week_end {
        let key = cursor.format("%Y-%m-%d").to_string();

        if !already_done.contains(&key) {
            // 23:59:59 UTC le dimanche, pour inclure toutes les tx du jour.
            let at = Utc.from_utc_datetime(&cursor.and_hms_opt(23, 59, 59).unwrap());

            let snapshot = portfolio_snapshot_at(tx_store, Some(at));
            let cost_basis = compute_fifo(tx_store, Some(at))?;
            let total_cost_basis_eur: f64 =
                snapshot.assets.iter().map(|a| cost_basis.open_cost_basis(&a.symbol)).sum();

            if computed == 0 {
                println!("=== BACKFILL HISTORIQUE HEBDOMADAIRE ===");
            }
            println!("  {key} : {:.2} EUR", snapshot.total_value_eur);

            history.push(WeeklyHistoryEntry {
                week_end: key,
                total_value_eur: snapshot.total_value_eur,
                total_cost_basis_eur,
                total_pnl_eur: snapshot.total_value_eur - total_cost_basis_eur,
            });
            computed += 1;
        }

        cursor += Duration::weeks(1);
    }

    if computed > 0 {
        history.sort_by(|a, b| a.week_end.cmp(&b.week_end));
        std::fs::write(path, serde_json::to_string_pretty(&history)?)?;
        println!("({computed} semaine(s) ajoutée(s) à l'historique)");
    }

    Ok(())
}