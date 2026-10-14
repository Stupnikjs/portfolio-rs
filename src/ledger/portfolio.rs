//! Portage de src/ledger/portfolio.py -- valorisation du portefeuille à
//! une date donnée. Combine les positions (quantités) avec les prix de
//! marché historiques.

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::ledger::positions::holdings_at;
use crate::market::client::MarketDataClient;
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

pub fn portfolio_snapshot_at(tx_store: &TxStore, at: Option<DateTime<Utc>>, market_client: &MarketDataClient) -> PortfolioSnapshot {
    let live_prices = |symbol: &str, kind: AssetKind, ticker: Option<&str>, at: DateTime<Utc>| {
        market_client.historical_price_eur(symbol, at, kind, ticker)
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
        let kind = asset.map(|a| a.kind).unwrap_or(AssetKind::Crypto);

        let price_eur = price_fn(&symbol, kind, ticker.as_deref(), at);
        let value_eur = quantity * price_eur;
        total_value_eur += value_eur;

        details.push(AssetSnapshot { symbol, quantity, price_eur, value_eur, kind, ticker });
    }

    details.sort_by(|a, b| b.value_eur.partial_cmp(&a.value_eur).unwrap_or(std::cmp::Ordering::Equal));

    PortfolioSnapshot { date: at.format("%Y-%m-%d").to_string(), total_value_eur, assets: details }
}