//! Portage de src/cli.py -- construit (ou met à jour) serialized_tx.json
//! à partir des exports bruts dans ./data/raw/.
//!
//! Ré-exécutable sans risque : le tx_store est recréé from scratch à chaque run.
//! Le cache des prix (price_cache.bin) persiste et bloque les appels API inutiles.
//!
//! `main` ne fait qu'enchaîner des étapes ; chacune vit dans sa fonction :
//! construction du wallet -> tickers -> persistance -> valorisation
//! (affichage) -> corrélations -> dashboard.json.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::Utc;
use serde::Serialize;

use core::devtools;
use core::ledger::cash::cash_summary;
use core::ledger::cost_basis::{compute_fifo, CostBasisResult, RealizedGain};
use core::ledger::history::record_weekly_history;
use core::ledger::holding::{closed_sales, holding_stats};
use core::ledger::portfolio::{portfolio_snapshot_at, AssetSnapshot, PortfolioSnapshot};
use core::ledger::positions::non_zero_holdings_at;
use core::ledger::realized::{realized_summary, TaxAccount};
use core::ledger::trades::{build_trade_frequency, build_trades};
use core::market::MarketDataClient; // <-- NOUVEAU SERVICE INJECTÉ
use core::market::correlation::{compute_correlation_matrices, CorrelationMatrices};
use core::parse::{binance, manual, xtb};
use core::schema::{AssetKind, CashSummary, ClosedSale, DashboardAsset, DashboardData, HoldingStats, TransactionKind};
use core::store::serialize::{save_wallet, TxStore};

const CORRELATION_MIN_VALUE_EUR: f64 = 10.0;

/// Symboles masqués dans le tableau de valorisation (poussière / hors suivi).
const HIDDEN_SYMBOLS: &[&str] = &["USDC", "SOL", "ALGO"];

fn data_dir() -> PathBuf {
    PathBuf::from("./data/raw")
}

fn accounts_path() -> PathBuf {
    data_dir().join("accounts")
}

fn main() -> Result<()> {
    let cache_dir = PathBuf::from("./data/cache");
    
    // 1. Initialisation du client de données de marché (remplace les globals)
    let mut market_client = MarketDataClient::new();
    market_client.init_caches(&cache_dir);

    // 2. Construction du wallet (le client est passé aux parseurs pour valoriser en EUR à l'import)
    let mut tx_store = build_wallet(&market_client)?;
    warn_on_invalid_quantities(&tx_store);
    resolve_missing_tickers(&mut tx_store, &market_client);

    save_wallet(&tx_store, Path::new("./data/tx_store.json"))?;
    println!("tx_store.json regénéré.");

    // 3. Historique (le client est passé pour récupérer les prix passés)
    record_weekly_history(&tx_store, Path::new("./data/history.json"), &market_client)?;

    // 4. Valorisation actuelle et FIFO
    let snapshot = portfolio_snapshot_at(&tx_store, None, &market_client);
    let cost_basis = compute_fifo(&tx_store, None)?;

    let latent_pnl = print_valuation(&snapshot, &cost_basis);
    let realized_pnl = cost_basis.total_realized_pnl_stocks(&tx_store);
    print_performance(latent_pnl, realized_pnl);
    print_realized_by_stock(&cost_basis, &tx_store);

    // Compte-titres seul : XTB hors PEA, FIFO refait sur ce compte (voir realized.rs).
    println!();
    print!(
        "{}",
        realized_summary(&tx_store, TaxAccount::Cto)?
            .report("PLUS-VALUES RÉALISÉES : COMPTE-TITRES (XTB, hors PEA et Binance)")
    );

    print_cash(&cash_summary(&tx_store, None));

    // Durée de détention (lots FIFO) et ventes de titres
    let (holdings, sales) = compute_holdings(&tx_store, &cost_basis, &snapshot);

    // 5. Corrélations et Dashboard
    let correlations = compute_correlations(&tx_store, &snapshot, &market_client);
    let dashboard = build_dashboard(&tx_store, &snapshot, &cost_basis, correlations, holdings, sales);
    let dashboard_path = PathBuf::from("./data/dashboard.json");
    write_json(&dashboard_path, &dashboard)?;
    println!("Dashboard écrit : {dashboard_path:?}");

    // 6. Sauvegarde des caches sur disque avant de quitter
    market_client.save_caches(&cache_dir);
    println!("Cache des prix 1h/1j sauvegardé : {cache_dir:?}");

    devtools::dump_for_llm()
}

// ---------------------------------------------------------------------
// Construction du wallet
// ---------------------------------------------------------------------

/// Parse toutes les sources (Binance, XTB CTO + PEA, saisies manuelles).
fn build_wallet(market_client: &MarketDataClient) -> Result<TxStore> {
    println!("=== CONSTRUCTION DU WALLET ===");
    let accounts = accounts_path();
    let mut tx_store = TxStore::new();

    // On passe le client aux parseurs qui en ont besoin (pour valoriser en EUR lors de l'import)
    tx_store.add_transactions(binance::parse_binance_sources(accounts.clone(), market_client));
    for file in ["account.xlsx", "account_pea.xlsx"] {
        tx_store.add_transactions(xtb::parse_xtb_file(&accounts.join(file), market_client));
    }
    tx_store.add_transactions(manual::parse_manual(&data_dir().join("manual_tx.json"))?);

    Ok(tx_store)
}

/// Un BUY/DEPOSIT à quantité <= 0 fait échouer le FIFO : on le signale ici.
fn warn_on_invalid_quantities(tx_store: &TxStore) {
    println!("\n=== DIAGNOSTIC : BUY/DEPOSIT à quantité négative ===");
    for tx in &tx_store.transactions {
        if matches!(tx.kind, TransactionKind::Buy | TransactionKind::Deposit) && tx.quantity <= 0.0 {
            println!(
                "  platform={:?} asset={} qty={} value_eur={} time={} source={} external_id={:?} remark={:?}",
                tx.platform, tx.asset.symbol, tx.quantity, tx.value_eur, tx.time, tx.source_file, tx.external_id, tx.remark
            );
        }
    }
}

/// Renseigne `identifiers.ticker` des actifs qui n'en ont pas (hors cash).
fn resolve_missing_tickers(tx_store: &mut TxStore, market_client: &MarketDataClient) {
    println!("\n=== RÉSOLUTION DES TICKERS ===");
    let (mut resolved, mut skipped, mut failed) = (0, 0, 0);

    let candidates: Vec<(String, AssetKind, bool)> = tx_store
        .assets
        .iter()
        .map(|(symbol, asset)| (symbol.clone(), asset.kind, asset.identifiers.ticker.is_some()))
        .collect();

    for (symbol, kind, has_ticker) in candidates {
        if kind == AssetKind::Cash || has_ticker {
            skipped += 1;
            continue;
        }
        // Utilisation de la méthode du client injecté
        match market_client.resolve_ticker(&symbol, kind.to_market_kind()) {
            Some(ticker) => {
                println!("  ✓ {symbol:<12} -> {ticker}");
                if let Some(asset) = tx_store.assets.get_mut(&symbol) {
                    asset.identifiers.ticker = Some(ticker);
                }
                resolved += 1;
            }
            None => {
                println!("  ✗ {symbol:<12} : ticker introuvable");
                failed += 1;
            }
        }
    }
    println!("({resolved} résolus, {skipped} ignorés, {failed} échoués)");
}

// ---------------------------------------------------------------------
// Affichage terminal
// ---------------------------------------------------------------------

fn is_displayed(asset: &AssetSnapshot) -> bool {
    asset.value_eur > 0.01 && asset.kind != AssetKind::Cash && !HIDDEN_SYMBOLS.contains(&asset.symbol.as_str())
}

/// Colonnes (cost basis, P&L, P&L %) d'une ligne, et le P&L à cumuler.
fn pnl_columns(asset: &AssetSnapshot, cost_basis: &CostBasisResult) -> (String, String, String, f64) {
    if cost_basis.average_cost(&asset.symbol).is_none() {
        return (format!("{:>11}", "—"), format!("{:>11}", "—"), format!("{:>8}", "—"), 0.0);
    }
    let cb_total = cost_basis.open_cost_basis(&asset.symbol);
    let pnl_eur = asset.value_eur - cb_total;
    let pnl_pct = if cb_total > 0.0 { pnl_eur / cb_total * 100.0 } else { 0.0 };
    (format!("{cb_total:>11.2}"), format!("{pnl_eur:>+11.2}"), format!("{pnl_pct:>+7.2}%"), pnl_eur)
}

/// Tableau de valorisation. Renvoie le P&L latent total affiché.
fn print_valuation(snapshot: &PortfolioSnapshot, cost_basis: &CostBasisResult) -> f64 {
    println!("\n=== VALORISATION ACTUELLE ===");
    println!("Date: {}", snapshot.date);
    println!("Valeur totale: {:.2} EUR", snapshot.total_value_eur);
    println!("Détail par actif:");
    println!(
        "  {:<10} {:>10} {:>10} {:>12} {:>12} {:>12} {:>8}",
        "Symbole", "Qty", "Prix", "Valeur", "Cost basis", "P&L", "P&L %"
    );

    let mut total_pnl = 0.0;
    for asset in snapshot.assets.iter().filter(|a| is_displayed(a)) {
        let (cb, pnl, pct, pnl_eur) = pnl_columns(asset, cost_basis);
        total_pnl += pnl_eur;
        println!(
            "  {:<10} {:>10.4} {:>10.2} {:>12.2} {} {} {}",
            asset.symbol, asset.quantity, asset.price_eur, asset.value_eur, cb, pnl, pct
        );
    }

    println!("\nP&L latent total : {total_pnl:>+.2} EUR");
    total_pnl
}

fn print_cash(c: &CashSummary) {
    println!("\n=== CASH XTB ===");
    for (label, p) in [("CTO", c.cto), ("PEA", c.pea), ("Total", c.total)] {
        println!("  {label:<6} cash {:>10.2} EUR   apports nets {:>10.2} EUR", p.cash_eur, p.net_contributions_eur);
    }
    println!("  Compare avec le solde XTB : un écart indique un type d'opération ignoré (ex. PEA deposit).");
}

fn print_performance(latent_pnl: f64, realized_pnl: f64) {
    println!("P&L réalisé total (actions uniquement) : {realized_pnl:>+.2} EUR");
    println!("\n=== PERFORMANCE GLOBALE (latent + réalisé actions) ===");
    println!(
        "Overall P&L : {:>+.2} EUR  (latent {latent_pnl:>+.2} + réalisé {realized_pnl:>+.2})",
        latent_pnl + realized_pnl
    );
}

fn print_realized_by_stock(cost_basis: &CostBasisResult, tx_store: &TxStore) {
    println!("\nDétail P&L réalisé par action :");
    println!("  {:<10} {:>10} {:>12} {:>12} {:>12}", "Symbole", "Qty", "Produit", "Coût", "P&L");

    // BTreeMap : symboles déjà triés.
    let mut by_symbol: BTreeMap<&str, Vec<&RealizedGain>> = BTreeMap::new();
    for gain in cost_basis.stock_realized_gains(tx_store) {
        by_symbol.entry(gain.symbol.as_str()).or_default().push(gain);
    }

    for (symbol, gains) in by_symbol {
        let qty: f64 = gains.iter().map(|g| g.quantity).sum();
        let proceeds: f64 = gains.iter().map(|g| g.proceeds_eur).sum();
        let cost: f64 = gains.iter().map(|g| g.cost_eur).sum();
        let pnl: f64 = gains.iter().map(|g| g.pnl_eur).sum();
        let flag = if gains.iter().any(|g| g.incomplete) { "  ⚠ incomplet" } else { "" };

        println!("  {symbol:<10} {qty:>10.4} {proceeds:>12.2} {cost:>12.2} {pnl:>+12.2}{flag}");
    }
}

// ---------------------------------------------------------------------
// Corrélations et dashboard.json
// ---------------------------------------------------------------------

fn compute_correlations(tx_store: &TxStore, snapshot: &PortfolioSnapshot, market_client: &MarketDataClient) -> CorrelationMatrices {
    println!("\n=== CALCUL DE LA CORRÉLATION (90j / 6m / 1an, seuil {CORRELATION_MIN_VALUE_EUR}€) ===");
    let holdings = non_zero_holdings_at(tx_store, None);
    let prices_eur: HashMap<String, f64> =
        snapshot.assets.iter().map(|a| (a.symbol.clone(), a.price_eur)).collect();
    compute_correlation_matrices(tx_store, &holdings, &prices_eur, CORRELATION_MIN_VALUE_EUR, market_client)
}

fn dashboard_asset(asset: &AssetSnapshot, cost_basis: &CostBasisResult) -> DashboardAsset {
    let cost_basis_eur = cost_basis.open_cost_basis(&asset.symbol);
    let pnl_eur = asset.value_eur - cost_basis_eur;
    let pnl_pct = if cost_basis_eur > 0.0 { pnl_eur / cost_basis_eur * 100.0 } else { 0.0 };

    DashboardAsset {
        symbol: asset.symbol.clone(),
        kind: asset.kind.as_str().to_string(),
        ticker: asset.ticker.clone(),
        quantity: asset.quantity,
        price_eur: asset.price_eur,
        value_eur: asset.value_eur,
        cost_basis_eur,
        pnl_eur,
        pnl_pct,
        realized_pnl_eur: cost_basis.total_realized_pnl(Some(&asset.symbol)),
        realized_cost_eur: cost_basis.total_realized_cost(Some(&asset.symbol)),
    }
}

fn build_dashboard(
    tx_store: &TxStore,
    snapshot: &PortfolioSnapshot,
    cost_basis: &CostBasisResult,
    correlation_matrices: CorrelationMatrices,
    holdings: Vec<HoldingStats>,
    sales: Vec<ClosedSale>,
) -> DashboardData {
    let assets: Vec<DashboardAsset> = snapshot.assets.iter().map(|a| dashboard_asset(a, cost_basis)).collect();
    let total_cost_basis_eur: f64 = assets.iter().map(|a| a.cost_basis_eur).sum();
    let stocks_value_eur: f64 = snapshot
        .assets
        .iter()
        .filter(|a| a.kind == AssetKind::Stock)
        .map(|a| a.value_eur)
        .sum();

    DashboardData {
        total_value_eur: snapshot.total_value_eur,
        stocks_value_eur,
        total_cost_basis_eur,
        total_pnl_eur: snapshot.total_value_eur - total_cost_basis_eur,
        realized_pnl_eur: cost_basis.total_realized_pnl_stocks(tx_store),
        trades: build_trades(tx_store),
        trade_frequency: build_trade_frequency(tx_store),
        assets,
        correlation_matrices,
        holdings,
        closed_sales: sales,
        cash: cash_summary(tx_store, None),
    }
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    fs::write(path, serde_json::to_string_pretty(value)?)?;
    Ok(())
}

fn compute_holdings(
    tx_store: &TxStore,
    cost_basis: &CostBasisResult,
    snapshot: &PortfolioSnapshot,
) -> (Vec<HoldingStats>, Vec<ClosedSale>) {
    println!("\n=== DURÉE DE DÉTENTION (FIFO) ===");
    let kind_of = |symbol: &str| tx_store.assets.get(symbol).map(|a| a.kind);
    let prices: HashMap<&str, f64> =
        snapshot.assets.iter().map(|a| (a.symbol.as_str(), a.price_eur)).collect();
    // Un prix à 0 est un prix manquant : le lot n'est alors pas valorisé.
    let price_of = |symbol: &str| prices.get(symbol).copied().filter(|p| *p > 0.0);

    let holdings = holding_stats(cost_basis, Utc::now(), &kind_of, &price_of);
    print_holdings(&holdings);
    (holdings, closed_sales(cost_basis, &kind_of))
}

fn fmt_days(days: Option<f64>) -> String {
    days.map_or("—".to_string(), |d| format!("{d:.0} j"))
}

fn print_holdings(stats: &[HoldingStats]) {
    println!(
        "  {:<10} {:>12} {:>12} {:>10} {:>10} {:>8} {:>10}",
        "Symbole", "1er achat", "Capital", "Moyenne", "Plus vieux", "Vendus", "Moy. vendus"
    );
    for s in stats {
        println!(
            "  {:<10} {:>12} {:>12.2} {:>10} {:>10} {:>8} {:>10}{}",
            s.symbol,
            s.first_acquired.as_deref().unwrap_or("—"),
            s.open_cost_eur,
            fmt_days(s.open_avg_days),
            fmt_days(s.open_oldest_days),
            s.closed_lots,
            fmt_days(s.closed_avg_days),
            if s.closed_incomplete { "  ⚠ incomplet" } else { "" }
        );
    }
}