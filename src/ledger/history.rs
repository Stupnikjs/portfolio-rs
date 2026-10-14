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
//! Le P&L total d'une entrée = latent (valeur - cost basis des lots ouverts)
//! + plus-values RÉALISÉES CUMULÉES jusqu'à la date du snapshot. Sans le
//! réalisé, une vente fait disparaître son gain de toutes les courbes (le
//! produit part en cash, exclu de l'historique). Seules les actions
//! (`AssetKind::Stock`) comptent, comme dans `main.rs` : les WITHDRAW crypto
//! sont vus comme des ventes FIFO et fausseraient le résultat.
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
use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Utc, Weekday};
use serde::{Deserialize, Serialize};

use crate::ledger::cost_basis::compute_fifo;
use crate::ledger::periods::{first_monday, is_first_monday, months_between, sundays_from};
use crate::ledger::portfolio::{portfolio_snapshot_with, AssetSnapshot, PriceFn};
use crate::market::client::MarketDataClient;
use crate::schema::AssetKind;
use crate::store::serialize::TxStore;

/// Fréquence des snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frequency {
    Weekly,
    Monthly,
}

impl Frequency {
    fn label(self) -> &'static str {
        match self {
            Frequency::Weekly => "HEBDOMADAIRE (dimanche)",
            Frequency::Monthly => "MENSUEL (1er lundi)",
        }
    }

    fn is_grid_date(self, date: NaiveDate) -> bool {
        match self {
            Frequency::Weekly => date.weekday() == Weekday::Sun,
            Frequency::Monthly => is_first_monday(date),
        }
    }
}

pub const HISTORY_ENTRY_VERSION: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceStatus {
    #[default]
    Ok,
    Carried,
    Missing,
}

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
    #[serde(alias = "week_end")]
    pub date: String,
    pub total_value_eur: f64,
    pub total_cost_basis_eur: f64,
    pub total_pnl_eur: f64,
    #[serde(default)]
    pub unrealized_pnl_eur: f64,
    #[serde(default)]
    pub realized_pnl_eur: f64,
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub complete: bool,
    #[serde(default)]
    pub assets: Vec<HistoryAsset>,
}

impl HistoryEntry {
    fn is_final(&self) -> bool {
        self.version >= HISTORY_ENTRY_VERSION && self.complete
    }
}

pub fn target_dates(start: NaiveDate, today: NaiveDate, freq: Frequency) -> Vec<NaiveDate> {
    match freq {
        Frequency::Weekly => weekly_dates(start, today),
        Frequency::Monthly => monthly_dates(start, today),
    }
}

fn weekly_dates(start: NaiveDate, today: NaiveDate) -> Vec<NaiveDate> {
    sundays_from(start).take_while(|d| *d < today).collect()
}

fn monthly_dates(start: NaiveDate, today: NaiveDate) -> Vec<NaiveDate> {
    months_between(start, today)
        .map(|(y, m)| first_monday(y, m))
        .filter(|d| *d >= start && *d < today)
        .collect()
}

fn retain_on_grid(history: &mut Vec<HistoryEntry>, freq: Frequency) -> usize {
    let before = history.len();
    history.retain(|e| NaiveDate::parse_from_str(&e.date, "%Y-%m-%d").map_or(false, |d| freq.is_grid_date(d)));
    before - history.len()
}

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
            let backup = path.with_extension("json.bak");
            eprintln!("[WARN history] {path:?} illisible ({e}) : copie dans {backup:?}, reconstruction complète");
            std::fs::copy(path, &backup).with_context(|| format!("sauvegarde vers {backup:?}"))?;
            Ok(Vec::new())
        }
    }
}

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

fn build_entry(
    tx_store: &TxStore,
    target: NaiveDate,
    price_fn: PriceFn,
    last_known: &mut HashMap<String, f64>,
) -> Result<HistoryEntry> {
    let at = Utc.from_utc_datetime(&target.and_hms_opt(23, 59, 59).unwrap());

    let mut snapshot = portfolio_snapshot_with(tx_store, Some(at), price_fn);
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

    let realized_pnl_eur = cost_basis.total_realized_pnl_stocks(tx_store);
    let unrealized_pnl_eur = total_value_eur - total_cost_basis_eur;

    Ok(HistoryEntry {
        date: target.format("%Y-%m-%d").to_string(),
        total_value_eur,
        total_cost_basis_eur,
        total_pnl_eur: unrealized_pnl_eur + realized_pnl_eur,
        unrealized_pnl_eur,
        realized_pnl_eur,
        version: HISTORY_ENTRY_VERSION,
        complete,
        assets,
    })
}

pub fn record_weekly_history(tx_store: &TxStore, path: &Path, market_client: &MarketDataClient) -> Result<()> {
    record_history(tx_store, path, Frequency::Weekly, market_client)
}

pub fn record_monthly_history(tx_store: &TxStore, path: &Path, market_client: &MarketDataClient) -> Result<()> {
    record_history(tx_store, path, Frequency::Monthly, market_client)
}

pub fn record_history(tx_store: &TxStore, path: &Path, freq: Frequency, market_client: &MarketDataClient) -> Result<()> {
    let rebuild = std::env::args().any(|a| a == "--rebuild-history");
    let live_prices = |symbol: &str, kind: AssetKind, ticker: Option<&str>, at: DateTime<Utc>| {
        market_client.historical_price_eur(symbol, at, kind, ticker)
    };
    record_history_with(tx_store, path, &live_prices, Utc::now().date_naive(), freq, rebuild)?;
    Ok(())
}

pub fn record_history_with(
    tx_store: &TxStore,
    path: &Path,
    price_fn: PriceFn,
    today: NaiveDate,
    freq: Frequency,
    rebuild: bool,
) -> Result<usize> {
    let Some(start_date) = earliest_tx_date(tx_store) else {
        return Ok(0);
    };

    let mut history = if rebuild { Vec::new() } else { load_history(path)? };
    let purged = retain_on_grid(&mut history, freq);
    let mut existing: HashMap<String, HistoryEntry> =
        history.into_iter().map(|e| (e.date.clone(), e)).collect();

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

        for a in &entry.assets {
            if a.price_eur > 0.0 {
                last_known_prices.insert(a.symbol.clone(), a.price_eur);
            }
        }
        result.push(entry);
    }

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