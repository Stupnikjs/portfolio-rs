//! Trades passés (Buy/Sell) et leur fréquence par mois / semaine, pour
//! dashboard.json.
//!
//! Un "trade" = une transaction Buy ou Sell sur un actif non-Cash.
//! Sont donc exclus : Deposit/Withdraw (transferts, jambes cash XTB,
//! dividendes...) et Fee.
//!
//! Attention : un Convert Binance produit deux transactions (Sell + Buy)
//! et compte donc pour 2 trades -- `buys` / `sells` dans chaque période
//! permettent de le voir.
//!
//! Les semaines sont identifiées par leur dimanche de fin (même clé que
//! history.json, pratique pour joindre les deux), les mois par "YYYY-MM".
//! Les périodes sans trade sont incluses (compteurs à 0) entre le premier
//! trade et aujourd'hui, pour que les creux soient visibles dans un graphe.

use std::collections::BTreeMap;

use chrono::{Datelike, Duration, NaiveDate, SecondsFormat, Utc};

use crate::history::week_end;
use crate::schema::{
    AssetKind, DashboardTrade, PeriodCount, TradeFrequency, Transaction, TransactionKind,
};
use crate::store::serialize::TxStore;

fn trade_txs(tx_store: &TxStore) -> impl Iterator<Item = &Transaction> {
    tx_store.transactions.iter().filter(|tx| {
        matches!(tx.kind, TransactionKind::Buy | TransactionKind::Sell)
            && tx.asset.kind != AssetKind::Cash
    })
}

/// Liste chronologique (plus ancien -> plus récent) des trades.
pub fn build_trades(tx_store: &TxStore) -> Vec<DashboardTrade> {
    let mut trades: Vec<DashboardTrade> = trade_txs(tx_store)
        .map(|tx| DashboardTrade {
            time: tx.time.to_rfc3339_opts(SecondsFormat::Secs, true),
            symbol: tx.asset.symbol.clone(),
            kind: tx.kind.as_str().to_string(),
            platform: tx.platform.as_str().to_string(),
            quantity: tx.quantity,
            // Dérivé de value_eur (autoritaire) : `price` peut être en
            // devise locale (cf. cost_basis.rs).
            unit_price_eur: if tx.quantity > 0.0 { tx.value_eur / tx.quantity } else { 0.0 },
            value_eur: tx.value_eur,
        })
        .collect();

    // RFC3339 UTC ("...Z", secondes) : l'ordre lexicographique = chronologique.
    trades.sort_by(|a, b| a.time.cmp(&b.time));
    trades
}

fn add_to_period(pc: &mut PeriodCount, is_buy: bool, value_eur: f64) {
    pc.trades += 1;
    if is_buy {
        pc.buys += 1;
    } else {
        pc.sells += 1;
    }
    pc.volume_eur += value_eur;
}

/// Nombre de trades par mois et par semaine (périodes vides incluses).
pub fn build_trade_frequency(tx_store: &TxStore) -> TradeFrequency {
    let mut by_month: BTreeMap<(i32, u32), PeriodCount> = BTreeMap::new();
    let mut by_week: BTreeMap<NaiveDate, PeriodCount> = BTreeMap::new();
    let mut total_trades = 0u32;
    let mut first: Option<NaiveDate> = None;

    for tx in trade_txs(tx_store) {
        let date = tx.time.date_naive();
        first = Some(first.map_or(date, |f| f.min(date)));
        let is_buy = tx.kind == TransactionKind::Buy;

        let month = by_month
            .entry((date.year(), date.month()))
            .or_insert_with(|| PeriodCount::new(format!("{:04}-{:02}", date.year(), date.month())));
        add_to_period(month, is_buy, tx.value_eur);

        let we = week_end(date);
        let week = by_week
            .entry(we)
            .or_insert_with(|| PeriodCount::new(we.format("%Y-%m-%d").to_string()));
        add_to_period(week, is_buy, tx.value_eur);

        total_trades += 1;
    }

    let Some(first) = first else {
        return TradeFrequency::default(); // aucun trade
    };
    let today = Utc::now().date_naive();

    // Mois vides entre le premier trade et le mois courant.
    let (mut y, mut m) = (first.year(), first.month());
    while (y, m) <= (today.year(), today.month()) {
        by_month
            .entry((y, m))
            .or_insert_with(|| PeriodCount::new(format!("{y:04}-{m:02}")));
        if m == 12 {
            y += 1;
            m = 1;
        } else {
            m += 1;
        }
    }

    // Semaines vides entre la première semaine et la semaine courante.
    let mut cursor = week_end(first);
    let last_week = week_end(today);
    while cursor <= last_week {
        by_week
            .entry(cursor)
            .or_insert_with(|| PeriodCount::new(cursor.format("%Y-%m-%d").to_string()));
        cursor += Duration::weeks(1);
    }

    let by_month: Vec<PeriodCount> = by_month.into_values().collect();
    let by_week: Vec<PeriodCount> = by_week.into_values().collect();

    TradeFrequency {
        total_trades,
        avg_per_month: total_trades as f64 / by_month.len() as f64,
        avg_per_week: total_trades as f64 / by_week.len() as f64,
        by_month,
        by_week,
    }
}
