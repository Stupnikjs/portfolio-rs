//! Plus-values réalisées par compte fiscal (CTO, PEA, Binance).
//!
//! `compute_fifo` rejoue TOUTES les transactions et indexe les lots par
//! symbole : un titre détenu à la fois sur le CTO et sur le PEA partagerait
//! ses lots entre les deux comptes, et le P&L réalisé global mélange PEA,
//! CTO et crypto. Ici on isole d'abord les transactions d'un seul compte,
//! puis on refait le FIFO sur ce périmètre.
//!
//! Périmètre actuel : les TITRES (`AssetKind::Stock`). Les cessions crypto
//! (Binance) sont calculées à part : leurs WITHDRAW (transferts) sont vus
//! comme des ventes FIFO et fausseraient le résultat.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use chrono::Datelike;

use crate::ledger::cost_basis::compute_fifo;
use crate::schema::{AssetKind, Platform, Transaction};
use crate::store::serialize::TxStore;

/// Compte fiscal d'une transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaxAccount {
    /// Compte-titres ordinaire (XTB, export principal `account.xlsx`).
    Cto,
    /// PEA (XTB, export `account_pea.xlsx`) : le fichier porte « pea » dans son nom.
    Pea,
    Binance,
    /// Saisies manuelles (crypto) : rattachées à aucun compte-titres.
    Other,
}

/// Compte d'une transaction : plateforme, puis nom du fichier source pour XTB
/// (les deux exports ont le même `account_label`, seul le fichier les distingue).
pub fn tax_account(tx: &Transaction) -> TaxAccount {
    match tx.platform {
        Platform::Binance => TaxAccount::Binance,
        Platform::Manual => TaxAccount::Other,
        Platform::Xtb => {
            let file = Path::new(&tx.source_file)
                .file_name()
                .map(|f| f.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            // « pea » comme mot entier : account_pea.xlsx, pea.xlsx... mais pas « speaker.xlsx ».
            if file.split(|c: char| !c.is_alphanumeric()).any(|word| word == "pea") {
                TaxAccount::Pea
            } else {
                TaxAccount::Cto
            }
        }
    }
}

/// Plus-values réalisées d'un compte (en EUR).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct RealizedSummary {
    /// Somme des cessions en plus-value (≥ 0).
    pub gains_eur: f64,
    /// Somme des cessions en moins-value (≤ 0).
    pub losses_eur: f64,
    /// Plus-value nette = gains + moins-values.
    pub net_eur: f64,
    /// Nombre de cessions (ventes) prises en compte.
    pub disposals: usize,
    /// Cessions dépassant les lots connus : coût inconnu traité à 0, donc plus-value surestimée.
    pub incomplete: usize,
    /// Plus-value nette par année de cession.
    pub net_by_year: BTreeMap<i32, f64>,
}

impl RealizedSummary {
    /// Texte à afficher dans le terminal.
    pub fn report(&self, title: &str) -> String {
        let mut out = format!("=== {title} ===\n");
        out.push_str(&format!(
            "Plus-value nette réalisée : {:>+.2} EUR  ({} cession{})\n",
            self.net_eur,
            self.disposals,
            if self.disposals > 1 { "s" } else { "" }
        ));
        out.push_str(&format!("  Plus-values  : {:>+12.2} EUR\n", self.gains_eur));
        out.push_str(&format!("  Moins-values : {:>+12.2} EUR\n", self.losses_eur));
        if !self.net_by_year.is_empty() {
            out.push_str("  Par année de cession :\n");
            for (year, net) in &self.net_by_year {
                out.push_str(&format!("    {year} : {net:>+12.2} EUR\n"));
            }
        }
        if self.incomplete > 0 {
            out.push_str(&format!(
                "  ⚠ {} cession(s) dépassent les lots connus (coût traité à 0) : plus-value surestimée\n",
                self.incomplete
            ));
        }
        out
    }
}

/// Plus-values réalisées sur les titres d'un compte, FIFO appliqué aux seules
/// transactions de ce compte.
pub fn realized_summary(tx_store: &TxStore, account: TaxAccount) -> Result<RealizedSummary> {
    let mut scoped = TxStore::new();
    scoped.add_transactions(
        tx_store.transactions.iter().filter(|tx| tax_account(tx) == account).cloned().collect(),
    );
    let fifo = compute_fifo(&scoped, None)?;

    let mut summary = RealizedSummary::default();
    for gain in &fifo.realized_gains {
        // Pas de cash (conversions, retraits) ni de crypto : titres uniquement.
        if scoped.assets.get(&gain.symbol).map(|a| a.kind) != Some(AssetKind::Stock) {
            continue;
        }
        if gain.pnl_eur >= 0.0 {
            summary.gains_eur += gain.pnl_eur;
        } else {
            summary.losses_eur += gain.pnl_eur;
        }
        summary.disposals += 1;
        if gain.incomplete {
            summary.incomplete += 1;
        }
        *summary.net_by_year.entry(gain.sell_time.year()).or_insert(0.0) += gain.pnl_eur;
    }
    summary.net_eur = summary.gains_eur + summary.losses_eur;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::TransactionKind::{Buy, Deposit, Sell, Withdraw};
    use crate::testutil::{store, tx};

    const CTO_FILE: &str = "./data/raw/accounts/account.xlsx";
    const PEA_FILE: &str = "./data/raw/accounts/account_pea.xlsx";

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    /// Transaction de titre XTB sur le fichier donné.
    fn stock(file: &str, kind: crate::schema::TransactionKind, symbol: &str, qty: f64, value: f64, time: &str, id: &str) -> Transaction {
        let mut t = tx(kind, symbol, AssetKind::Stock, qty, value, time, id);
        t.platform = Platform::Xtb;
        t.account_label = "XTB".to_string();
        t.source_file = file.to_string();
        t
    }

    fn on(platform: Platform, file: &str) -> Transaction {
        let mut t = tx(Buy, "X", AssetKind::Stock, 1.0, 1.0, "2026-01-01T10:00:00Z", "x");
        t.platform = platform;
        t.source_file = file.to_string();
        t
    }

    // --- tax_account ---

    #[test]
    fn xtb_main_export_is_the_cto_and_the_pea_export_is_the_pea() {
        assert_eq!(tax_account(&on(Platform::Xtb, CTO_FILE)), TaxAccount::Cto);
        assert_eq!(tax_account(&on(Platform::Xtb, PEA_FILE)), TaxAccount::Pea);
    }

    #[test]
    fn pea_detection_is_case_insensitive_and_whole_word() {
        assert_eq!(tax_account(&on(Platform::Xtb, "C:\\data\\Account_PEA.xlsx")), TaxAccount::Pea);
        assert_eq!(tax_account(&on(Platform::Xtb, "./data/pea.xlsx")), TaxAccount::Pea);
        assert_eq!(tax_account(&on(Platform::Xtb, "./data/speaker.xlsx")), TaxAccount::Cto);
        // Seul le nom du fichier compte, pas le dossier.
        assert_eq!(tax_account(&on(Platform::Xtb, "./pea/account.xlsx")), TaxAccount::Cto);
    }

    #[test]
    fn binance_and_manual_are_never_the_cto() {
        assert_eq!(tax_account(&on(Platform::Binance, CTO_FILE)), TaxAccount::Binance);
        assert_eq!(tax_account(&on(Platform::Manual, CTO_FILE)), TaxAccount::Other);
    }

    // --- realized_summary ---

    #[test]
    fn net_is_gains_minus_losses_over_the_cto_sales() {
        let s = store(vec![
            stock(CTO_FILE, Buy, "AIR", 10.0, 1_000.0, "2025-01-10T10:00:00Z", "a1"),
            stock(CTO_FILE, Sell, "AIR", 10.0, 1_200.0, "2025-03-10T10:00:00Z", "a2"), // +200
            stock(CTO_FILE, Buy, "SAN", 5.0, 500.0, "2025-02-10T10:00:00Z", "s1"),
            stock(CTO_FILE, Sell, "SAN", 5.0, 400.0, "2025-04-10T10:00:00Z", "s2"), // -100
        ]);
        let r = realized_summary(&s, TaxAccount::Cto).unwrap();

        assert!(approx(r.gains_eur, 200.0));
        assert!(approx(r.losses_eur, -100.0));
        assert!(approx(r.net_eur, 100.0));
        assert_eq!(r.disposals, 2);
        assert_eq!(r.incomplete, 0);
    }

    #[test]
    fn pea_and_binance_sales_are_excluded_from_the_cto() {
        let mut btc_buy = tx(Buy, "BTC", AssetKind::Crypto, 1.0, 20_000.0, "2025-01-01T10:00:00Z", "b1");
        let mut btc_sell = tx(Sell, "BTC", AssetKind::Crypto, 1.0, 50_000.0, "2025-06-01T10:00:00Z", "b2");
        btc_buy.platform = Platform::Binance;
        btc_sell.platform = Platform::Binance;

        let s = store(vec![
            stock(CTO_FILE, Buy, "AIR", 10.0, 1_000.0, "2025-01-10T10:00:00Z", "a1"),
            stock(CTO_FILE, Sell, "AIR", 10.0, 1_200.0, "2025-03-10T10:00:00Z", "a2"), // +200
            stock(PEA_FILE, Buy, "SAN", 10.0, 500.0, "2025-01-10T10:00:00Z", "p1"),
            stock(PEA_FILE, Sell, "SAN", 10.0, 5_000.0, "2025-03-10T10:00:00Z", "p2"), // +4500 (PEA)
            btc_buy,
            btc_sell, // +30000 (Binance)
        ]);

        assert!(approx(realized_summary(&s, TaxAccount::Cto).unwrap().net_eur, 200.0));
        assert!(approx(realized_summary(&s, TaxAccount::Pea).unwrap().net_eur, 4_500.0));
    }

    #[test]
    fn a_ticker_held_on_both_accounts_does_not_share_its_lots() {
        // Même titre sur PEA (acheté 100, plus ancien) et CTO (acheté 1000).
        // FIFO global : la vente CTO consommerait le lot PEA -> +1000 faux.
        let s = store(vec![
            stock(PEA_FILE, Buy, "AIR", 10.0, 100.0, "2024-01-01T10:00:00Z", "p1"),
            stock(CTO_FILE, Buy, "AIR", 10.0, 1_000.0, "2025-01-01T10:00:00Z", "c1"),
            stock(CTO_FILE, Sell, "AIR", 10.0, 1_100.0, "2025-06-01T10:00:00Z", "c2"),
        ]);
        let r = realized_summary(&s, TaxAccount::Cto).unwrap();

        assert!(approx(r.net_eur, 100.0), "net CTO = {}", r.net_eur);
    }

    #[test]
    fn fifo_consumes_the_oldest_cto_lots_first() {
        let s = store(vec![
            stock(CTO_FILE, Buy, "AIR", 10.0, 1_000.0, "2025-01-01T10:00:00Z", "a1"), // 100 / part
            stock(CTO_FILE, Buy, "AIR", 10.0, 2_000.0, "2025-02-01T10:00:00Z", "a2"), // 200 / part
            stock(CTO_FILE, Sell, "AIR", 15.0, 3_000.0, "2025-03-01T10:00:00Z", "a3"), // coût 1000 + 1000
        ]);
        let r = realized_summary(&s, TaxAccount::Cto).unwrap();

        assert!(approx(r.net_eur, 1_000.0));
        assert_eq!(r.disposals, 1);
    }

    #[test]
    fn net_is_split_by_year_of_sale() {
        let s = store(vec![
            stock(CTO_FILE, Buy, "AIR", 20.0, 2_000.0, "2024-01-01T10:00:00Z", "a1"),
            stock(CTO_FILE, Sell, "AIR", 10.0, 1_500.0, "2025-12-31T23:00:00Z", "a2"), // 2025 : +500
            stock(CTO_FILE, Sell, "AIR", 10.0, 800.0, "2026-01-02T10:00:00Z", "a3"),   // 2026 : -200
        ]);
        let r = realized_summary(&s, TaxAccount::Cto).unwrap();

        assert_eq!(r.net_by_year.len(), 2);
        assert!(approx(r.net_by_year[&2025], 500.0));
        assert!(approx(r.net_by_year[&2026], -200.0));
        assert!(approx(r.net_eur, 300.0));
    }

    #[test]
    fn a_sale_beyond_known_lots_is_flagged_incomplete() {
        let s = store(vec![
            stock(CTO_FILE, Buy, "AIR", 5.0, 500.0, "2025-01-01T10:00:00Z", "a1"),
            stock(CTO_FILE, Sell, "AIR", 10.0, 1_200.0, "2025-02-01T10:00:00Z", "a2"),
        ]);
        let r = realized_summary(&s, TaxAccount::Cto).unwrap();

        assert_eq!(r.incomplete, 1);
        assert!(approx(r.net_eur, 700.0)); // reliquat de 5 titres compté à coût 0
        assert!(r.report("T").contains("⚠ 1 cession(s)"));
    }

    #[test]
    fn cash_movements_never_count_as_realized_gains() {
        let mut dep = tx(Deposit, "EUR", AssetKind::Cash, 1_000.0, 1_000.0, "2025-01-01T09:00:00Z", "d1");
        let mut wd = tx(Withdraw, "EUR", AssetKind::Cash, 400.0, 400.0, "2025-02-01T09:00:00Z", "d2");
        dep.platform = Platform::Xtb;
        dep.source_file = CTO_FILE.to_string();
        wd.platform = Platform::Xtb;
        wd.source_file = CTO_FILE.to_string();
        let r = realized_summary(&store(vec![dep, wd]), TaxAccount::Cto).unwrap();

        assert_eq!(r, RealizedSummary::default());
    }

    #[test]
    fn no_sale_gives_an_empty_summary() {
        let s = store(vec![stock(CTO_FILE, Buy, "AIR", 10.0, 1_000.0, "2025-01-01T10:00:00Z", "a1")]);
        let r = realized_summary(&s, TaxAccount::Cto).unwrap();

        assert_eq!(r, RealizedSummary::default());
        assert!(r.report("T").contains("0 cession"));
        assert!(!r.report("T").contains("⚠"));
    }

    #[test]
    fn report_shows_signed_amounts_years_in_order_and_plural() {
        let s = store(vec![
            stock(CTO_FILE, Buy, "AIR", 30.0, 3_000.0, "2024-01-01T10:00:00Z", "a1"),
            stock(CTO_FILE, Sell, "AIR", 10.0, 1_500.0, "2026-03-01T10:00:00Z", "a2"),
            stock(CTO_FILE, Sell, "AIR", 10.0, 800.0, "2025-03-01T10:00:00Z", "a3"),
        ]);
        let text = realized_summary(&s, TaxAccount::Cto).unwrap().report("CTO TEST");

        assert!(text.starts_with("=== CTO TEST ===\n"));
        assert!(text.contains("Plus-value nette réalisée : +300.00 EUR  (2 cessions)"));
        assert!(text.contains("Plus-values  :"));
        assert!(text.contains("-200.00 EUR")); // moins-value 2025
        assert!(text.find("2025 :").unwrap() < text.find("2026 :").unwrap());
    }
}
