//! Construit la matrice de corrélation des rendements journaliers,
//! combinant les actifs détenus (au-dessus d'un seuil de valeur) et les
//! indices/matières premières de référence (`benchmarks.rs`), sur une
//! fenêtre glissante de `lookback_days`.

use std::collections::{BTreeMap, HashMap};

use chrono::NaiveDate;
use chrono::{Duration, Utc};

use crate::market::benchmarks::BENCHMARKS;
use crate::market::prices::{binance_daily_closes, yahoo_daily_closes};
use crate::schema::AssetKind;
use crate::store::serialize::TxStore;

/// Fenêtres calculées à chaque run. Le label est celui utilisé comme clé
/// dans le JSON de sortie (consommé tel quel par dashboard.py).
pub const CORRELATION_WINDOWS: &[(&str, i64)] = &[
    ("90d", 90),
    ("6m", 182),
    ("1y", 365),
];


/// Restreint une série de rendements aux dates >= aujourd'hui - window_days.
fn windowed(returns: &BTreeMap<NaiveDate, f64>, window_days: i64) -> BTreeMap<NaiveDate, f64> {
    let cutoff = Utc::now().date_naive() - Duration::days(window_days);
    returns.range(cutoff..).map(|(d, v)| (*d, *v)).collect()
}

/// Récupère la série de clôtures pour un actif de portefeuille selon son
/// type. Best-effort : renvoie None si la source ne répond pas plutôt que
/// de faire échouer tout le calcul de corrélation pour un seul actif.
fn closes_for_portfolio_asset(symbol: &str, kind: AssetKind, ticker: Option<&str>, lookback_days: i64) -> Option<BTreeMap<NaiveDate, f64>> {
    let series = match kind {
        AssetKind::Crypto => binance_daily_closes(symbol, lookback_days).ok()?,
        AssetKind::Stock => yahoo_daily_closes(ticker?, lookback_days).ok()?,
        AssetKind::Cash => return None, // le cash n'a pas de rendement à corréler
    };
    if series.len() < 2 {
        return None;
    }
    Some(series.into_iter().collect())
}

fn closes_for_benchmark(ticker: &str, lookback_days: i64) -> Option<BTreeMap<NaiveDate, f64>> {
    let series = yahoo_daily_closes(ticker, lookback_days).ok()?;
    if series.len() < 2 {
        return None;
    }
    Some(series.into_iter().collect())
}

/// Transforme une série de prix en rendements journaliers simples
/// (r_t = p_t / p_{t-1} - 1), clé par date de fin de période.
fn to_returns(prices: &BTreeMap<NaiveDate, f64>) -> BTreeMap<NaiveDate, f64> {
    let mut out = BTreeMap::new();
    let mut prev: Option<f64> = None;
    for (date, price) in prices {
        if let Some(prev_price) = prev {
            if prev_price > 0.0 {
                out.insert(*date, price / prev_price - 1.0);
            }
        }
        prev = Some(*price);
    }
    out
}

/// Corrélation de Pearson entre deux séries de rendements, restreinte aux
/// dates communes aux deux. `None` si moins de 5 points communs (pas
/// assez de signal pour qu'un coefficient veuille dire quelque chose).
fn pearson(a: &BTreeMap<NaiveDate, f64>, b: &BTreeMap<NaiveDate, f64>) -> Option<f64> {
    let common: Vec<(f64, f64)> = a.iter().filter_map(|(date, ra)| b.get(date).map(|rb| (*ra, *rb))).collect();
    if common.len() < 5 {
        return None;
    }

    let n = common.len() as f64;
    let mean_a = common.iter().map(|(x, _)| x).sum::<f64>() / n;
    let mean_b = common.iter().map(|(_, y)| y).sum::<f64>() / n;

    let mut cov = 0.0;
    let mut var_a = 0.0;
    let mut var_b = 0.0;
    for (x, y) in &common {
        let dx = x - mean_a;
        let dy = y - mean_b;
        cov += dx * dy;
        var_a += dx * dx;
        var_b += dy * dy;
    }

    if var_a <= 0.0 || var_b <= 0.0 {
        return None;
    }
    Some(cov / (var_a.sqrt() * var_b.sqrt()))
}

/// Calcule la matrice de corrélation complète : actifs détenus dont la
/// valeur courante dépasse `min_value_eur`, plus les benchmarks fixes.
/// Résultat au format dict-de-dicts (label -> label -> coefficient),
/// directement sérialisable pour dashboard.json et consommable tel quel
/// par `pd.DataFrame(...)` côté Streamlit.
/// Calcule les matrices de corrélation pour toutes les fenêtres de
/// `CORRELATION_WINDOWS`, en une seule passe de fetch de prix (sur la
/// fenêtre la plus large) pour éviter de solliciter Yahoo/Binance une
/// fois par fenêtre. Résultat : label de fenêtre -> matrice (label ->
/// label -> coefficient), directement sérialisable pour dashboard.json.
pub fn compute_correlation_matrices(
    tx_store: &TxStore,
    holdings: &HashMap<String, f64>,
    prices_eur: &HashMap<String, f64>,
    min_value_eur: f64,
) -> HashMap<String, HashMap<String, HashMap<String, f64>>> {
    let max_lookback = CORRELATION_WINDOWS.iter().map(|(_, d)| *d).max().unwrap_or(365);

    let mut series: Vec<(String, BTreeMap<NaiveDate, f64>)> = Vec::new();

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
        if let Some(prices) = closes_for_portfolio_asset(symbol, asset.kind, asset.identifiers.ticker.as_deref(), max_lookback) {
            series.push((symbol.clone(), prices));
        } else {
            eprintln!("  [WARN corrélation] historique indisponible pour {symbol}, exclu de la matrice.");
        }
    }

    for (label, ticker) in BENCHMARKS {
        if let Some(prices) = closes_for_benchmark(ticker, max_lookback) {
            series.push((label.to_string(), prices));
        } else {
            eprintln!("  [WARN corrélation] historique indisponible pour le benchmark {label} ({ticker}), exclu.");
        }
    }

    // Rendements calculés une seule fois sur l'historique complet.
    let full_returns: Vec<(String, BTreeMap<NaiveDate, f64>)> =
        series.into_iter().map(|(label, prices)| (label, to_returns(&prices))).collect();

    let mut result: HashMap<String, HashMap<String, HashMap<String, f64>>> = HashMap::new();

    for (window_label, window_days) in CORRELATION_WINDOWS {
        let windowed_returns: Vec<(String, BTreeMap<NaiveDate, f64>)> = full_returns
            .iter()
            .map(|(label, r)| (label.clone(), windowed(r, *window_days)))
            .collect();

        let mut matrix: HashMap<String, HashMap<String, f64>> = HashMap::new();
        for (label_a, returns_a) in &windowed_returns {
            let mut row = HashMap::new();
            for (label_b, returns_b) in &windowed_returns {
                let coeff = if label_a == label_b { 1.0 } else { pearson(returns_a, returns_b).unwrap_or(0.0) };
                row.insert(label_b.clone(), coeff);
            }
            matrix.insert(label_a.clone(), row);
        }
        result.insert(window_label.to_string(), matrix);
    }

    result
}
