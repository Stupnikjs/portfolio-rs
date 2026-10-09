//! Historique de valorisation du portefeuille, hebdomadaire ou mensuel.
//!
//! Principe : un snapshot par période, pris en fin de journée (23:59:59 UTC) :
//! - `Frequency::Weekly`  : le DIMANCHE de chaque semaine ISO ;
//! - `Frequency::Monthly` : le PREMIER LUNDI de chaque mois.
//!
//! Les périodes passées sont immuables : on ne
//! (re)calcule jamais une date déjà présente dans history.json, SAUF si
//! l'entrée est périmée (ancien format sans détail par actif) ou
//! incomplète (au moins un prix manquant/reporté) -- elle est alors
//! recalculée au prochain run, ce qui permet de se rattraper après une
//! panne Yahoo/Binance. Chaque `cargo run` ne backfille que les dates
//! manquantes, et seulement celles dont la journée est terminée
//! (date < aujourd'hui) : la semaine / le mois en cours n'est pas écrit.
//!
//! Le cash (EUR, USDC, USD...) est EXCLU de l'historique : ses dépôts
//! créent des lots à coût de revient plein alors que les achats ne le
//! consomment pas toujours (jambes cash non tracées), ce qui gonfle le
//! cost basis et fabrique un P&L négatif fictif. Seuls les actifs
//! Crypto/Stock sont valorisés.
//!
//! Chaque entrée contient le snapshot de chaque actif (quantité, prix EUR,
//! valeur EUR, cost basis, fiabilité du prix) et pas seulement les totaux :
//! les totaux sont toujours la somme des actifs, et un prix suspect est
//! visible au lieu d'être noyé dans un agrégat.
//!
//! `cargo run -- --rebuild-history` recalcule tout l'historique.
//!
//! Les entrées dont la date n'est pas sur la grille de la fréquence choisie
//! (ex. un lundi quand on passe en hebdo, un dimanche en mensuel) sont
//! purgées au chargement : changer de fréquence recalcule donc l'historique
//! (les prix déjà en cache rendent ça rapide).
//!
//! Gestion jours de fermeture / crypto vs stock : entièrement déléguée à
//! `historical_price_eur` (via `portfolio_snapshot_with`).

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc, Weekday};
use serde::{Deserialize, Serialize};

use crate::ledger::cost_basis::compute_fifo;
use crate::ledger::portfolio::{portfolio_snapshot_with, AssetSnapshot, PriceFn};
use crate::market::prices::historical_price_eur;
use crate::schema::AssetKind;
use crate::store::serialize::TxStore;

/// Fréquence des snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frequency {
    /// Dimanche de chaque semaine ISO.
    Weekly,
    /// Premier lundi de chaque mois.
    Monthly,
}

impl Frequency {
    fn label(self) -> &'static str {
        match self {
            Frequency::Weekly => "HEBDOMADAIRE (dimanche)",
            Frequency::Monthly => "MENSUEL (1er lundi)",
        }
    }

    /// True si `date` est une date de snapshot valide pour cette fréquence.
    fn is_grid_date(self, date: NaiveDate) -> bool {
        match self {
            Frequency::Weekly => date.weekday() == Weekday::Sun,
            Frequency::Monthly => is_first_monday(date),
        }
    }
}

/// Version du format des entrées. Une entrée de version inférieure (ancien
/// history.json, calculé avec des prix figés) est recalculée.
pub const HISTORY_ENTRY_VERSION: u32 = 2;

/// Fiabilité du prix utilisé pour un actif dans un snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceStatus {
    /// Prix récupéré pour cette date.
    #[default]
    Ok,
    /// Échec de récupération : dernier prix valide connu (mois précédent).
    Carried,
    /// Échec et aucun prix antérieur : valeur à 0.
    Missing,
}

/// Snapshot d'un actif à la date de l'entrée.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryAsset {
    pub symbol: String,
    pub kind: AssetKind,
    pub quantity: f64,
    pub price_eur: f64,
    pub value_eur: f64,
    pub cost_basis_eur: f64,
    #[serde(default)]
    pub price_status: PriceStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// Date du snapshot (dimanche en hebdo, 1er lundi en mensuel), "YYYY-MM-DD".
    /// `alias` : lit encore les anciens fichiers (ils sont ensuite purgés).
    #[serde(alias = "week_end")]
    pub date: String,
    pub total_value_eur: f64,
    pub total_cost_basis_eur: f64,
    pub total_pnl_eur: f64,
    /// 0 pour un ancien fichier (champ absent) -> entrée recalculée.
    #[serde(default)]
    pub version: u32,
    /// False si au moins un actif n'a pas un prix `Ok` -> recalculée au prochain run.
    #[serde(default)]
    pub complete: bool,
    #[serde(default)]
    pub assets: Vec<HistoryAsset>,
}

impl HistoryEntry {
    /// Une entrée finale n'est jamais recalculée.
    fn is_final(&self) -> bool {
        self.version >= HISTORY_ENTRY_VERSION && self.complete
    }
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

/// Dates à snapshotter, de la première période contenant une transaction
/// jusqu'à la dernière période TERMINÉE (date strictement avant `today`).
pub fn target_dates(start: NaiveDate, today: NaiveDate, freq: Frequency) -> Vec<NaiveDate> {
    match freq {
        Frequency::Weekly => weekly_dates(start, today),
        Frequency::Monthly => monthly_dates(start, today),
    }
}

/// Dimanches de la semaine de `start` jusqu'à `today` (exclu).
fn weekly_dates(start: NaiveDate, today: NaiveDate) -> Vec<NaiveDate> {
    let mut out = Vec::new();
    let mut cursor = week_end(start);
    while cursor < today {
        out.push(cursor);
        cursor += Duration::weeks(1);
    }
    out
}

/// Premiers lundis : un par mois entre le mois de `start` et celui de
/// `today`, en excluant ceux antérieurs à `start` (portefeuille vide) et
/// ceux qui ne sont pas strictement passés (`>= today`).
fn monthly_dates(start: NaiveDate, today: NaiveDate) -> Vec<NaiveDate> {
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

/// Supprime les entrées dont la date n'est pas sur la grille de `freq` (ou
/// n'est pas parsable). Renvoie le nombre d'entrées purgées.
fn retain_on_grid(history: &mut Vec<HistoryEntry>, freq: Frequency) -> usize {
    let before = history.len();
    history.retain(|e| NaiveDate::parse_from_str(&e.date, "%Y-%m-%d").map_or(false, |d| freq.is_grid_date(d)));
    before - history.len()
}

/// Fiabilité de chaque prix AVANT correction (à appeler avant `patch_prices`).
fn classify_prices(assets: &[AssetSnapshot], last_known: &HashMap<String, f64>) -> Vec<PriceStatus> {
    assets
        .iter()
        .map(|a| {
            if a.price_eur > 0.0 && a.price_eur.is_finite() {
                PriceStatus::Ok
            } else if last_known.contains_key(&a.symbol) {
                PriceStatus::Carried
            } else {
                PriceStatus::Missing
            }
        })
        .collect()
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

fn load_history(path: &Path) -> Result<Vec<HistoryEntry>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(path).with_context(|| format!("lecture de {path:?}"))?;
    match serde_json::from_str::<Vec<HistoryEntry>>(&raw) {
        Ok(history) => Ok(history),
        Err(e) => {
            // On ne jette pas silencieusement un fichier illisible.
            let backup = path.with_extension("json.bak");
            eprintln!("[WARN history] {path:?} illisible ({e}) : copie dans {backup:?}, reconstruction complète");
            std::fs::copy(path, &backup).with_context(|| format!("sauvegarde vers {backup:?}"))?;
            Ok(Vec::new())
        }
    }
}

/// Écriture atomique (fichier temporaire + rename) : un crash en cours
/// d'écriture ne laisse pas un history.json tronqué.
fn write_atomic(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, content).with_context(|| format!("écriture de {tmp:?}"))?;
    std::fs::rename(&tmp, path).with_context(|| format!("remplacement de {path:?}"))?;
    Ok(())
}

/// Calcule le snapshot complet (par actif) à la date `target`.
fn build_entry(
    tx_store: &TxStore,
    target: NaiveDate,
    price_fn: PriceFn,
    last_known: &mut HashMap<String, f64>,
) -> Result<HistoryEntry> {
    // 23:59:59 UTC le lundi, pour inclure toutes les tx du jour.
    let at = Utc.from_utc_datetime(&target.and_hms_opt(23, 59, 59).unwrap());

    let mut snapshot = portfolio_snapshot_with(tx_store, Some(at), price_fn);
    // Pas de cash dans l'historique (voir en-tête du module).
    snapshot.assets.retain(|a| a.kind != AssetKind::Cash);
    let cost_basis = compute_fifo(tx_store, Some(at))?;

    let statuses = classify_prices(&snapshot.assets, last_known);
    let total_value_eur = patch_prices(&mut snapshot.assets, last_known);

    let assets: Vec<HistoryAsset> = snapshot
        .assets
        .iter()
        .zip(statuses)
        .map(|(a, status)| HistoryAsset {
            symbol: a.symbol.clone(),
            kind: a.kind,
            quantity: a.quantity,
            price_eur: a.price_eur,
            value_eur: a.value_eur,
            cost_basis_eur: cost_basis.open_cost_basis(&a.symbol),
            price_status: status,
        })
        .collect();

    let total_cost_basis_eur: f64 = assets.iter().map(|a| a.cost_basis_eur).sum();
    let complete = assets.iter().all(|a| a.price_status == PriceStatus::Ok);

    Ok(HistoryEntry {
        date: target.format("%Y-%m-%d").to_string(),
        total_value_eur,
        total_cost_basis_eur,
        total_pnl_eur: total_value_eur - total_cost_basis_eur,
        version: HISTORY_ENTRY_VERSION,
        complete,
        assets,
    })
}

/// Historique hebdomadaire (dimanches). Voir `record_history`.
pub fn record_weekly_history(tx_store: &TxStore, path: &Path) -> Result<()> {
    record_history(tx_store, path, Frequency::Weekly)
}

/// Historique mensuel (premiers lundis). Voir `record_history`.
pub fn record_monthly_history(tx_store: &TxStore, path: &Path) -> Result<()> {
    record_history(tx_store, path, Frequency::Monthly)
}

/// Backfille (de façon incrémentale) l'historique dans `path`.
/// Réexécutable sans risque : les entrées finales ne sont jamais retouchées.
/// `--rebuild-history` en argument de ligne de commande force le recalcul.
pub fn record_history(tx_store: &TxStore, path: &Path, freq: Frequency) -> Result<()> {
    let rebuild = std::env::args().any(|a| a == "--rebuild-history");
    let live_prices = |symbol: &str, kind: AssetKind, ticker: Option<&str>, at: DateTime<Utc>| {
        historical_price_eur(symbol, at, kind, ticker)
    };
    record_history_with(tx_store, path, &live_prices, Utc::now().date_naive(), freq, rebuild)?;
    Ok(())
}

/// Version injectable (prix, date du jour) de `record_history`.
/// Renvoie le nombre d'entrées (re)calculées.
pub fn record_history_with(
    tx_store: &TxStore,
    path: &Path,
    price_fn: PriceFn,
    today: NaiveDate,
    freq: Frequency,
    rebuild: bool,
) -> Result<usize> {
    let Some(start_date) = earliest_tx_date(tx_store) else {
        return Ok(0); // aucune transaction, rien à backfiller
    };

    let mut history = if rebuild { Vec::new() } else { load_history(path)? };
    let purged = retain_on_grid(&mut history, freq);
    let mut existing: HashMap<String, HistoryEntry> =
        history.into_iter().map(|e| (e.date.clone(), e)).collect();

    // Mémoire des derniers prix valides (pallie les trous de l'API).
    // Les dates sont traitées dans l'ordre chronologique.
    let mut last_known_prices: HashMap<String, f64> = HashMap::new();
    let mut result: Vec<HistoryEntry> = Vec::new();
    let mut computed = 0;

    for target in target_dates(start_date, today, freq) {
        let key = target.format("%Y-%m-%d").to_string();

        let entry = match existing.remove(&key) {
            Some(e) if e.is_final() => e,
            _ => {
                if computed == 0 {
                    println!("=== BACKFILL HISTORIQUE {} ===", freq.label());
                }
                let e = build_entry(tx_store, target, price_fn, &mut last_known_prices)?;
                let degraded: Vec<&str> = e
                    .assets
                    .iter()
                    .filter(|a| a.price_status != PriceStatus::Ok)
                    .map(|a| a.symbol.as_str())
                    .collect();
                if degraded.is_empty() {
                    println!("  {key} : {:.2} EUR", e.total_value_eur);
                } else {
                    println!("  {key} : {:.2} EUR  ⚠ prix manquants/reportés : {}", e.total_value_eur, degraded.join(", "));
                }
                computed += 1;
                e
            }
        };

        // Les entrées conservées alimentent aussi la mémoire des prix.
        for a in &entry.assets {
            if a.price_eur > 0.0 {
                last_known_prices.insert(a.symbol.clone(), a.price_eur);
            }
        }
        result.push(entry);
    }

    // Entrées valides hors des dates ciblées (ex. antérieures au début) : conservées.
    result.extend(existing.into_values());

    if computed > 0 || purged > 0 {
        result.sort_by(|a, b| a.date.cmp(&b.date));
        write_atomic(path, &serde_json::to_string_pretty(&result)?)?;
        println!("({computed} snapshot(s) (re)calculé(s), {purged} entrée(s) hors grille purgée(s))");
    }

    let incomplete = result.iter().filter(|e| !e.complete).count();
    if incomplete > 0 {
        println!("⚠ {incomplete} snapshot(s) avec prix manquants/reportés (voir price_status) : réessayés au prochain run");
    }

    Ok(computed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::TransactionKind::{Buy, Deposit, Sell};
    use crate::testutil::{store, tx};
    use std::cell::Cell;
    use std::path::PathBuf;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn entry(date: &str) -> HistoryEntry {
        HistoryEntry {
            date: date.to_string(),
            total_value_eur: 1.0,
            total_cost_basis_eur: 1.0,
            total_pnl_eur: 0.0,
            version: 0,
            complete: false,
            assets: Vec::new(),
        }
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
        let dates = target_dates(d(2026, 1, 15), d(2026, 4, 10), Frequency::Monthly);
        assert_eq!(dates, vec![d(2026, 2, 2), d(2026, 3, 2), d(2026, 4, 6)]);
    }

    #[test]
    fn target_dates_excludes_today_and_future() {
        // today == 1er lundi de février -> pas encore une journée complète.
        let dates = target_dates(d(2026, 1, 1), d(2026, 2, 2), Frequency::Monthly);
        assert_eq!(dates, vec![d(2026, 1, 5)]);
    }

    #[test]
    fn target_dates_handles_year_rollover() {
        let dates = target_dates(d(2025, 11, 1), d(2026, 2, 10), Frequency::Monthly);
        assert_eq!(dates, vec![d(2025, 11, 3), d(2025, 12, 1), d(2026, 1, 5), d(2026, 2, 2)]);
    }

    #[test]
    fn target_dates_empty_when_nothing_is_complete_yet() {
        assert!(target_dates(d(2026, 10, 6), d(2026, 10, 8), Frequency::Monthly).is_empty());
    }

    #[test]
    fn target_dates_are_sorted_unique_first_mondays() {
        let dates = target_dates(d(2021, 3, 17), d(2026, 10, 8), Frequency::Monthly);
        assert!(dates.windows(2).all(|w| w[0] < w[1]));
        assert!(dates.iter().all(|&date| is_first_monday(date)));
    }

    // --- retain_first_mondays ---

    #[test]
    fn retain_on_grid_monthly_purges_old_weekly_sundays_and_garbage() {
        let mut history = vec![
            entry("2026-10-04"), // dimanche (ancien format)
            entry("2026-10-05"), // 1er lundi -> gardé
            entry("2026-10-12"), // 2e lundi -> purgé
            entry("pas-une-date"),
            entry("2026-11-02"), // 1er lundi -> gardé
        ];
        let purged = retain_on_grid(&mut history, Frequency::Monthly);

        assert_eq!(purged, 3);
        let dates: Vec<&str> = history.iter().map(|e| e.date.as_str()).collect();
        assert_eq!(dates, vec!["2026-10-05", "2026-11-02"]);
    }

    // --- patch_prices / classify_prices ---

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

    #[test]
    fn classify_prices_distinguishes_ok_carried_and_missing() {
        let mut last_known = HashMap::new();
        last_known.insert("BTC".to_string(), 50_000.0);
        let assets = vec![asset("ETH", 1.0, 2_000.0), asset("BTC", 1.0, 0.0), asset("NEW", 1.0, 0.0), asset("NAN", 1.0, f64::NAN)];

        assert_eq!(
            classify_prices(&assets, &last_known),
            vec![PriceStatus::Ok, PriceStatus::Carried, PriceStatus::Missing, PriceStatus::Missing]
        );
    }

    // --- sérialisation ---

    #[test]
    fn history_entry_reads_legacy_week_end_field() {
        let json = r#"{"week_end":"2026-10-04","total_value_eur":1.0,"total_cost_basis_eur":1.0,"total_pnl_eur":0.0}"#;
        let e: HistoryEntry = serde_json::from_str(json).unwrap();
        assert_eq!(e.date, "2026-10-04");
        assert_eq!(e.version, 0); // ancien format -> sera recalculé
        assert!(!e.is_final());
    }

    #[test]
    fn history_entry_serializes_as_date() {
        let json = serde_json::to_string(&entry("2026-10-05")).unwrap();
        assert!(json.contains("\"date\":\"2026-10-05\""));
        assert!(!json.contains("week_end"));
    }

    #[test]
    fn price_status_serializes_in_snake_case() {
        assert_eq!(serde_json::to_string(&PriceStatus::Carried).unwrap(), "\"carried\"");
    }

    // --- record_history_with (mensuel) : bout en bout, sans réseau ---

    struct TmpDir(PathBuf);
    impl TmpDir {
        fn new(name: &str) -> Self {
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let p = std::env::temp_dir().join(format!("pf_history_{name}_{}_{nanos}", std::process::id()));
            std::fs::create_dir_all(&p).unwrap();
            TmpDir(p)
        }
        fn file(&self) -> PathBuf {
            self.0.join("history.json")
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Premiers lundis ciblés avec today = 2026-05-20 :
    /// 2026-01-05, 02-02, 03-02, 04-06, 05-04.
    fn today() -> NaiveDate {
        d(2026, 5, 20)
    }

    fn fixture() -> TxStore {
        store(vec![
            tx(Buy, "BTC", AssetKind::Crypto, 1.0, 40_000.0, "2026-01-02T10:00:00Z", "t1"),
            tx(Buy, "AIR", AssetKind::Stock, 10.0, 900.0, "2026-02-10T10:00:00Z", "t2"),
            // Lundi 22:00 : inclus dans le snapshot du 06/04 (23:59:59).
            tx(Sell, "BTC", AssetKind::Crypto, 0.5, 26_000.0, "2026-04-06T22:00:00Z", "t3"),
            // Mardi 00:30 : exclu du 06/04, inclus au 04/05.
            tx(Buy, "AIR", AssetKind::Stock, 5.0, 600.0, "2026-04-07T00:30:00Z", "t4"),
        ])
    }

    /// Prix qui évolue avec la date : permet de détecter un prix figé.
    fn moving_price(symbol: &str, at: DateTime<Utc>) -> f64 {
        let days = (at.date_naive() - d(2026, 1, 1)).num_days() as f64;
        match symbol {
            "BTC" => 50_000.0 + days * 10.0,
            "AIR" => 100.0 + days * 0.5,
            _ => 0.0,
        }
    }

    fn run(s: &TxStore, path: &Path, price_fn: PriceFn, rebuild: bool) -> usize {
        record_history_with(s, path, price_fn, today(), Frequency::Monthly, rebuild).unwrap()
    }

    fn read(path: &Path) -> Vec<HistoryEntry> {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn entry_at<'a>(h: &'a [HistoryEntry], date: &str) -> &'a HistoryEntry {
        h.iter().find(|e| e.date == date).unwrap_or_else(|| panic!("entrée {date} absente"))
    }

    fn asset_of<'a>(e: &'a HistoryEntry, symbol: &str) -> &'a HistoryAsset {
        e.assets.iter().find(|a| a.symbol == symbol).unwrap_or_else(|| panic!("{symbol} absent de {}", e.date))
    }

    #[test]
    fn backfill_records_one_snapshot_per_asset_per_month() {
        let tmp = TmpDir::new("per_asset");
        let price = |s: &str, _k: AssetKind, _t: Option<&str>, at: DateTime<Utc>| moving_price(s, at);

        assert_eq!(run(&fixture(), &tmp.file(), &price, false), 5);
        let h = read(&tmp.file());
        let dates: Vec<&str> = h.iter().map(|e| e.date.as_str()).collect();
        assert_eq!(dates, vec!["2026-01-05", "2026-02-02", "2026-03-02", "2026-04-06", "2026-05-04"]);

        // Janvier : BTC seul. Prix = 50 000 + 10 * 4 jours.
        let jan = entry_at(&h, "2026-01-05");
        assert_eq!(jan.assets.len(), 1);
        assert!(approx(asset_of(jan, "BTC").price_eur, 50_040.0));
        assert!(approx(asset_of(jan, "BTC").value_eur, 50_040.0));

        // Mars : BTC + AIR, triés par valeur décroissante.
        let mar = entry_at(&h, "2026-03-02");
        let order: Vec<&str> = mar.assets.iter().map(|a| a.symbol.as_str()).collect();
        assert_eq!(order, vec!["BTC", "AIR"]);
        assert!(approx(asset_of(mar, "AIR").quantity, 10.0));
        assert!(approx(asset_of(mar, "AIR").cost_basis_eur, 900.0));

        // Avril : vente du lundi 22:00 incluse (BTC 0.5, cost basis FIFO 20 000),
        // achat AIR du mardi 00:30 exclu.
        let apr = entry_at(&h, "2026-04-06");
        assert!(approx(asset_of(apr, "BTC").quantity, 0.5));
        assert!(approx(asset_of(apr, "BTC").cost_basis_eur, 20_000.0));
        assert!(approx(asset_of(apr, "AIR").quantity, 10.0));

        // Mai : l'achat du mardi est maintenant inclus.
        let may = entry_at(&h, "2026-05-04");
        assert!(approx(asset_of(may, "AIR").quantity, 15.0));
        assert!(approx(asset_of(may, "AIR").cost_basis_eur, 1_500.0));
    }

    #[test]
    fn cash_is_excluded_from_assets_and_totals() {
        // Un dépôt EUR de 12 000 (lot à coût plein) + un achat USDC jamais
        // "dépensé" : ne doivent apparaître ni dans la liste ni dans les totaux.
        let s = store(vec![
            tx(Deposit, "EUR", AssetKind::Cash, 12_000.0, 12_000.0, "2026-01-02T09:00:00Z", "c1"),
            tx(Deposit, "USDC", AssetKind::Cash, 500.0, 430.0, "2026-01-02T09:30:00Z", "c2"),
            tx(Buy, "BTC", AssetKind::Crypto, 1.0, 40_000.0, "2026-01-02T10:00:00Z", "t1"),
        ]);
        let tmp = TmpDir::new("cash");
        let price = |sym: &str, _k: AssetKind, _t: Option<&str>, at: DateTime<Utc>| match sym {
            "EUR" => 1.0,
            "USDC" => 0.86,
            other => moving_price(other, at),
        };
        record_history_with(&s, &tmp.file(), &price, today(), Frequency::Monthly, false).unwrap();

        for e in read(&tmp.file()) {
            let symbols: Vec<&str> = e.assets.iter().map(|a| a.symbol.as_str()).collect();
            assert_eq!(symbols, vec!["BTC"], "{}", e.date);
            assert!(approx(e.total_cost_basis_eur, 40_000.0), "{}", e.date);
            assert!(approx(e.total_value_eur, asset_of(&e, "BTC").value_eur), "{}", e.date);
            assert!(e.complete, "{}", e.date);
        }
    }

    #[test]
    fn cash_with_unknown_price_does_not_make_entries_incomplete() {
        let s = store(vec![
            tx(Deposit, "TUSD", AssetKind::Cash, 100.0, 90.0, "2026-01-02T09:00:00Z", "c1"),
            tx(Buy, "BTC", AssetKind::Crypto, 1.0, 40_000.0, "2026-01-02T10:00:00Z", "t1"),
        ]);
        let tmp = TmpDir::new("cash_unknown");
        let price = |sym: &str, _k: AssetKind, _t: Option<&str>, at: DateTime<Utc>| {
            if sym == "TUSD" { 0.0 } else { moving_price(sym, at) }
        };
        record_history_with(&s, &tmp.file(), &price, today(), Frequency::Monthly, false).unwrap();

        assert!(read(&tmp.file()).iter().all(|e| e.complete));
    }

    #[test]
    fn prices_move_between_months_and_are_never_frozen() {
        // Régression du bug d'origine : même prix recopié d'un mois à l'autre.
        let tmp = TmpDir::new("not_frozen");
        let price = |s: &str, _k: AssetKind, _t: Option<&str>, at: DateTime<Utc>| moving_price(s, at);
        run(&fixture(), &tmp.file(), &price, false);
        let h = read(&tmp.file());

        let btc: Vec<f64> = h.iter().map(|e| asset_of(e, "BTC").price_eur).collect();
        assert!(btc.windows(2).all(|w| w[0] != w[1]), "prix BTC figés : {btc:?}");
    }

    #[test]
    fn totals_are_always_the_sum_of_the_assets() {
        let tmp = TmpDir::new("sums");
        let price = |s: &str, _k: AssetKind, _t: Option<&str>, at: DateTime<Utc>| moving_price(s, at);
        run(&fixture(), &tmp.file(), &price, false);

        for e in read(&tmp.file()) {
            let value: f64 = e.assets.iter().map(|a| a.value_eur).sum();
            let cost: f64 = e.assets.iter().map(|a| a.cost_basis_eur).sum();
            assert!(approx(e.total_value_eur, value), "{}", e.date);
            assert!(approx(e.total_cost_basis_eur, cost), "{}", e.date);
            assert!(approx(e.total_pnl_eur, value - cost), "{}", e.date);
            assert!(e.assets.iter().all(|a| approx(a.value_eur, a.quantity * a.price_eur)), "{}", e.date);
            assert_eq!(e.version, HISTORY_ENTRY_VERSION);
            assert!(e.complete);
        }
    }

    #[test]
    fn complete_entries_are_immutable_and_trigger_no_price_lookup() {
        let tmp = TmpDir::new("immutable");
        let price = |s: &str, _k: AssetKind, _t: Option<&str>, at: DateTime<Utc>| moving_price(s, at);
        run(&fixture(), &tmp.file(), &price, false);
        let before = std::fs::read_to_string(tmp.file()).unwrap();

        let calls = Cell::new(0);
        let doubled = |s: &str, _k: AssetKind, _t: Option<&str>, at: DateTime<Utc>| {
            calls.set(calls.get() + 1);
            moving_price(s, at) * 2.0
        };
        assert_eq!(run(&fixture(), &tmp.file(), &doubled, false), 0);

        assert_eq!(calls.get(), 0);
        assert_eq!(std::fs::read_to_string(tmp.file()).unwrap(), before);
    }

    #[test]
    fn rebuild_flag_recomputes_everything() {
        let tmp = TmpDir::new("rebuild");
        let price = |s: &str, _k: AssetKind, _t: Option<&str>, at: DateTime<Utc>| moving_price(s, at);
        run(&fixture(), &tmp.file(), &price, false);

        let doubled = |s: &str, _k: AssetKind, _t: Option<&str>, at: DateTime<Utc>| moving_price(s, at) * 2.0;
        assert_eq!(run(&fixture(), &tmp.file(), &doubled, true), 5);

        let h = read(&tmp.file());
        assert!(approx(asset_of(entry_at(&h, "2026-01-05"), "BTC").price_eur, 100_080.0));
    }

    #[test]
    fn legacy_entries_without_asset_detail_are_recomputed() {
        // Ancien history.json (prix figés, pas de détail) : doit être remplacé.
        let tmp = TmpDir::new("legacy");
        std::fs::write(
            tmp.file(),
            r#"[{"date":"2026-01-05","total_value_eur":123.0,"total_cost_basis_eur":1.0,"total_pnl_eur":122.0}]"#,
        )
        .unwrap();
        let price = |s: &str, _k: AssetKind, _t: Option<&str>, at: DateTime<Utc>| moving_price(s, at);

        assert_eq!(run(&fixture(), &tmp.file(), &price, false), 5);
        let h = read(&tmp.file());
        let jan = entry_at(&h, "2026-01-05");
        assert!(approx(jan.total_value_eur, 50_040.0));
        assert_eq!(jan.assets.len(), 1);
    }

    #[test]
    fn old_weekly_sunday_entries_are_purged() {
        let tmp = TmpDir::new("purge");
        std::fs::write(
            tmp.file(),
            r#"[{"week_end":"2026-01-04","total_value_eur":1.0,"total_cost_basis_eur":1.0,"total_pnl_eur":0.0}]"#,
        )
        .unwrap();
        let price = |s: &str, _k: AssetKind, _t: Option<&str>, at: DateTime<Utc>| moving_price(s, at);
        run(&fixture(), &tmp.file(), &price, false);

        let h = read(&tmp.file());
        assert!(h.iter().all(|e| e.date != "2026-01-04"));
        assert_eq!(h.len(), 5);
    }

    #[test]
    fn failed_price_is_carried_from_previous_month_flagged_then_healed() {
        let tmp = TmpDir::new("carried");
        // BTC en panne le 02/03 uniquement.
        let flaky = |s: &str, _k: AssetKind, _t: Option<&str>, at: DateTime<Utc>| {
            if s == "BTC" && at.date_naive() == d(2026, 3, 2) { 0.0 } else { moving_price(s, at) }
        };
        run(&fixture(), &tmp.file(), &flaky, false);

        let h = read(&tmp.file());
        let feb_price = asset_of(entry_at(&h, "2026-02-02"), "BTC").price_eur;
        let mar = entry_at(&h, "2026-03-02");
        let mar_btc = asset_of(mar, "BTC");
        assert_eq!(mar_btc.price_status, PriceStatus::Carried);
        assert!(approx(mar_btc.price_eur, feb_price));
        assert!(approx(mar_btc.value_eur, feb_price)); // 1 BTC
        assert!(!mar.complete);
        assert!(entry_at(&h, "2026-02-02").complete);

        // L'API revient : seule l'entrée incomplète est recalculée.
        let healthy = |s: &str, _k: AssetKind, _t: Option<&str>, at: DateTime<Utc>| moving_price(s, at);
        assert_eq!(run(&fixture(), &tmp.file(), &healthy, false), 1);

        let h = read(&tmp.file());
        let mar = entry_at(&h, "2026-03-02");
        assert!(mar.complete);
        assert_eq!(asset_of(mar, "BTC").price_status, PriceStatus::Ok);
        assert!(approx(asset_of(mar, "BTC").price_eur, moving_price("BTC", utc_date(2026, 3, 2))));

        // Et plus rien à faire ensuite.
        assert_eq!(run(&fixture(), &tmp.file(), &healthy, false), 0);
    }

    fn utc_date(y: i32, m: u32, day: u32) -> DateTime<Utc> {
        Utc.from_utc_datetime(&d(y, m, day).and_hms_opt(23, 59, 59).unwrap())
    }

    #[test]
    fn failed_price_without_any_previous_price_is_flagged_missing() {
        let tmp = TmpDir::new("missing");
        let no_btc_in_january = |s: &str, _k: AssetKind, _t: Option<&str>, at: DateTime<Utc>| {
            if s == "BTC" && at.date_naive() == d(2026, 1, 5) { 0.0 } else { moving_price(s, at) }
        };
        run(&fixture(), &tmp.file(), &no_btc_in_january, false);

        let h = read(&tmp.file());
        let jan = entry_at(&h, "2026-01-05");
        let btc = asset_of(jan, "BTC");
        assert_eq!(btc.price_status, PriceStatus::Missing);
        assert!(approx(btc.value_eur, 0.0));
        assert!(!jan.complete);
    }

    #[test]
    fn today_that_is_a_first_monday_is_not_snapshotted_yet() {
        let tmp = TmpDir::new("today");
        let price = |s: &str, _k: AssetKind, _t: Option<&str>, at: DateTime<Utc>| moving_price(s, at);
        record_history_with(&fixture(), &tmp.file(), &price, d(2026, 5, 4), Frequency::Monthly, false).unwrap();

        let h = read(&tmp.file());
        assert_eq!(h.last().unwrap().date, "2026-04-06");
    }

    #[test]
    fn no_transactions_means_no_file() {
        let tmp = TmpDir::new("empty");
        let price = |_s: &str, _k: AssetKind, _t: Option<&str>, _at: DateTime<Utc>| 1.0;
        let n = record_history_with(&store(vec![]), &tmp.file(), &price, today(), Frequency::Weekly, false).unwrap();

        assert_eq!(n, 0);
        assert!(!tmp.file().exists());
    }

    #[test]
    fn unreadable_history_is_backed_up_then_rebuilt() {
        let tmp = TmpDir::new("corrupt");
        std::fs::write(tmp.file(), "{ pas du json").unwrap();
        let price = |s: &str, _k: AssetKind, _t: Option<&str>, at: DateTime<Utc>| moving_price(s, at);

        assert_eq!(run(&fixture(), &tmp.file(), &price, false), 5);
        let backup = tmp.0.join("history.json.bak");
        assert_eq!(std::fs::read_to_string(backup).unwrap(), "{ pas du json");
        assert_eq!(read(&tmp.file()).len(), 5);
    }

    // --- hebdomadaire ---

    #[test]
    fn weekly_target_dates_are_consecutive_complete_sundays() {
        // Début un vendredi : première date = dimanche de cette semaine.
        let dates = target_dates(d(2026, 1, 2), d(2026, 1, 28), Frequency::Weekly);
        assert_eq!(dates, vec![d(2026, 1, 4), d(2026, 1, 11), d(2026, 1, 18), d(2026, 1, 25)]);
    }

    #[test]
    fn weekly_target_dates_exclude_the_current_week_and_a_sunday_today() {
        // today = dimanche 18/01 : cette journée n'est pas terminée.
        let dates = target_dates(d(2026, 1, 2), d(2026, 1, 18), Frequency::Weekly);
        assert_eq!(dates.last(), Some(&d(2026, 1, 11)));
        // today = mercredi : la semaine en cours (dimanche à venir) est exclue.
        let dates = target_dates(d(2026, 1, 2), d(2026, 1, 21), Frequency::Weekly);
        assert_eq!(dates.last(), Some(&d(2026, 1, 18)));
    }

    #[test]
    fn weekly_target_dates_empty_when_the_first_week_is_not_over() {
        assert!(target_dates(d(2026, 1, 5), d(2026, 1, 8), Frequency::Weekly).is_empty());
    }

    #[test]
    fn weekly_target_dates_are_sorted_sundays_with_year_rollover() {
        let dates = target_dates(d(2025, 12, 20), d(2026, 1, 20), Frequency::Weekly);
        assert!(dates.windows(2).all(|w| (w[1] - w[0]).num_days() == 7));
        assert!(dates.iter().all(|x| x.weekday() == Weekday::Sun));
        assert_eq!(dates.first(), Some(&d(2025, 12, 21)));
        assert_eq!(dates.last(), Some(&d(2026, 1, 18)));
    }

    #[test]
    fn retain_on_grid_weekly_keeps_sundays_and_purges_mondays() {
        let mut history = vec![entry("2026-01-04"), entry("2026-01-05"), entry("n'importe quoi")];
        let purged = retain_on_grid(&mut history, Frequency::Weekly);

        assert_eq!(purged, 2);
        assert_eq!(history[0].date, "2026-01-04");
    }

    /// Dimanches ciblés avec today = 2026-05-20 : du 2026-01-04 au 2026-05-17 (20).
    fn weekly_fixture() -> TxStore {
        store(vec![
            tx(Buy, "BTC", AssetKind::Crypto, 1.0, 40_000.0, "2026-01-02T10:00:00Z", "w1"),
            tx(Buy, "AIR", AssetKind::Stock, 10.0, 900.0, "2026-02-10T10:00:00Z", "w2"),
            // Dimanche 22:00 : inclus dans le snapshot du 05/04 (23:59:59).
            tx(Sell, "BTC", AssetKind::Crypto, 0.5, 26_000.0, "2026-04-05T22:00:00Z", "w3"),
            // Lundi 00:30 : exclu du 05/04, inclus au 12/04.
            tx(Buy, "AIR", AssetKind::Stock, 5.0, 600.0, "2026-04-06T00:30:00Z", "w4"),
        ])
    }

    fn run_weekly(s: &TxStore, path: &Path, price_fn: PriceFn, rebuild: bool) -> usize {
        record_history_with(s, path, price_fn, today(), Frequency::Weekly, rebuild).unwrap()
    }

    fn moving() -> impl Fn(&str, AssetKind, Option<&str>, DateTime<Utc>) -> f64 {
        |s: &str, _k: AssetKind, _t: Option<&str>, at: DateTime<Utc>| moving_price(s, at)
    }

    #[test]
    fn weekly_backfill_records_one_snapshot_per_asset_per_week() {
        let tmp = TmpDir::new("weekly");
        let price = moving();

        assert_eq!(run_weekly(&weekly_fixture(), &tmp.file(), &price, false), 20);
        let h = read(&tmp.file());
        assert_eq!(h.len(), 20);
        assert_eq!(h.first().unwrap().date, "2026-01-04");
        assert_eq!(h.last().unwrap().date, "2026-05-17");
        assert!(h.windows(2).all(|w| w[0].date < w[1].date));

        // Première semaine : BTC seul, prix = 50 000 + 10 * 3 jours.
        let first = entry_at(&h, "2026-01-04");
        assert_eq!(first.assets.len(), 1);
        assert!(approx(asset_of(first, "BTC").price_eur, 50_030.0));

        // AIR absent avant l'achat du mardi 10/02, présent le dimanche 15/02.
        assert!(entry_at(&h, "2026-02-08").assets.iter().all(|a| a.symbol != "AIR"));
        assert!(approx(asset_of(entry_at(&h, "2026-02-15"), "AIR").quantity, 10.0));
    }

    #[test]
    fn weekly_snapshot_is_taken_at_the_end_of_sunday() {
        let tmp = TmpDir::new("weekly_boundary");
        run_weekly(&weekly_fixture(), &tmp.file(), &moving(), false);
        let h = read(&tmp.file());

        // 05/04 : vente du dimanche 22:00 incluse, achat du lundi 00:30 exclu.
        let sun = entry_at(&h, "2026-04-05");
        assert!(approx(asset_of(sun, "BTC").quantity, 0.5));
        assert!(approx(asset_of(sun, "BTC").cost_basis_eur, 20_000.0));
        assert!(approx(asset_of(sun, "AIR").quantity, 10.0));

        // 12/04 : l'achat du lundi apparaît.
        let next = entry_at(&h, "2026-04-12");
        assert!(approx(asset_of(next, "AIR").quantity, 15.0));
        assert!(approx(asset_of(next, "AIR").cost_basis_eur, 1_500.0));
    }

    #[test]
    fn weekly_prices_change_every_week_and_totals_match_assets() {
        let tmp = TmpDir::new("weekly_prices");
        run_weekly(&weekly_fixture(), &tmp.file(), &moving(), false);
        let h = read(&tmp.file());

        let btc: Vec<f64> = h.iter().map(|e| asset_of(e, "BTC").price_eur).collect();
        assert!(btc.windows(2).all(|w| w[0] != w[1]), "prix BTC figés : {btc:?}");

        for e in &h {
            let value: f64 = e.assets.iter().map(|a| a.value_eur).sum();
            let cost: f64 = e.assets.iter().map(|a| a.cost_basis_eur).sum();
            assert!(approx(e.total_value_eur, value), "{}", e.date);
            assert!(approx(e.total_pnl_eur, value - cost), "{}", e.date);
        }
    }

    #[test]
    fn weekly_rerun_is_idempotent_and_only_adds_the_new_week() {
        let tmp = TmpDir::new("weekly_incremental");
        let price = moving();
        run_weekly(&weekly_fixture(), &tmp.file(), &price, false);
        let before = std::fs::read_to_string(tmp.file()).unwrap();

        assert_eq!(run_weekly(&weekly_fixture(), &tmp.file(), &price, false), 0);
        assert_eq!(std::fs::read_to_string(tmp.file()).unwrap(), before);

        // Une semaine plus tard : exactement un nouveau dimanche (24/05).
        let n = record_history_with(&weekly_fixture(), &tmp.file(), &price, d(2026, 5, 27), Frequency::Weekly, false).unwrap();
        assert_eq!(n, 1);
        assert_eq!(read(&tmp.file()).last().unwrap().date, "2026-05-24");
    }

    #[test]
    fn switching_frequency_purges_the_other_grid_and_recomputes() {
        let tmp = TmpDir::new("switch");
        let price = moving();
        run(&fixture(), &tmp.file(), &price, false); // mensuel : 5 lundis

        assert_eq!(run_weekly(&weekly_fixture(), &tmp.file(), &price, false), 20);
        let h = read(&tmp.file());
        assert!(h.iter().all(|e| NaiveDate::parse_from_str(&e.date, "%Y-%m-%d").unwrap().weekday() == Weekday::Sun));
    }

    #[test]
    fn weekly_failed_price_is_carried_from_the_previous_week_then_healed() {
        let tmp = TmpDir::new("weekly_carried");
        let flaky = |s: &str, _k: AssetKind, _t: Option<&str>, at: DateTime<Utc>| {
            if s == "BTC" && at.date_naive() == d(2026, 3, 1) { 0.0 } else { moving_price(s, at) }
        };
        run_weekly(&weekly_fixture(), &tmp.file(), &flaky, false);

        let h = read(&tmp.file());
        let prev = asset_of(entry_at(&h, "2026-02-22"), "BTC").price_eur;
        let bad = asset_of(entry_at(&h, "2026-03-01"), "BTC");
        assert_eq!(bad.price_status, PriceStatus::Carried);
        assert!(approx(bad.price_eur, prev));
        assert!(!entry_at(&h, "2026-03-01").complete);

        assert_eq!(run_weekly(&weekly_fixture(), &tmp.file(), &moving(), false), 1);
        assert!(entry_at(&read(&tmp.file()), "2026-03-01").complete);
    }
}