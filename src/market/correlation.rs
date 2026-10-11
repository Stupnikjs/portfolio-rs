//! Matrice de corrélation des rendements journaliers : actifs détenus
//! (au-dessus d'un seuil de valeur) + benchmarks (`benchmarks.rs`), sur des
//! fenêtres glissantes (`CORRELATION_WINDOWS`).
//!
//! Alignement calendaire. Les marchés n'ont pas le même calendrier (crypto
//! 7j/7, actions 5j/7, fériés propres à chaque place). Calculer les
//! rendements de chaque série sur SES propres clôtures consécutives puis les
//! apparier par date compare, le lundi, le rendement vendredi->lundi (3 jours)
//! du Nasdaq au rendement dimanche->lundi (1 jour) de l'ETH : le coefficient
//! est biaisé vers le bas. Ici, pour CHAQUE PAIRE, on ne garde que les dates
//! communes, puis on calcule les deux rendements entre les mêmes dates
//! consécutives.
//!
//! Limite restante : les clôtures n'ont pas la même heure (Binance 00:00 UTC,
//! Nasdaq ~21:00 UTC). Pour s'en affranchir, comparer aussi des rendements
//! hebdomadaires.
//!
//! Prérequis : `yahoo_daily_closes_raw` dans prices.rs (voir notes).

use std::collections::{BTreeMap, HashMap};

use chrono::{Duration, NaiveDate, Utc};

use crate::market::benchmarks::{Benchmark, BENCHMARKS};
use crate::market::prices::{binance_daily_closes, yahoo_daily_closes, yahoo_daily_closes_raw};
use crate::schema::AssetKind;
use crate::store::serialize::TxStore;

/// Fenêtres calculées à chaque run. Le label est la clé dans le JSON de
/// sortie (consommé tel quel par dashboard.py).
pub const CORRELATION_WINDOWS: &[(&str, i64)] = &[("90d", 90), ("6m", 182), ("1y", 365)];

/// Nombre minimal de rendements appariés pour qu'un coefficient ait un sens.
const MIN_PAIRED_RETURNS: usize = 5;

type PriceSeries = BTreeMap<NaiveDate, f64>;

/// label -> label -> coefficient. `None` (sérialisé en `null`) veut dire
/// « on ne sait pas », jamais « zéro » : ne pas le convertir en 0.0 en amont.
pub type CorrelationMatrix = HashMap<String, HashMap<String, Option<f64>>>;
pub type CorrelationMatrices = HashMap<String, CorrelationMatrix>;

fn to_series(points: Vec<(NaiveDate, f64)>) -> Option<PriceSeries> {
    (points.len() >= 2).then(|| points.into_iter().collect())
}

/// Best-effort : `None` si la source ne répond pas, plutôt que de faire
/// échouer toute la matrice pour un seul actif.
fn closes_for_portfolio_asset(
    symbol: &str,
    kind: AssetKind,
    ticker: Option<&str>,
    lookback_days: i64,
) -> Option<PriceSeries> {
    let points = match kind {
        AssetKind::Crypto => binance_daily_closes(symbol, lookback_days).ok()?,
        AssetKind::Stock => yahoo_daily_closes(ticker?, lookback_days).ok()?,
        AssetKind::Cash => return None, // pas de rendement à corréler
    };
    to_series(points)
}

fn closes_for_benchmark(bench: &Benchmark, lookback_days: i64) -> Option<PriceSeries> {
    let points = if bench.in_eur {
        yahoo_daily_closes(bench.ticker, lookback_days)
    } else {
        yahoo_daily_closes_raw(bench.ticker, lookback_days)
    }
    .ok()?;
    to_series(points)
}

/// Rendements appariés de `a` et `b` entre dates communes consécutives,
/// à partir de `cutoff` (inclus).
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

/// Corrélation de Pearson d'une série de couples. `None` si pas assez de
/// points, ou si une des deux séries a une variance nulle (prix figé :
/// souvent un symptôme de données dégradées, pas une vraie absence de mouvement).
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

/// Matrice complète pour une fenêtre. Symétrique : on ne calcule que le
/// triangle supérieur.
fn build_matrix(series: &[(String, PriceSeries)], cutoff: NaiveDate) -> CorrelationMatrix {
    let n = series.len();
    let mut coeffs: Vec<Vec<Option<f64>>> = vec![vec![None; n]; n];

    for i in 0..n {
        coeffs[i][i] = Some(1.0);
        for j in (i + 1)..n {
            let pairs = paired_returns(&series[i].1, &series[j].1, cutoff);
            let coeff = pearson(&pairs);
            if coeff.is_none() {
                eprintln!(
                    "  [DEBUG corrélation] {} / {} : {} rendement(s) apparié(s), coefficient indéterminé",
                    series[i].0,
                    series[j].0,
                    pairs.len()
                );
            }
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

/// Matrices pour toutes les fenêtres de `CORRELATION_WINDOWS`, en une seule
/// passe de fetch de prix (sur la fenêtre la plus large).
pub fn compute_correlation_matrices(
    tx_store: &TxStore,
    holdings: &HashMap<String, f64>,
    prices_eur: &HashMap<String, f64>,
    min_value_eur: f64,
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
        match closes_for_portfolio_asset(symbol, asset.kind, asset.identifiers.ticker.as_deref(), max_lookback) {
            Some(prices) => series.push((symbol.clone(), prices)),
            None => eprintln!("  [WARN corrélation] historique indisponible pour {symbol}, exclu de la matrice."),
        }
    }

    for bench in BENCHMARKS {
        match closes_for_benchmark(bench, max_lookback) {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Octobre 2026 : le 9 est un vendredi.
    fn d(day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, day).unwrap()
    }

    fn series(points: &[(u32, f64)]) -> PriceSeries {
        points.iter().map(|&(day, p)| (d(day), p)).collect()
    }

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn paired_returns_span_the_same_interval_for_both_series() {
        // Crypto : tous les jours. Action : vendredi puis lundi seulement.
        let crypto = series(&[(9, 100.0), (10, 110.0), (11, 120.0), (12, 130.0)]);
        let stock = series(&[(9, 10.0), (12, 13.0)]);

        let pairs = paired_returns(&crypto, &stock, d(1));

        // Un seul rendement apparié, vendredi -> lundi, pour les DEUX séries.
        assert_eq!(pairs.len(), 1);
        assert!(approx(pairs[0].0, 0.3));
        assert!(approx(pairs[0].1, 0.3));
    }

    #[test]
    fn paired_returns_ignore_dates_before_the_cutoff() {
        let a = series(&[(1, 1.0), (2, 2.0), (3, 3.0), (4, 4.0)]);
        assert_eq!(paired_returns(&a, &a, d(3)).len(), 1); // 3 -> 4 seulement
    }

    #[test]
    fn pearson_of_identical_series_is_one_and_of_opposite_series_is_minus_one() {
        let same: Vec<(f64, f64)> = (1..=10).map(|i| (i as f64 * 0.01, i as f64 * 0.01)).collect();
        let opposite: Vec<(f64, f64)> = (1..=10).map(|i| (i as f64 * 0.01, -(i as f64) * 0.01)).collect();

        assert!(approx(pearson(&same).unwrap(), 1.0));
        assert!(approx(pearson(&opposite).unwrap(), -1.0));
    }

    #[test]
    fn pearson_is_unknown_with_too_few_points_or_zero_variance() {
        assert_eq!(pearson(&[(0.01, 0.02); 3]), None);

        let flat: Vec<(f64, f64)> = (1..=10).map(|i| (0.0, i as f64 * 0.01)).collect();
        assert_eq!(pearson(&flat), None);
    }

    #[test]
    fn build_matrix_is_symmetric_with_unit_diagonal() {
        let a = series(&[(1, 1.0), (2, 1.1), (3, 1.0), (4, 1.2), (5, 1.1), (6, 1.3), (7, 1.2)]);
        let b = series(&[(1, 5.0), (2, 5.4), (3, 5.1), (4, 5.9), (5, 5.3), (6, 6.4), (7, 5.8)]);
        let m = build_matrix(&[("A".to_string(), a), ("B".to_string(), b)], d(1));

        assert_eq!(m["A"]["A"], Some(1.0));
        assert_eq!(m["B"]["B"], Some(1.0));
        assert_eq!(m["A"]["B"], m["B"]["A"]);
        assert!(m["A"]["B"].is_some());
    }
}
