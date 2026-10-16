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

use chrono::{Datelike, NaiveDate, SecondsFormat, Utc};

use crate::ledger::periods::{months_between, sundays_from, week_end};
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

/// Nombre de trades par mois et par semaine (périodes vides incluses jusqu'à aujourd'hui).
pub fn build_trade_frequency(tx_store: &TxStore) -> TradeFrequency {
    build_trade_frequency_at(tx_store, Utc::now().date_naive())
}

/// Comme `build_trade_frequency`, avec la date du jour fournie (testable).
pub fn build_trade_frequency_at(tx_store: &TxStore, today: NaiveDate) -> TradeFrequency {
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

    // Mois vides entre le premier trade et le mois courant.
    for (y, m) in months_between(first, today) {
        by_month
            .entry((y, m))
            .or_insert_with(|| PeriodCount::new(format!("{y:04}-{m:02}")));
    }

    // Semaines vides entre la première semaine et la semaine courante.
    let last_week = week_end(today);
    for cursor in sundays_from(first).take_while(|d| *d <= last_week) {
        by_week
            .entry(cursor)
            .or_insert_with(|| PeriodCount::new(cursor.format("%Y-%m-%d").to_string()));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{store, tx};
    use TransactionKind::{Buy, Deposit, Fee, Sell};

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn fixture() -> TxStore {
        store(vec![
            tx(Buy, "AIR", AssetKind::Stock, 10.0, 1_000.0, "2026-01-07T10:00:00Z", "t1"), // mercredi
            tx(Sell, "AIR", AssetKind::Stock, 4.0, 600.0, "2026-03-20T10:00:00Z", "t2"),   // vendredi
            tx(Buy, "BTC", AssetKind::Crypto, 0.5, 20_000.0, "2026-01-09T09:00:00Z", "t3"),
            // Pas des trades : cash, dépôt, frais.
            tx(Buy, "EUR", AssetKind::Cash, 500.0, 500.0, "2026-02-01T09:00:00Z", "c1"),
            tx(Deposit, "AIR", AssetKind::Stock, 1.0, 100.0, "2026-02-02T09:00:00Z", "c2"),
            tx(Fee, "AIR", AssetKind::Stock, 1.0, 2.0, "2026-02-03T09:00:00Z", "c3"),
        ])
    }

    #[test]
    fn trades_are_chronological_buys_and_sells_of_non_cash_assets_only() {
        let trades = build_trades(&fixture());

        let seen: Vec<(&str, &str)> = trades.iter().map(|t| (t.symbol.as_str(), t.kind.as_str())).collect();
        assert_eq!(seen, vec![("AIR", "Buy"), ("BTC", "Buy"), ("AIR", "Sell")]);
        assert_eq!(trades[2].unit_price_eur, 150.0); // 600 / 4
    }

    #[test]
    fn frequency_of_an_empty_store_is_default() {
        let f = build_trade_frequency_at(&store(vec![]), d(2026, 4, 1));
        assert_eq!(f.total_trades, 0);
        assert!(f.by_month.is_empty() && f.by_week.is_empty());
    }

    #[test]
    fn frequency_fills_empty_months_and_counts_buys_and_sells() {
        let f = build_trade_frequency_at(&fixture(), d(2026, 4, 15));

        assert_eq!(f.total_trades, 3);
        let months: Vec<&str> = f.by_month.iter().map(|p| p.period.as_str()).collect();
        assert_eq!(months, vec!["2026-01", "2026-02", "2026-03", "2026-04"]);
        assert_eq!((f.by_month[0].buys, f.by_month[0].sells, f.by_month[0].trades), (2, 0, 2));
        assert_eq!(f.by_month[1].trades, 0); // février : creux visible
        assert_eq!((f.by_month[2].buys, f.by_month[2].sells), (0, 1));
        assert_eq!(f.by_month[0].volume_eur, 21_000.0);
        assert!((f.avg_per_month - 0.75).abs() < 1e-9); // 3 trades / 4 mois
    }

    #[test]
    fn frequency_weeks_are_consecutive_sundays_up_to_the_current_week() {
        let today = d(2026, 4, 15); // mercredi -> semaine du dimanche 19/04
        let f = build_trade_frequency_at(&fixture(), today);

        assert_eq!(f.by_week.first().unwrap().period, "2026-01-11"); // semaine du 07/01
        assert_eq!(f.by_week.last().unwrap().period, "2026-04-19");
        let dates: Vec<NaiveDate> = f
            .by_week
            .iter()
            .map(|p| NaiveDate::parse_from_str(&p.period, "%Y-%m-%d").unwrap())
            .collect();
        assert!(dates.windows(2).all(|w| (w[1] - w[0]).num_days() == 7));
        let in_week: u32 = f.by_week.iter().filter(|p| p.period == "2026-01-11").map(|p| p.trades).sum();
        assert_eq!(in_week, 2); // les deux achats du 07 et du 09/01
        assert_eq!(f.by_week.iter().map(|p| p.trades).sum::<u32>(), 3);
    }

    #[test]
    fn frequency_months_and_weeks_use_the_same_trades_total() {
        let f = build_trade_frequency_at(&fixture(), d(2026, 12, 31));

        assert_eq!(f.by_month.iter().map(|p| p.trades).sum::<u32>(), f.total_trades);
        assert_eq!(f.by_week.iter().map(|p| p.trades).sum::<u32>(), f.total_trades);
        assert_eq!(f.by_month.len(), 12);
    }
}
