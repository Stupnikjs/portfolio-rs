//! Portage de src/cli.py -- construit (ou met à jour) serialized_tx.json
//! à partir des exports bruts dans ./data/raw/.
//!
//! Ré-exécutable sans risque : le tx_store est recréé from scratch à chaque run.
//! Le cache des prix (price_cache.bin) persiste et bloque les appels API inutiles.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;

use portfolio_rs::ledger::cost_basis::compute_fifo;
use portfolio_rs::ledger::portfolio::portfolio_snapshot_at;
use portfolio_rs::ledger::positions::non_zero_holdings_at;
use portfolio_rs::market::correlation::compute_correlation_matrices;
use portfolio_rs::history::record_weekly_history;
use portfolio_rs::market::tickers::resolve_ticker;
use portfolio_rs::parse::{binance, manual, xtb};
use portfolio_rs::schema::{AssetKind,  DashboardAsset, DashboardData, TransactionKind};
use portfolio_rs::store::serialize::{TxStore, save_wallet};
use portfolio_rs::market::prices::{init_price_caches, save_price_caches};
use portfolio_rs::trades::{build_trade_frequency, build_trades};

const CORRELATION_MIN_VALUE_EUR: f64 = 10.0;

fn data_dir() -> PathBuf {
    PathBuf::from("./data/raw")
}

fn accounts_path() -> PathBuf {
    data_dir().join("accounts")
}

// --- NOUVEAU : Génération automatique des fichiers pour le LLM ---

fn collect_files(dir: &Path, files: &mut Vec<PathBuf>, extension: &str) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, files, extension)?;
        } else if path.extension().map_or(false, |ext| ext == extension) {
            files.push(path);
        }
    }
    Ok(())
}

fn generate_concatenated_txt(root_dir: &str, extension: &str, output_file: &str) -> Result<()> {
    let mut files = Vec::new();
    let path = Path::new(root_dir);
    
    if !path.exists() {
        println!("[Omis] Dossier introuvable pour la génération : {}", root_dir);
        return Ok(());
    }

    collect_files(path, &mut files, extension)?;
    files.sort();
    
    let mut output = String::new();
    for file_path in &files {
        let absolute_rel_path = file_path.to_string_lossy().replace("\\", "/");
        
        let (dossier, chemin_relatif) = if absolute_rel_path.starts_with(&format!("{}/", root_dir)) {
            let rel = absolute_rel_path.trim_start_matches(&format!("{}/", root_dir));
            let d = rel.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
            let full_d = if d.is_empty() { root_dir.to_string() } else { format!("{}/{}", root_dir, d) };
            (full_d, rel.to_string())
        } else {
            (absolute_rel_path.clone(), absolute_rel_path.clone())
        };
        
        let fichier = file_path.file_name().unwrap().to_string_lossy();
        
        output.push_str("################################################################################
");
        output.push_str(&format!("# Dossier : {}\n", dossier));
        output.push_str(&format!("# Fichier : {}\n", fichier));
        output.push_str(&format!("# Chemin relatif : {}\n", chemin_relatif));
        output.push_str("################################################################################

");
        
        let content = fs::read_to_string(file_path)?;
        output.push_str(&content);
        output.push_str("

");
    }
    
    fs::write(output_file, output)?;
    println!("{} généré avec succès.", output_file);
    Ok(())
}

// ---------------------------------------------------

fn main() -> Result<()> {
    println!("=== CONSTRUCTION DU WALLET ===");
    let cache_path = PathBuf::from("./data/cache");
    init_price_caches(&cache_path);

    // Le wallet repart de zéro à chaque exécution
    let mut tx_store = TxStore::new();

    let new_transactions = binance::parse_binance_sources(accounts_path());
    let mut xtb_tx = Vec::new();
    xtb_tx.extend(xtb::parse_xtb_file(&accounts_path().join("account.xlsx")));
    xtb_tx.extend(xtb::parse_xtb_file(&accounts_path().join("account_pea.xlsx")));

    // L'ajout des transactions va automatiquement "nourrir" le cache des prix
    // au cas où le cache binaire aurait une trouée.
    tx_store.add_transactions(new_transactions);
    tx_store.add_transactions(xtb_tx);

    let manual_tx = manual::parse_manual(&data_dir().join("manual_tx.json"))?;
    tx_store.add_transactions(manual_tx);

    println!("\n=== DIAGNOSTIC : BUY/DEPOSIT à quantité négative ===");
    for tx in &tx_store.transactions {
        if matches!(tx.kind, TransactionKind::Buy | TransactionKind::Deposit) && tx.quantity <= 0.0 {
            println!(
                "  platform={:?} asset={} qty={} value_eur={} time={} source={} external_id={:?} remark={:?}",
                tx.platform, tx.asset.symbol, tx.quantity, tx.value_eur, tx.time, tx.source_file, tx.external_id, tx.remark
            );
        }
    }

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

    // On écrase systématiquement tx_store.json (fichier jetable)
    let tx_store_path = PathBuf::from("./data/tx_store.json");
    save_wallet(&tx_store, &tx_store_path)?;
    println!("tx_store.json regénéré.");

    record_weekly_history(&tx_store, &PathBuf::from("./data/history.json"))?;
    
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
        if asset.value_eur <= 0.01 || matches!(asset.symbol.as_str(), "USDC" | "SOL" | "ALGO") || asset.kind == AssetKind::Cash {
            continue; 
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

        
    // On ignore la crypto : les WITHDRAW (transferts entre wallets/exchanges)
    // sont comptés comme des "ventes" FIFO, ce qui gonfle artificiellement le
    // P&L réalisé alors qu'aucune vente réelle n'a eu lieu côté crypto.
    let stock_symbols: std::collections::HashSet<&str> = tx_store
        .assets
        .values()
        .filter(|a| a.kind == AssetKind::Stock)
        .map(|a| a.symbol.as_str())
        .collect();

    let total_realized_pnl_stocks: f64 = cost_basis
        .realized_gains
        .iter()
        .filter(|g| stock_symbols.contains(g.symbol.as_str()))
        .map(|g| g.pnl_eur)
        .sum();

    let overall_pnl = total_pnl + total_realized_pnl_stocks;

    println!("P&L réalisé total (actions uniquement) : {total_realized_pnl_stocks:>+.2} EUR");
    println!("\n=== PERFORMANCE GLOBALE (latent + réalisé actions) ===");
    println!("Overall P&L : {overall_pnl:>+.2} EUR  (latent {total_pnl:>+.2} + réalisé {total_realized_pnl_stocks:>+.2})");


    println!("\nDétail P&L réalisé par action :");
    println!("  {:<10} {:>10} {:>12} {:>12} {:>12}", "Symbole", "Qty", "Produit", "Coût", "P&L");

    let mut realized_by_stock: std::collections::HashMap<&str, Vec<&portfolio_rs::ledger::cost_basis::RealizedGain>> =
        std::collections::HashMap::new();
    for g in cost_basis.realized_gains.iter().filter(|g| stock_symbols.contains(g.symbol.as_str())) {
        realized_by_stock.entry(g.symbol.as_str()).or_default().push(g);
    }

    let mut symbols_sorted: Vec<&&str> = realized_by_stock.keys().collect();
    symbols_sorted.sort();

    for symbol in symbols_sorted {
        let gains = &realized_by_stock[symbol];
        let qty: f64 = gains.iter().map(|g| g.quantity).sum();
        let proceeds: f64 = gains.iter().map(|g| g.proceeds_eur).sum();
        let cost: f64 = gains.iter().map(|g| g.cost_eur).sum();
        let pnl: f64 = gains.iter().map(|g| g.pnl_eur).sum();
        let has_incomplete = gains.iter().any(|g| g.incomplete);

        println!(
            "  {:<10} {:>10.4} {:>12.2} {:>12.2} {:>+12.2}{}",
            symbol, qty, proceeds, cost, pnl,
            if has_incomplete { "  ⚠ incomplet" } else { "" }
        );
    }

    println!("\n=== CALCUL DE LA CORRÉLATION (90j / 6m / 1an, seuil {CORRELATION_MIN_VALUE_EUR}€) ===");
    let holdings = non_zero_holdings_at(&tx_store, None);
    let prices_eur: std::collections::HashMap<String, f64> =
        snapshot.assets.iter().map(|a| (a.symbol.clone(), a.price_eur)).collect();
    let correlation_matrices = compute_correlation_matrices(&tx_store, &holdings, &prices_eur, CORRELATION_MIN_VALUE_EUR);
    
    let dashboard_assets: Vec<DashboardAsset> = snapshot
        .assets
        .iter()
        .map(|a| {
            let cost_basis_eur = cost_basis.open_cost_basis(&a.symbol);
            let pnl_eur = a.value_eur - cost_basis_eur;
            let pnl_pct = if cost_basis_eur > 0.0 { pnl_eur / cost_basis_eur * 100.0 } else { 0.0 };
            let realized_pnl_eur = cost_basis.total_realized_pnl(Some(&a.symbol));
            DashboardAsset {
                symbol: a.symbol.clone(),
                kind: a.kind.as_str().to_string(),
                ticker: a.ticker.clone(),
                quantity: a.quantity,
                price_eur: a.price_eur,
                value_eur: a.value_eur,
                cost_basis_eur,
                pnl_eur,
                pnl_pct,
                realized_pnl_eur, // <-- On l'ajoute ici
            }
        })
        .collect();

    let total_cost_basis_eur: f64 = dashboard_assets.iter().map(|a| a.cost_basis_eur).sum();
    let dashboard_data = DashboardData {
        total_value_eur: snapshot.total_value_eur,
        total_cost_basis_eur,
        total_pnl_eur: snapshot.total_value_eur - total_cost_basis_eur,
        realized_pnl_eur: total_realized_pnl_stocks,
        trades: build_trades(&tx_store),
        trade_frequency: build_trade_frequency(&tx_store),
        assets: dashboard_assets,
        correlation_matrices,
    };
 
    let dashboard_path = PathBuf::from("./data/dashboard.json");

    std::fs::write(&dashboard_path, serde_json::to_string_pretty(&dashboard_data)?)?;
    println!("Dashboard écrit : {dashboard_path:?}");

    // Sauvegarde atomique et définitive du cache de prix
    save_price_caches(&cache_path);
    println!("Cache des prix 1h sauvegardé : {cache_path:?}");

    // --- Génération des fichiers pour le LLM ---
    generate_concatenated_txt("src", "rs", "forllm.txt")?;
    generate_concatenated_txt("dashboard", "py", "forllmdashboard.txt")?;

    Ok(())
}