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

pub fn portfolio_snapshot_at(tx_store: &TxStore, at: Option<DateTime<Utc>>) -> PortfolioSnapshot {
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

        let price_eur = historical_price_eur(&symbol, at, kind, ticker.as_deref());
        let value_eur = quantity * price_eur;
        total_value_eur += value_eur;

        details.push(AssetSnapshot { symbol, quantity, price_eur, value_eur, kind, ticker });
    }

    details.sort_by(|a, b| b.value_eur.partial_cmp(&a.value_eur).unwrap_or(std::cmp::Ordering::Equal));

    PortfolioSnapshot { date: at.format("%Y-%m-%d").to_string(), total_value_eur, assets: details }
}
