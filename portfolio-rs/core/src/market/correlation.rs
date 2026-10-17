//! Matrice de corrélation des rendements journaliers : actifs détenus
//! (au-dessus d'un seuil de valeur) + benchmarks (`benchmarks.rs`), sur des
//! fenêtres glissantes (`CORRELATION_WINDOWS`).

use std::collections::{BTreeMap, HashMap};
use chrono::{Duration, NaiveDate, Utc};

use crate::market::benchmarks::{Benchmark, BENCHMARKS};
use crate::market::MarketDataClient;
use crate::schema::AssetKind;
use crate::store::serialize::TxStore;

pub const CORRELATION_WINDOWS: &[(&str, i64)] = &[("90d", 90), ("6m", 182), ("1y", 365)];
const MIN_PAIRED_RETURNS: usize = 5;

type PriceSeries = BTreeMap<NaiveDate, f64>;

pub type CorrelationMatrix = HashMap<String, HashMap<String, Option<f64>>>;
pub type CorrelationMatrices = HashMap<String, CorrelationMatrix>;

fn to_series(points: Vec<(NaiveDate, f64)>) -> Option<PriceSeries> {
    (points.len() >= 2).then(|| points.into_iter().collect())
}

fn closes_for_portfolio_asset(
    market_client: &MarketDataClient,
    symbol: &str,
    kind: AssetKind,
    ticker: Option<&str>,
    lookback_days: i64,
) -> Option<PriceSeries> {
    let points = match kind {
        AssetKind::Crypto => market_client.binance_daily_closes(symbol, lookback_days).ok()?,
        AssetKind::Stock => market_client.yahoo_daily_closes(ticker?, lookback_days).ok()?,
        AssetKind::Cash => return None,
    };
    to_series(points)
}

fn closes_for_benchmark(market_client: &MarketDataClient, bench: &Benchmark, lookback_days: i64) -> Option<PriceSeries> {
    let points = if bench.in_eur {
        market_client.yahoo_daily_closes(bench.ticker, lookback_days)
    } else {
        market_client.yahoo_daily_closes(bench.ticker, lookback_days)
    }
    .ok()?;
    to_series(points)
}

fn paired_returns(a: &PriceSeries, b: &PriceSeries, cutoff: NaiveDate) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let mut prev: Option<(f64, f64)> = None;

    for (date, &pa) in a.range(cutoff..) {
        let Some(&pb) = b.get(date) else { continue };
        if let Some((qa, qb)) = prev {
            if qa > 0.0 && qb > 0.0 {
                out.push((pa / qa - 1.0, pb / qb - 1.0));
            }
        }
        prev = Some((pa, pb));
    }
    out
}

fn pearson(pairs: &[(f64, f64)]) -> Option<f64> {
    if pairs.len() < MIN_PAIRED_RETURNS {
        return None;
    }

    let n = pairs.len() as f64;
    let (sum_a, sum_b) = pairs.iter().fold((0.0, 0.0), |(sa, sb), &(x, y)| (sa + x, sb + y));
    let (mean_a, mean_b) = (sum_a / n, sum_b / n);

    let (mut cov, mut var_a, mut var_b) = (0.0, 0.0, 0.0);
    for &(x, y) in pairs {
        let (dx, dy) = (x - mean_a, y - mean_b);
        cov += dx * dy;
        var_a += dx * dx;
        var_b += dy * dy;
    }

    if var_a <= 0.0 || var_b <= 0.0 {
        return None;
    }
    Some(cov / (var_a.sqrt() * var_b.sqrt()))
}

fn build_matrix(series: &[(String, PriceSeries)], cutoff: NaiveDate) -> CorrelationMatrix {
    let n = series.len();
    let mut coeffs: Vec<Vec<Option<f64>>> = vec![vec![None; n]; n];

    for i in 0..n {
        coeffs[i][i] = Some(1.0);
        for j in (i + 1)..n {
            let pairs = paired_returns(&series[i].1, &series[j].1, cutoff);
            let coeff = pearson(&pairs);
            coeffs[i][j] = coeff;
            coeffs[j][i] = coeff;
        }
    }

    series
        .iter()
        .enumerate()
        .map(|(i, (label_a, _))| {
            let row: HashMap<String, Option<f64>> =
                series.iter().enumerate().map(|(j, (label_b, _))| (label_b.clone(), coeffs[i][j])).collect();
            (label_a.clone(), row)
        })
        .collect()
}

pub fn compute_correlation_matrices(
    tx_store: &TxStore,
    holdings: &HashMap<String, f64>,
    prices_eur: &HashMap<String, f64>,
    min_value_eur: f64,
    market_client: &MarketDataClient,
) -> CorrelationMatrices {
    let max_lookback = CORRELATION_WINDOWS.iter().map(|(_, d)| *d).max().unwrap_or(365);
    let mut series: Vec<(String, PriceSeries)> = Vec::new();

    for (symbol, quantity) in holdings {
        if *quantity <= 0.0 {
            continue;
        }
        let Some(asset) = tx_store.assets.get(symbol) else { continue };
        if asset.kind == AssetKind::Cash {
            continue;
        }
        let value_eur = quantity * prices_eur.get(symbol).copied().unwrap_or(0.0);
        if value_eur < min_value_eur {
            continue;
        }
        match closes_for_portfolio_asset(market_client, symbol, asset.kind, asset.identifiers.ticker.as_deref(), max_lookback) {
            Some(prices) => series.push((symbol.clone(), prices)),
            None => eprintln!("  [WARN corrélation] historique indisponible pour {symbol}, exclu de la matrice."),
        }
    }

    for bench in BENCHMARKS {
        match closes_for_benchmark(market_client, bench, max_lookback) {
            Some(prices) => series.push((bench.label.to_string(), prices)),
            None => eprintln!(
                "  [WARN corrélation] historique indisponible pour le benchmark {} ({}), exclu.",
                bench.label, bench.ticker
            ),
        }
    }

    let today = Utc::now().date_naive();
    CORRELATION_WINDOWS
        .iter()
        .map(|(label, days)| (label.to_string(), build_matrix(&series, today - Duration::days(*days))))
        .collect()
}
