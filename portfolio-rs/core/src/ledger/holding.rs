//! Durée de détention par actif, dérivée des lots FIFO déjà calculés
//! (`CostBasisResult`) : aucune donnée en plus, aucun appel réseau.
//!
//! - positions OUVERTES : âge des lots encore détenus à `now` ;
//! - positions FERMÉES (titres uniquement) : durée entre l'achat de chaque lot
//!   consommé et la vente. Pour la crypto, un WITHDRAW est un transfert entre
//!   wallets (vu comme une vente FIFO, voir cost_basis.rs) : le « fermé » serait
//!   trompeur, on ne le calcule pas.
//!
//! Les moyennes sont pondérées par le capital (quantité x coût unitaire) : un lot
//! de 3 000 € pèse trois fois plus qu'un lot de 1 000 €. Si le capital est nul
//! (lots à coût 0), on pondère par la quantité.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};

use crate::ledger::cost_basis::{CostBasisResult, Lot, RealizedGain};
use crate::schema::{AssetKind, ClosedSale, HoldingStats, OpenLotStat};

const SECONDS_PER_DAY: f64 = 86_400.0;
const EPSILON: f64 = 1e-12;

/// Un lot observé : sa durée de détention (jours), son capital et sa quantité.
#[derive(Debug, Clone, Copy)]
struct Sample {
    days: f64,
    cost_eur: f64,
    quantity: f64,
}

impl Sample {
    fn from_lot(lot: &Lot, until: DateTime<Utc>) -> Self {
        Self {
            days: days_between(lot.acquired_at, until),
            cost_eur: lot.quantity * lot.unit_cost_eur,
            quantity: lot.quantity,
        }
    }
}

fn days_between(from: DateTime<Utc>, to: DateTime<Utc>) -> f64 {
    ((to - from).num_seconds() as f64 / SECONDS_PER_DAY).max(0.0)
}

/// Moyenne des durées pondérée par le capital (à défaut, par la quantité).
fn weighted_avg_days(samples: &[Sample]) -> Option<f64> {
    let total_cost: f64 = samples.iter().map(|s| s.cost_eur).sum();
    let by_cost = total_cost > EPSILON;
    let weight = |s: &Sample| if by_cost { s.cost_eur } else { s.quantity };

    let total: f64 = samples.iter().map(weight).sum();
    if total <= EPSILON {
        return None;
    }
    Some(samples.iter().map(|s| s.days * weight(s)).sum::<f64>() / total)
}

fn min_days(samples: &[Sample]) -> Option<f64> {
    samples.iter().map(|s| s.days).reduce(f64::min)
}

fn max_days(samples: &[Sample]) -> Option<f64> {
    samples.iter().map(|s| s.days).reduce(f64::max)
}

#[derive(Default)]
struct Acc {
    kind: Option<AssetKind>,
    first_acquired: Option<DateTime<Utc>>,
    open: Vec<Sample>,
    open_lots: Vec<OpenLotStat>,
    open_quantity: f64,
    open_cost_eur: f64,
    closed: Vec<Sample>,
    closed_incomplete: bool,
}

impl Acc {
    fn note_acquired(&mut self, at: DateTime<Utc>) {
        self.first_acquired = Some(self.first_acquired.map_or(at, |first| first.min(at)));
    }
}

/// Les lots réellement consommés par une vente : si elle est incomplète, le dernier
/// « lot » est un reliquat fictif (coût 0, daté de la vente) qu'on écarte.
fn real_lots(gain: &RealizedGain) -> &[Lot] {
    if gain.incomplete {
        &gain.lots_consumed[..gain.lots_consumed.len().saturating_sub(1)]
    } else {
        &gain.lots_consumed[..]
    }
}

fn format_date(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%d").to_string()
}

/// Durées de détention de tous les actifs non-cash, triées par symbole.
///
/// - `kind_of` donne le type d'un symbole (ex. `|s| tx_store.assets.get(s).map(|a| a.kind)`) ;
/// - `price_of` son prix EUR actuel (ex. depuis `PortfolioSnapshot`), pour valoriser les lots ouverts.
pub fn holding_stats(
    cost_basis: &CostBasisResult,
    now: DateTime<Utc>,
    kind_of: &dyn Fn(&str) -> Option<AssetKind>,
    price_of: &dyn Fn(&str) -> Option<f64>,
) -> Vec<HoldingStats> {
    let mut by_symbol: BTreeMap<&str, Acc> = BTreeMap::new();

    for (symbol, lots) in &cost_basis.open_lots {
        let Some(kind) = kind_of(symbol).filter(|k| *k != AssetKind::Cash) else { continue };
        let acc = by_symbol.entry(symbol.as_str()).or_default();
        acc.kind = Some(kind);
        let price = price_of(symbol);
        for lot in lots {
            let sample = Sample::from_lot(lot, now);
            acc.note_acquired(lot.acquired_at);
            acc.open_quantity += lot.quantity;
            acc.open_cost_eur += sample.cost_eur;
            acc.open_lots.push(OpenLotStat {
                days: sample.days,
                cost_eur: sample.cost_eur,
                value_eur: price.map(|p| p * lot.quantity),
            });
            acc.open.push(sample);
        }
    }

    for gain in &cost_basis.realized_gains {
        if kind_of(&gain.symbol) != Some(AssetKind::Stock) {
            continue;
        }
        let acc = by_symbol.entry(gain.symbol.as_str()).or_default();
        acc.kind = Some(AssetKind::Stock);
        acc.closed_incomplete |= gain.incomplete;

        for lot in real_lots(gain) {
            acc.note_acquired(lot.acquired_at);
            acc.closed.push(Sample::from_lot(lot, gain.sell_time));
        }
    }

    by_symbol
        .into_iter()
        .map(|(symbol, acc)| HoldingStats {
            symbol: symbol.to_string(),
            kind: acc.kind.map_or("", |k| k.as_str()).to_string(),
            first_acquired: acc.first_acquired.map(format_date),
            open_quantity: acc.open_quantity,
            open_cost_eur: acc.open_cost_eur,
            open_avg_days: weighted_avg_days(&acc.open),
            open_oldest_days: max_days(&acc.open),
            open_lots: acc.open_lots,
            closed_lots: acc.closed.len() as u32,
            closed_avg_days: weighted_avg_days(&acc.closed),
            closed_min_days: min_days(&acc.closed),
            closed_max_days: max_days(&acc.closed),
            closed_incomplete: acc.closed_incomplete,
        })
        .collect()
}

/// Une ligne par vente de titres : durée de détention et résultat, pour comparer
/// la durée des gagnants à celle des perdants. Triée par date de vente.
pub fn closed_sales(
    cost_basis: &CostBasisResult,
    kind_of: &dyn Fn(&str) -> Option<AssetKind>,
) -> Vec<ClosedSale> {
    let mut sales: Vec<ClosedSale> = cost_basis
        .realized_gains
        .iter()
        .filter(|g| kind_of(&g.symbol) == Some(AssetKind::Stock))
        .map(|g| {
            let samples: Vec<Sample> =
                real_lots(g).iter().map(|lot| Sample::from_lot(lot, g.sell_time)).collect();
            ClosedSale {
                symbol: g.symbol.clone(),
                sell_date: format_date(g.sell_time),
                days: weighted_avg_days(&samples),
                cost_eur: g.cost_eur,
                proceeds_eur: g.proceeds_eur,
                pnl_eur: g.pnl_eur,
                pnl_pct: (g.cost_eur > EPSILON).then(|| g.pnl_eur / g.cost_eur * 100.0),
                incomplete: g.incomplete,
            }
        })
        .collect();
    sales.sort_by(|a, b| a.sell_date.cmp(&b.sell_date).then_with(|| a.symbol.cmp(&b.symbol)));
    sales
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::cost_basis::RealizedGain;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    fn lot(quantity: f64, unit_cost_eur: f64, acquired_at: &str) -> Lot {
        Lot { quantity, unit_cost_eur, acquired_at: utc(acquired_at), external_id: None }
    }

    fn gain(symbol: &str, sell_time: &str, lots: Vec<Lot>, incomplete: bool) -> RealizedGain {
        RealizedGain {
            symbol: symbol.into(),
            sell_time: utc(sell_time),
            quantity: lots.iter().map(|l| l.quantity).sum(),
            proceeds_eur: 0.0,
            cost_eur: 0.0,
            pnl_eur: 0.0,
            external_id: None,
            lots_consumed: lots,
            incomplete,
        }
    }

    fn kinds(symbol: &str) -> Option<AssetKind> {
        match symbol {
            "EUR" => Some(AssetKind::Cash),
            "BTC" => Some(AssetKind::Crypto),
            _ => Some(AssetKind::Stock),
        }
    }

    fn find<'a>(stats: &'a [HoldingStats], symbol: &str) -> &'a HoldingStats {
        stats.iter().find(|s| s.symbol == symbol).unwrap()
    }

    fn prices(symbol: &str) -> Option<f64> {
        match symbol {
            "AIR" => Some(150.0),
            "BTC" => Some(30_000.0),
            _ => None,
        }
    }

    const NOW: &str = "2025-06-01T00:00:00Z";

    #[test]
    fn open_average_is_weighted_by_capital_and_reports_the_oldest_lot() {
        // 1 000 € il y a 100 j, 3 000 € il y a 20 j : (100*1000 + 20*3000) / 4000 = 40 j.
        let mut cb = CostBasisResult::default();
        cb.open_lots.insert(
            "AIR".into(),
            vec![lot(10.0, 100.0, "2025-02-21T00:00:00Z"), lot(30.0, 100.0, "2025-05-12T00:00:00Z")],
        );
        let stats = holding_stats(&cb, utc(NOW), &kinds, &prices);
        let air = find(&stats, "AIR");

        assert!(approx(air.open_avg_days.unwrap(), 40.0));
        assert!(approx(air.open_oldest_days.unwrap(), 100.0));
        assert!(approx(air.open_quantity, 40.0));
        assert!(approx(air.open_cost_eur, 4_000.0));
        assert_eq!(air.first_acquired.as_deref(), Some("2025-02-21"));
        assert_eq!(air.closed_lots, 0);
        assert!(air.closed_avg_days.is_none());
    }

    #[test]
    fn closed_durations_run_from_each_consumed_lot_to_the_sale() {
        // Vente le 2025-04-01 : lot A (30 j, 1 000 €) + lot B (10 j, 1 000 €) -> moyenne 20 j.
        let mut cb = CostBasisResult::default();
        cb.realized_gains.push(gain(
            "AIR",
            "2025-04-01T00:00:00Z",
            vec![lot(10.0, 100.0, "2025-03-02T00:00:00Z"), lot(10.0, 100.0, "2025-03-22T00:00:00Z")],
            false,
        ));
        let stats = holding_stats(&cb, utc(NOW), &kinds, &prices);
        let air = find(&stats, "AIR");

        assert_eq!(air.closed_lots, 2);
        assert!(approx(air.closed_avg_days.unwrap(), 20.0));
        assert!(approx(air.closed_min_days.unwrap(), 10.0));
        assert!(approx(air.closed_max_days.unwrap(), 30.0));
        assert!(!air.closed_incomplete);
        assert!(air.open_avg_days.is_none());
    }

    #[test]
    fn an_incomplete_sale_ignores_the_fictional_zero_cost_remainder() {
        let mut cb = CostBasisResult::default();
        cb.realized_gains.push(gain(
            "AIR",
            "2025-04-01T00:00:00Z",
            vec![
                lot(5.0, 100.0, "2025-03-02T00:00:00Z"),  // vrai lot : 30 j
                lot(5.0, 0.0, "2025-04-01T00:00:00Z"),    // reliquat fictif : daté de la vente
            ],
            true,
        ));
        let stats = holding_stats(&cb, utc(NOW), &kinds, &prices);
        let air = find(&stats, "AIR");

        assert_eq!(air.closed_lots, 1);
        assert!(approx(air.closed_avg_days.unwrap(), 30.0));
        assert!(air.closed_incomplete);
    }

    #[test]
    fn cash_is_skipped_and_crypto_has_no_closed_stats() {
        let mut cb = CostBasisResult::default();
        cb.open_lots.insert("EUR".into(), vec![lot(500.0, 1.0, "2025-01-01T00:00:00Z")]);
        cb.open_lots.insert("BTC".into(), vec![lot(1.0, 20_000.0, "2025-01-01T00:00:00Z")]);
        // WITHDRAW BTC = transfert, vu comme une vente FIFO : ne doit pas produire de « fermé ».
        cb.realized_gains.push(gain("BTC", "2025-03-01T00:00:00Z", vec![lot(0.5, 10_000.0, "2025-01-01T00:00:00Z")], false));
        let stats = holding_stats(&cb, utc(NOW), &kinds, &prices);

        assert!(stats.iter().all(|s| s.symbol != "EUR"));
        let btc = find(&stats, "BTC");
        assert_eq!(btc.kind, "Crypto");
        assert!(btc.open_avg_days.is_some());
        assert_eq!(btc.closed_lots, 0);
        assert!(btc.closed_avg_days.is_none());
    }

    #[test]
    fn zero_cost_lots_fall_back_to_quantity_weights() {
        // Lots reçus gratuitement (coût 0) : 100 j x 1 et 20 j x 3 -> (100 + 60) / 4 = 40 j.
        let mut cb = CostBasisResult::default();
        cb.open_lots.insert(
            "AIR".into(),
            vec![lot(1.0, 0.0, "2025-02-21T00:00:00Z"), lot(3.0, 0.0, "2025-05-12T00:00:00Z")],
        );
        let stats = holding_stats(&cb, utc(NOW), &kinds, &prices);
        assert!(approx(find(&stats, "AIR").open_avg_days.unwrap(), 40.0));
    }

    #[test]
    fn an_asset_with_open_and_closed_lots_reports_both_and_the_earliest_purchase() {
        let mut cb = CostBasisResult::default();
        cb.realized_gains.push(gain("AIR", "2025-02-01T00:00:00Z", vec![lot(5.0, 100.0, "2025-01-01T00:00:00Z")], false));
        cb.open_lots.insert("AIR".into(), vec![lot(5.0, 120.0, "2025-03-01T00:00:00Z")]);
        let stats = holding_stats(&cb, utc(NOW), &kinds, &prices);
        let air = find(&stats, "AIR");

        assert_eq!(air.first_acquired.as_deref(), Some("2025-01-01"));
        assert!(approx(air.closed_avg_days.unwrap(), 31.0));
        assert!(approx(air.open_avg_days.unwrap(), 92.0)); // 2025-03-01 -> 2025-06-01
    }

    #[test]
    fn output_is_sorted_by_symbol_and_future_lots_never_go_negative() {
        let mut cb = CostBasisResult::default();
        cb.open_lots.insert("ZED".into(), vec![lot(1.0, 10.0, "2025-07-01T00:00:00Z")]); // après `now`
        cb.open_lots.insert("AIR".into(), vec![lot(1.0, 10.0, "2025-01-01T00:00:00Z")]);
        let stats = holding_stats(&cb, utc(NOW), &kinds, &prices);

        let symbols: Vec<&str> = stats.iter().map(|s| s.symbol.as_str()).collect();
        assert_eq!(symbols, vec!["AIR", "ZED"]);
        assert!(approx(find(&stats, "ZED").open_avg_days.unwrap(), 0.0));
    }

    #[test]
    fn open_lots_are_valued_at_the_current_price_when_it_is_known() {
        let mut cb = CostBasisResult::default();
        cb.open_lots.insert(
            "AIR".into(),
            vec![lot(10.0, 100.0, "2025-02-21T00:00:00Z"), lot(30.0, 100.0, "2025-05-12T00:00:00Z")],
        );
        cb.open_lots.insert("ZED".into(), vec![lot(1.0, 10.0, "2025-05-12T00:00:00Z")]); // prix inconnu
        let stats = holding_stats(&cb, utc(NOW), &kinds, &prices);

        let air = find(&stats, "AIR");
        assert_eq!(air.open_lots.len(), 2);
        assert!(approx(air.open_lots[0].days, 100.0));
        assert!(approx(air.open_lots[0].cost_eur, 1_000.0));
        assert!(approx(air.open_lots[0].value_eur.unwrap(), 1_500.0));
        assert!(approx(air.open_lots[1].value_eur.unwrap(), 4_500.0));
        assert!(find(&stats, "ZED").open_lots[0].value_eur.is_none());
    }

    #[test]
    fn closed_sales_give_one_row_per_stock_sale_with_duration_and_result() {
        let mut cb = CostBasisResult::default();
        let mut win = gain("AIR", "2025-04-01T00:00:00Z", vec![lot(10.0, 100.0, "2025-03-02T00:00:00Z")], false); // 30 j
        win.cost_eur = 1_000.0;
        win.proceeds_eur = 1_200.0;
        win.pnl_eur = 200.0;
        let mut loss = gain("SAN", "2025-03-01T00:00:00Z", vec![lot(10.0, 100.0, "2024-12-01T00:00:00Z")], false); // 90 j
        loss.cost_eur = 1_000.0;
        loss.proceeds_eur = 900.0;
        loss.pnl_eur = -100.0;
        cb.realized_gains.push(win);
        cb.realized_gains.push(loss);
        // Une vente de crypto (WITHDRAW) ne doit pas apparaître.
        cb.realized_gains.push(gain("BTC", "2025-03-01T00:00:00Z", vec![lot(0.5, 10_000.0, "2025-01-01T00:00:00Z")], false));

        let sales = closed_sales(&cb, &kinds);

        assert_eq!(sales.len(), 2);
        assert_eq!(sales[0].symbol, "SAN"); // triée par date de vente
        assert!(approx(sales[0].days.unwrap(), 90.0));
        assert!(approx(sales[0].pnl_pct.unwrap(), -10.0));
        assert_eq!(sales[1].sell_date, "2025-04-01");
        assert!(approx(sales[1].days.unwrap(), 30.0));
        assert!(approx(sales[1].pnl_pct.unwrap(), 20.0));
    }

    #[test]
    fn an_incomplete_sale_is_flagged_and_its_remainder_is_not_counted_in_the_duration() {
        let mut cb = CostBasisResult::default();
        let mut g = gain(
            "AIR",
            "2025-04-01T00:00:00Z",
            vec![lot(5.0, 100.0, "2025-03-02T00:00:00Z"), lot(5.0, 0.0, "2025-04-01T00:00:00Z")],
            true,
        );
        g.cost_eur = 500.0;
        g.pnl_eur = 700.0;
        cb.realized_gains.push(g);

        let sales = closed_sales(&cb, &kinds);
        assert!(sales[0].incomplete);
        assert!(approx(sales[0].days.unwrap(), 30.0));
    }

    #[test]
    fn a_sale_with_zero_cost_has_no_percentage() {
        let mut cb = CostBasisResult::default();
        cb.realized_gains.push(gain("AIR", "2025-04-01T00:00:00Z", vec![lot(1.0, 0.0, "2025-03-02T00:00:00Z")], false));
        assert!(closed_sales(&cb, &kinds)[0].pnl_pct.is_none());
    }
}
