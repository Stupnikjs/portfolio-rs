//! Portage de src/cli.py -- construit (ou met à jour) serialized_tx.json
//! à partir des exports bruts dans ./data/raw/.
//!
//! Ré-exécutable sans risque : le dédoublonnage par external_id garantit
//! qu'un même fichier réimporté ne crée pas de doublons.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;

use portfolio_rs::ledger::cost_basis::compute_fifo;
use portfolio_rs::ledger::portfolio::portfolio_snapshot_at;
use portfolio_rs::market::tickers::resolve_ticker;
use portfolio_rs::parse::{binance, manual, xtb};
use portfolio_rs::schema::{AssetKind, Platform, Transaction, TransactionKind};
use portfolio_rs::store::serialize::{load_tx_store, save_wallet};

fn data_dir() -> PathBuf {
    PathBuf::from("./data/raw")
}

fn accounts_path() -> PathBuf {
    data_dir().join("accounts")
}

fn parse_binance_sources(known_tx: &HashMap<String, Transaction>) -> Vec<Transaction> {
    let mut out = Vec::new();
    let trades_path = accounts_path().join("trades.csv");
    let converts_path = accounts_path().join("convert.csv");

    if trades_path.exists() {
        println!("Lecture Binance Trades : {trades_path:?}");
        match binance::parse_trades(&trades_path, known_tx) {
            Ok(mut tx) => out.append(&mut tx),
            Err(e) => eprintln!("  [Erreur Binance Trades] {e}"),
        }
    } else {
        println!("[Omis] Fichier introuvable : {trades_path:?}");
    }

    if converts_path.exists() {
        println!("Lecture Binance Converts : {converts_path:?}");
        match binance::parse_converts(&converts_path, known_tx) {
            Ok(mut tx) => out.append(&mut tx),
            Err(e) => eprintln!("  [Erreur Binance Converts] {e}"),
        }
    } else {
        println!("[Omis] Fichier introuvable : {converts_path:?}");
    }

    out
}

fn parse_xtb_file(path: &Path, known_tx: &HashMap<String, Transaction>) -> Vec<Transaction> {
    let mut out = Vec::new();
    if !path.exists() {
        println!("[Omis] Fichier introuvable : {path:?}");
        return out;
    }

    println!("Lecture XTB : {path:?}");

    match xtb::find_sheet_by_prefix(path, "Closed Position") {
        Ok(sheet) => match xtb::parse_closed_positions(path, &sheet) {
            Ok(positions) => {
                for pos in positions {
                    out.extend(pos.to_transactions());
                }
            }
            Err(e) => println!("  [Erreur XTB Closed] {e}"),
        },
        Err(e) => println!("  [Erreur XTB Closed] {e}"),
    }

    match xtb::find_sheet_by_prefix(path, "Open Position") {
        Ok(sheet) => match xtb::parse_open_positions(path, &sheet, known_tx) {
            Ok(positions) => out.extend(positions.iter().map(|p| p.to_transaction())),
            Err(e) => println!("  [Erreur XTB Open] {e}"),
        },
        Err(e) => println!("  [Erreur XTB Open] {e}"),
    }

    match xtb::find_sheet_by_prefix(path, "Cash") {
        Ok(sheet) => match xtb::parse_cash_operations(path, &sheet) {
            Ok(mut tx) => out.append(&mut tx),
            Err(e) => println!("  [Erreur XTB Cash] {e}"),
        },
        Err(e) => println!("  [Erreur XTB Cash] {e}"),
    }

    out
}

fn main() -> Result<()> {
    println!("=== CONSTRUCTION DU WALLET ===");

    let tx_store_path = PathBuf::from("./data/tx_store.json");
    let mut tx_store = load_tx_store(&tx_store_path)?;
    println!("Wallet chargé : {} transaction(s) existante(s)", tx_store.transactions.len());

    // --- NOUVEAU : Création du cache des transactions existantes ---
    // Permet d'éviter de refetcher les prix API pour les TX déjà connues
    let known_tx: HashMap<String, Transaction> = tx_store
        .transactions
        .iter()
        .filter_map(|tx| tx.external_id.clone().map(|id| (id, tx.clone())))
        .collect();

    let new_transactions = parse_binance_sources(&known_tx);
    let mut xtb_tx = Vec::new();
    xtb_tx.extend(parse_xtb_file(&accounts_path().join("account.xlsx"), &known_tx));
    xtb_tx.extend(parse_xtb_file(&accounts_path().join("account_pea.xlsx"), &known_tx));

    tx_store.add_transactions(new_transactions);
    tx_store.add_transactions(xtb_tx.clone());

    println!("\n=== DIAGNOSTIC : BUY/DEPOSIT à quantité négative ===");
    for tx in &tx_store.transactions {
        if matches!(tx.kind, TransactionKind::Buy | TransactionKind::Deposit) && tx.quantity <= 0.0 {
            println!(
                "  platform={:?} asset={} qty={} value_eur={} time={} source={} external_id={:?} remark={:?}",
                tx.platform, tx.asset.symbol, tx.quantity, tx.value_eur, tx.time, tx.source_file, tx.external_id, tx.remark
            );
        }
    }

    let replaced = tx_store.replace_platform(Platform::Xtb, xtb_tx);
    println!("XTB : {replaced} transaction(s) (remplacement complet)");

    let manual_tx = manual::parse_manual(&data_dir().join("manual_tx.json"))?;
    tx_store.replace_platform(Platform::Manual, manual_tx);

    // --- RÉSOLUTION DES TICKERS MANQUANTS ---
    println!("\n=== RÉSOLUTION DES TICKERS ===");
    let mut resolved = 0;
    let mut skipped = 0;
    let mut failed = 0;

    let symbols: Vec<String> = tx_store.assets.keys().cloned().collect();
    for symbol in symbols {
        let (kind, has_ticker) = {
            let asset = tx_store.assets.get(&symbol).unwrap();
            (asset.kind, asset.identifiers.ticker.is_some())
        };

        if kind == AssetKind::Cash || has_ticker {
            skipped += 1;
            continue;
        }

        match resolve_ticker(&symbol, kind) {
            Some(ticker) => {
                println!("  ✓ {symbol:<12} -> {ticker}");
                tx_store.assets.get_mut(&symbol).unwrap().identifiers.ticker = Some(ticker);
                resolved += 1;
            }
            None => {
                println!("  ✗ {symbol:<12} : ticker introuvable");
                failed += 1;
            }
        }
    }
    println!("({resolved} résolus, {skipped} ignorés, {failed} échoués)");

    save_wallet(&tx_store, &tx_store_path)?;

    println!("\n=== VALORISATION ACTUELLE ===");
    let snapshot = portfolio_snapshot_at(&tx_store, None);
    let cost_basis = compute_fifo(&tx_store, None)?;

    println!("Date: {}", snapshot.date);
    println!("Valeur totale: {:.2} EUR", snapshot.total_value_eur);
    println!("Détail par actif:");
    println!(
        "  {:<10} {:>10} {:>10} {:>12} {:>12} {:>12} {:>8}",
        "Symbole", "Qty", "Prix", "Valeur", "Cost basis", "P&L", "P&L %"
    );

    let mut total_pnl = 0.0;
    for asset in &snapshot.assets {
        if asset.value_eur <= 0.01 || matches!(asset.symbol.as_str(), "USDC" | "SOL" | "ALGO") {
            continue; // on cache les poussières
        }

        let avg_cost = cost_basis.average_cost(&asset.symbol);
        let cb_total = cost_basis.open_cost_basis(&asset.symbol);

        let (cb_str, pnl_eur_str, pnl_pct_str) = match avg_cost {
            Some(_) => {
                let pnl_eur = asset.value_eur - cb_total;
                let pnl_pct = if cb_total > 0.0 { pnl_eur / cb_total * 100.0 } else { 0.0 };
                total_pnl += pnl_eur;
                (format!("{cb_total:>11.2}"), format!("{pnl_eur:>+11.2}"), format!("{pnl_pct:>+7.2}%"))
            }
            None => (format!("{:>11}", "—"), format!("{:>11}", "—"), format!("{:>8}", "—")),
        };

        println!(
            "  {:<10} {:>10.4} {:>10.2} {:>12.2} {} {} {}",
            asset.symbol, asset.quantity, asset.price_eur, asset.value_eur, cb_str, pnl_eur_str, pnl_pct_str
        );
    }

    println!("\nP&L latent total : {total_pnl:>+.2} EUR");

  
       // --- EXPORT POUR LE DASHBOARD STREAMLIT ---
    println!("\n=== EXPORT DASHBOARD ===");
    use serde::Serialize;

    #[derive(Serialize)]
    struct DashboardAsset {
        symbol: String,
        quantity: f64,
        price_eur: f64,
        value_eur: f64,
        cost_basis_eur: f64,
        pnl_eur: f64,
        pnl_pct: f64,
        kind: AssetKind,
    }

    #[derive(Serialize)]
    struct DashboardData {
        date: String,
        total_value_eur: f64,
        total_cost_basis_eur: f64,
        total_pnl_eur: f64,
        assets: Vec<DashboardAsset>,
        correlation_matrix: HashMap<String, HashMap<String, f64>>,
    }

    let mut dashboard_assets = Vec::new();
    let mut total_cb = 0.0;
    let mut total_pnl_calc = 0.0;

    for asset in &snapshot.assets {
        if asset.value_eur <= 0.01 || matches!(asset.symbol.as_str(), "USDC" | "SOL" | "ALGO") {
            continue; // on cache les poussières
        }

        let cb_total = cost_basis.open_cost_basis(&asset.symbol);
        let pnl_eur = asset.value_eur - cb_total;
        let pnl_pct = if cb_total > 0.0 { pnl_eur / cb_total * 100.0 } else { 0.0 };

        total_cb += cb_total;
        total_pnl_calc += pnl_eur;

        dashboard_assets.push(DashboardAsset {
            symbol: asset.symbol.clone(),
            quantity: asset.quantity,
            price_eur: asset.price_eur,
            value_eur: asset.value_eur,
            cost_basis_eur: cb_total,
            pnl_eur,
            pnl_pct,
            kind: asset.kind,
        });
    }
    // --- NOUVEAU : CALCUL DE LA MATRICE ---
    println!("\n=== CALCUL DE LA MATRICE DE CORRÉLATION ===");
    let correlation_matrix = portfolio_rs::ledger::metrics::compute_correlation_matrix(&tx_store, 90);

    let dashboard_data = DashboardData {
        date: snapshot.date.clone(),
        total_value_eur: snapshot.total_value_eur,
        total_cost_basis_eur: total_cb,
        total_pnl_eur: total_pnl_calc,
        assets: dashboard_assets,
        correlation_matrix: correlation_matrix,
    };

    let dashboard_path = PathBuf::from("./data/dashboard.json");
    let dashboard_json = serde_json::to_string_pretty(&dashboard_data)?;
    std::fs::write(&dashboard_path, dashboard_json)?;
    println!("Données du dashboard sauvegardées dans : {dashboard_path:?}");

    Ok(())
}

