//! Portage de src/ledger/portfolio.py -- valorisation du portefeuille à
//! une date donnée. Combine les positions (quantités) avec les prix de
//! marché historiques.

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::ledger::positions::holdings_at;
use crate::market::prices::historical_price_eur;
use crate::schema::AssetKind;
use crate::store::serialize::TxStore;

#[derive(Debug, Clone, Serialize)]
pub struct AssetSnapshot {
    pub symbol: String,
    pub quantity: f64,
    pub price_eur: f64,
    pub value_eur: f64,
    pub kind: AssetKind,
    pub ticker: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PortfolioSnapshot {
    pub date: String,
    pub total_value_eur: f64,
    pub assets: Vec<AssetSnapshot>,
}

/// Fonction de prix injectable : (symbole, nature, ticker, date) -> prix EUR
/// (0.0 si introuvable). Permet de tester la valorisation sans réseau.
pub type PriceFn<'a> = &'a dyn Fn(&str, AssetKind, Option<&str>, DateTime<Utc>) -> f64;

pub fn portfolio_snapshot_at(tx_store: &TxStore, at: Option<DateTime<Utc>>) -> PortfolioSnapshot {
    let live_prices = |symbol: &str, kind: AssetKind, ticker: Option<&str>, at: DateTime<Utc>| {
        historical_price_eur(symbol, at, kind, ticker)
    };
    portfolio_snapshot_with(tx_store, at, &live_prices)
}

pub fn portfolio_snapshot_with(
    tx_store: &TxStore,
    at: Option<DateTime<Utc>>,
    price_fn: PriceFn,
) -> PortfolioSnapshot {
    let at = at.unwrap_or_else(Utc::now);

    let holdings = holdings_at(tx_store, Some(at));
    let mut total_value_eur = 0.0;
    let mut details: Vec<AssetSnapshot> = Vec::new();

    for (symbol, quantity) in holdings {
        if quantity.abs() < 1e-12 {
            continue;
        }

        let asset = tx_store.assets.get(&symbol);
        let ticker = asset.and_then(|a| a.identifiers.ticker.clone());
        let kind = asset.map(|a| a.kind).unwrap_or(AssetKind::Crypto); // fallback défensif

        let price_eur = price_fn(&symbol, kind, ticker.as_deref(), at);
        let value_eur = quantity * price_eur;
        total_value_eur += value_eur;

        details.push(AssetSnapshot { symbol, quantity, price_eur, value_eur, kind, ticker });
    }

    details.sort_by(|a, b| b.value_eur.partial_cmp(&a.value_eur).unwrap_or(std::cmp::Ordering::Equal));

    PortfolioSnapshot { date: at.format("%Y-%m-%d").to_string(), total_value_eur, assets: details }
}



#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::TransactionKind::{Buy, Sell};
    use crate::testutil::{store, tx, utc};
    use std::cell::RefCell;

    fn prices(symbol: &str, _k: AssetKind, _t: Option<&str>, _at: DateTime<Utc>) -> f64 {
        match symbol {
            "BTC" => 50_000.0,
            "AIR" => 100.0,
            _ => 0.0,
        }
    }

    #[test]
    fn snapshot_uses_injected_prices_and_sorts_by_value_desc() {
        let s = store(vec![
            tx(Buy, "AIR", AssetKind::Stock, 10.0, 900.0, "2026-01-05T10:00:00Z", "a"),
            tx(Buy, "BTC", AssetKind::Crypto, 1.0, 40_000.0, "2026-01-06T10:00:00Z", "b"),
        ]);
        let snap = portfolio_snapshot_with(&s, Some(utc("2026-02-01T00:00:00Z")), &prices);

        let symbols: Vec<&str> = snap.assets.iter().map(|a| a.symbol.as_str()).collect();
        assert_eq!(symbols, vec!["BTC", "AIR"]);
        assert_eq!(snap.assets[1].value_eur, 1_000.0);
        assert_eq!(snap.total_value_eur, 51_000.0);
        assert_eq!(snap.date, "2026-02-01");
    }

    #[test]
    fn snapshot_skips_fully_sold_positions() {
        let s = store(vec![
            tx(Buy, "BTC", AssetKind::Crypto, 1.0, 40_000.0, "2026-01-05T10:00:00Z", "a"),
            tx(Sell, "BTC", AssetKind::Crypto, 1.0, 45_000.0, "2026-01-10T10:00:00Z", "b"),
        ]);
        let snap = portfolio_snapshot_with(&s, Some(utc("2026-02-01T00:00:00Z")), &prices);
        assert!(snap.assets.is_empty());
        assert_eq!(snap.total_value_eur, 0.0);
    }

    #[test]
    fn snapshot_only_contains_holdings_up_to_the_requested_date_and_passes_it_to_prices() {
        let s = store(vec![
            tx(Buy, "BTC", AssetKind::Crypto, 1.0, 40_000.0, "2026-01-05T10:00:00Z", "a"),
            tx(Buy, "AIR", AssetKind::Stock, 10.0, 900.0, "2026-03-05T10:00:00Z", "b"),
        ]);
        let seen: RefCell<Vec<DateTime<Utc>>> = RefCell::new(Vec::new());
        let at = utc("2026-02-01T23:59:59Z");
        let recorder = |sym: &str, k: AssetKind, t: Option<&str>, when: DateTime<Utc>| {
            seen.borrow_mut().push(when);
            prices(sym, k, t, when)
        };
        let snap = portfolio_snapshot_with(&s, Some(at), &recorder);

        assert_eq!(snap.assets.len(), 1); // AIR pas encore acheté
        assert_eq!(*seen.borrow(), vec![at]);
    }
}