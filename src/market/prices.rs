//! src/market/prices.rs — récupération de prix (Yahoo/Binance) et
//! conversion EUR. Le stockage/cache persistant vit dans `cache.rs`.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use once_cell::sync::Lazy;
use serde_json::Value;
use thiserror::Error;

use crate::schema::AssetKind;

use super::cache::{is_live_bucket, Resolution, CACHE_1D, CACHE_1H};

pub use super::cache::{init_price_caches, save_price_caches};

const BINANCE_API: &str = "https://data-api.binance.vision/api/v3/klines";
const YAHOO_CHART_API: &str = "https://query1.finance.yahoo.com/v8/finance/chart";

#[derive(Debug, Error)]
pub enum PriceError {
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
}

fn client() -> &'static reqwest::blocking::Client {
    static CLIENT: Lazy<reqwest::blocking::Client> = Lazy::new(|| {
        reqwest::blocking::Client::builder()
            .user_agent("Mozilla/5.0")
            .timeout(Duration::from_secs(10))
            .build()
            .expect("client HTTP")
    });
    &CLIENT
}

// ---------------------------------------------------------------------
// UN SEUL point de fetch Yahoo : tout le monde passe par là.
// ---------------------------------------------------------------------

struct YahooSeries {
    points: Vec<(i64, f64)>,
    currency: String,
}

fn fetch_yahoo(ticker: &str, period1: i64, period2: i64, interval: &str) -> Result<YahooSeries, PriceError> {
    let url = format!("{YAHOO_CHART_API}/{ticker}");
    let resp = client()
        .get(&url)
        .query(&[
            ("period1", period1.to_string()),
            ("period2", period2.to_string()),
            ("interval", interval.to_string()),
            ("events", "history".to_string()),
        ])
        .send()?
        .error_for_status()?;

    let data: Value = resp.json()?;
    let result = data
        .get("chart")
        .and_then(|c| c.get("result"))
        .and_then(|r| r.as_array())
        .and_then(|a| a.first())
        .ok_or_else(|| PriceError::Message(format!("Yahoo KO pour {ticker}")))?;

    let currency = result
        .get("meta")
        .and_then(|m| m.get("currency"))
        .and_then(|c| c.as_str())
        .unwrap_or("USD")
        .to_string();

    let timestamps: Vec<i64> = result
        .get("timestamp")
        .and_then(|t| t.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_i64()).collect())
        .unwrap_or_default();
    let closes: Vec<Option<f64>> = result
        .get("indicators")
        .and_then(|i| i.get("quote"))
        .and_then(|q| q.as_array())
        .and_then(|a| a.first())
        .and_then(|q0| q0.get("close"))
        .and_then(|c| c.as_array())
        .map(|a| a.iter().map(|v| v.as_f64()).collect())
        .unwrap_or_default();

    let points = timestamps.into_iter().zip(closes).filter_map(|(ts, c)| c.map(|c| (ts, c))).collect();

    Ok(YahooSeries { points, currency })
}

pub fn normalize_currency_for_fx(currency: &str) -> (String, f64) {
    match currency {
        "GBp" | "GBX" => ("GBP".to_string(), 0.01),
        other => (other.to_string(), 1.0),
    }
}

fn closest_at_or_before(points: &[(i64, f64)], ts: i64) -> Option<f64> {
    points.iter().filter(|(t, _)| *t <= ts).max_by_key(|(t, _)| *t).map(|(_, p)| *p)
}

/// Taux EUR -> `currency`, avec cache 1h + repli statique. Point d'entrée
/// UNIQUE pour toute conversion de devise.
pub fn eur_rate(currency: &str, ts: i64) -> f64 {
    let (currency, _) = normalize_currency_for_fx(currency);
    if currency == "EUR" {
        return 1.0;
    }

    let aligned_ts = Resolution::Hour.align(ts);
    let live = is_live_bucket(aligned_ts, Resolution::Hour);

    let cached = if live {
        CACHE_1H.get_exact(&currency, aligned_ts)
    } else {
        CACHE_1H.get_closest(&currency, aligned_ts)
    };
    if let Some(rate) = cached {
        return rate;
    }

    let pair = format!("EUR{currency}=X");
    let period1 = Utc.timestamp_opt(aligned_ts, 0).unwrap() - chrono::Duration::days(3);
    let period2 = Utc.timestamp_opt(aligned_ts, 0).unwrap() + chrono::Duration::days(1);

    if let Ok(series) = fetch_yahoo(&pair, period1.timestamp(), period2.timestamp(), "1h") {
        if let Some(rate) = closest_at_or_before(&series.points, aligned_ts) {
            if rate > 0.0 {
                CACHE_1H.insert(&currency, aligned_ts, rate);
                return rate;
            }
        }
    }

    eprintln!("  [WARN fx] échec {currency} ts={aligned_ts}, fallback statique");
    match currency.as_str() {
        "USD" => 1.08,
        "GBP" => 0.87,
        _ => 1.0,
    }
}

// ---------------------------------------------------------------------
// Historique daily : logique de cache commune à Yahoo et Binance.
// ---------------------------------------------------------------------

/// Sert `days` jours de clôtures EUR pour `cache_key`, en ne rappelant
/// `fetch` que si le cache n'est pas frais (dernier point < hier) OU pas
/// assez profond (premier point > début de la fenêtre demandée). `fetch`
/// reçoit un `start_ts` (secondes) et doit renvoyer des (ts, close) déjà
/// convertis en EUR.
fn daily_closes_cached<F>(cache_key: &str, days: i64, fetch: F) -> Result<Vec<(chrono::NaiveDate, f64)>, PriceError>
where
    F: FnOnce(i64) -> Result<Vec<(i64, f64)>, PriceError>,
{
    let yesterday = Utc::now().date_naive() - chrono::Duration::days(1);
    let yesterday_ts = Resolution::Day.align(Utc.from_utc_datetime(&yesterday.and_hms_opt(0, 0, 0).unwrap()).timestamp());
    let cutoff_ts = Resolution::Day.align((Utc::now() - chrono::Duration::days(days)).timestamp());

    let fresh = CACHE_1D.latest(cache_key).map_or(false, |ts| ts >= yesterday_ts);
    let deep_enough = CACHE_1D.earliest(cache_key).map_or(false, |ts| ts <= cutoff_ts);

    if !(fresh && deep_enough) {
        // Marge de 5 jours pour absorber week-ends/jours fériés côté actions.
        let start_ts = cutoff_ts - Resolution::Day.seconds() * 5;
        for (ts, close) in fetch(start_ts)? {
            let day = Utc.timestamp_opt(ts, 0).unwrap().date_naive();
            CACHE_1D.insert_date(cache_key, day, close);
        }
    }

    Ok(CACHE_1D.range_days(cache_key, days))
}

pub fn yahoo_daily_closes(ticker: &str, days: i64) -> Result<Vec<(chrono::NaiveDate, f64)>, PriceError> {
    let cache_key = format!("YHO:{ticker}");
    daily_closes_cached(&cache_key, days, |start_ts| {
        let series = fetch_yahoo(ticker, start_ts, Utc::now().timestamp(), "1d")?;
        let (fx_currency, price_factor) = normalize_currency_for_fx(&series.currency);
        Ok(series
            .points
            .into_iter()
            .map(|(ts, mut close)| {
                close *= price_factor;
                if fx_currency != "EUR" {
                    close /= eur_rate(&fx_currency, ts);
                }
                (ts, close)
            })
            .collect())
    })
}

/// Dernière clôture Yahoo <= day_str (import XTB, coût d'acquisition...).
pub fn yahoo_historical_price(ticker: &str, day_str: &str) -> Result<(f64, String), PriceError> {
    let target = chrono::NaiveDate::parse_from_str(day_str, "%Y-%m-%d").map_err(|e| PriceError::Message(e.to_string()))?;
    let target_dt = Utc.from_utc_datetime(&target.and_hms_opt(0, 0, 0).unwrap());
    let series = fetch_yahoo(
        ticker,
        (target_dt - chrono::Duration::days(7)).timestamp(),
        (target_dt + chrono::Duration::days(1)).timestamp(),
        "1d",
    )?;

    series
        .points
        .iter()
        .filter(|(ts, _)| Utc.timestamp_opt(*ts, 0).unwrap().date_naive() <= target)
        .max_by_key(|(ts, _)| *ts)
        .map(|(_, p)| (*p, series.currency.clone()))
        .ok_or_else(|| PriceError::Message(format!("Pas de clôture pour {ticker} au {day_str}")))
}

// ---------------------------------------------------------------------
// Crypto -> EUR : UN SEUL chemin (direct SYMBOLEUR, sinon pont USDT),
// partagé par le prix live (1h) et l'historique (daily).
// ---------------------------------------------------------------------

fn fetch_binance_close(pair: &str, start_ms: i64, interval: &str, limit: &str) -> Result<Vec<(i64, f64)>, PriceError> {
    let resp = client()
        .get(BINANCE_API)
        .query(&[("symbol", pair), ("interval", interval), ("startTime", &start_ms.to_string()), ("limit", limit)])
        .send()?;
    if !resp.status().is_success() {
        return Ok(Vec::new()); // paire inexistante -> vide, pas d'erreur
    }
    let data: Vec<Value> = resp.json()?;
    Ok(data
        .iter()
        .filter_map(|k| {
            let ts = k.get(0)?.as_i64()?;
            let close = k.get(4)?.as_str()?.parse::<f64>().ok()?;
            Some((ts, close))
        })
        .collect())
}

/// Clôtures EUR d'une crypto sur `[start_ms, ...]` : paire directe
/// SYMBOLEUR d'abord, pont via USDT sinon (EURUSDT est liquide,
/// contrairement à EURUSDC). Point d'entrée UNIQUE crypto -> EUR.
fn crypto_closes_eur(symbol: &str, start_ms: i64, interval: &str, limit: &str) -> Result<Vec<(i64, f64)>, PriceError> {
    let direct = fetch_binance_close(&format!("{symbol}EUR"), start_ms, interval, limit)?;
    if !direct.is_empty() {
        return Ok(direct);
    }

    let usdt = fetch_binance_close(&format!("{symbol}USDT"), start_ms, interval, limit)?;
    if usdt.is_empty() {
        return Ok(Vec::new());
    }
    let eur_usdt: HashMap<i64, f64> = fetch_binance_close("EURUSDT", start_ms, interval, limit)?.into_iter().collect();
    Ok(usdt.into_iter().filter_map(|(ts, p)| eur_usdt.get(&ts).map(|&fx| (ts, p / fx))).collect())
}

pub fn binance_daily_closes(symbol: &str, days: i64) -> Result<Vec<(chrono::NaiveDate, f64)>, PriceError> {
    let cache_key = format!("BIN:{symbol}");
    daily_closes_cached(&cache_key, days, |start_ts| crypto_closes_eur(symbol, start_ts * 1000, "1d", "1000"))
}

// ---------------------------------------------------------------------
// Point d'entrée unique utilisé par le reste du code
// ---------------------------------------------------------------------

pub fn historical_price_eur(symbol: &str, time: DateTime<Utc>, kind: AssetKind, ticker: Option<&str>) -> f64 {
    let symbol = symbol.to_uppercase();
    let aligned_ts = Resolution::Hour.align(time.timestamp());
    let live = is_live_bucket(aligned_ts, Resolution::Hour);

    if kind == AssetKind::Cash {
        return match symbol.as_str() {
            "EUR" | "EURI" => 1.0,
            "USD" | "USDT" | "USDC" | "BUSD" => 1.0 / eur_rate("USD", aligned_ts),
            "GBP" => 1.0 / eur_rate("GBP", aligned_ts),
            _ => 0.0,
        };
    }

    // "Maintenant" -> il faut un point EXACTEMENT sur le bucket courant,
    // sinon on resservirait indéfiniment une vieille valeur (bug initial).
    // Date passée -> immuable, le point connu le plus proche avant suffit.
    let cached = if live {
        CACHE_1H.get_exact(&symbol, aligned_ts)
    } else {
        CACHE_1H.get_closest(&symbol, aligned_ts)
    };
    if let Some(price) = cached {
        return price;
    }

    let price = match kind {
        AssetKind::Crypto => crypto_closes_eur(&symbol, aligned_ts * 1000, "1h", "1")
            .ok()
            .and_then(|v| v.into_iter().next())
            .map(|(_, p)| p)
            .unwrap_or(0.0),
        AssetKind::Stock => {
            let Some(ticker) = ticker else { return 0.0 };
            let day_str = Utc.timestamp_opt(aligned_ts, 0).unwrap().format("%Y-%m-%d").to_string();
            match yahoo_historical_price(ticker, &day_str) {
                Ok((raw, currency)) => {
                    let (fx_currency, factor) = normalize_currency_for_fx(&currency);
                    let p = raw * factor;
                    if fx_currency != "EUR" {
                        p / eur_rate(&fx_currency, aligned_ts)
                    } else {
                        p
                    }
                }
                Err(_) => 0.0,
            }
        }
        AssetKind::Cash => unreachable!(),
    };

    if price > 0.0 {
        CACHE_1H.insert(&symbol, aligned_ts, price);
    }
    price
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // --- Fonctions pures : pas de réseau, testables directement ---

    #[test]
    fn normalize_currency_for_fx_converts_pence_to_pounds() {
        let (currency, factor) = normalize_currency_for_fx("GBp");
        assert_eq!(currency, "GBP");
        assert_eq!(factor, 0.01);

        let (currency, factor) = normalize_currency_for_fx("GBX");
        assert_eq!(currency, "GBP");
        assert_eq!(factor, 0.01);
    }

    #[test]
    fn normalize_currency_for_fx_leaves_other_currencies_untouched() {
        let (currency, factor) = normalize_currency_for_fx("EUR");
        assert_eq!(currency, "EUR");
        assert_eq!(factor, 1.0);

        let (currency, factor) = normalize_currency_for_fx("USD");
        assert_eq!(currency, "USD");
        assert_eq!(factor, 1.0);
    }

    #[test]
    fn closest_at_or_before_picks_the_latest_point_not_after_ts() {
        let points = vec![(10, 1.0), (20, 2.0), (30, 3.0)];

        assert_eq!(closest_at_or_before(&points, 25), Some(2.0));
        assert_eq!(closest_at_or_before(&points, 30), Some(3.0));
    }

    #[test]
    fn closest_at_or_before_returns_none_if_everything_is_in_the_future() {
        let points = vec![(10, 1.0), (20, 2.0)];
        assert_eq!(closest_at_or_before(&points, 5), None);
    }

    // --- daily_closes_cached : orchestration du cache, fetch injecté donc
    // testable sans réseau. C'est ici que vivait le bug de troncature
    // silencieuse (cache "frais" mais pas assez "profond").

    fn fake_points(days_back: i64, count: i64) -> Vec<(i64, f64)> {
        let today = Utc::now().date_naive();
        (0..count)
            .map(|i| {
                let day = today - chrono::Duration::days(days_back - i);
                let ts = Utc.from_utc_datetime(&day.and_hms_opt(0, 0, 0).unwrap()).timestamp();
                (ts, 100.0)
            })
            .collect()
    }

    #[test]
    fn daily_closes_cached_fetches_once_on_a_cold_cache() {
        let key = "TEST:cold_cache";
        let calls = AtomicUsize::new(0);

        let result = daily_closes_cached(key, 10, |_start_ts| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(fake_points(15, 16)) // 15 jours en arrière jusqu'à aujourd'hui
        });

        assert!(result.is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(!result.unwrap().is_empty());
    }

    #[test]
    fn daily_closes_cached_does_not_refetch_when_fresh_and_deep_enough() {
        let key = "TEST:no_refetch_needed";
        let calls = AtomicUsize::new(0);

        // Premier appel : cache vide -> fetch, on peuple 40 jours d'historique.
        let _ = daily_closes_cached(key, 30, |_start_ts| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(fake_points(39, 40))
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // Deuxième appel, même profondeur (30j) demandée : le cache est
        // déjà frais ET assez profond -> aucun nouveau fetch.
        let _ = daily_closes_cached(key, 30, |_start_ts| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn daily_closes_cached_refetches_when_a_deeper_history_is_requested() {
        // Ce test verrouille explicitement le bug corrigé : avant le
        // refactor, un cache "frais" (dernier point récent) était considéré
        // valide même s'il ne couvrait pas la fenêtre demandée, et
        // renvoyait silencieusement un historique tronqué.
        let key = "TEST:refetch_on_deeper_request";
        let calls = AtomicUsize::new(0);

        // Cache initial peu profond (15 jours).
        let _ = daily_closes_cached(key, 10, |_start_ts| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(fake_points(14, 15))
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // On redemande un historique bien plus long (365j) : le cache est
        // frais mais pas assez profond -> doit redéclencher un fetch.
        let _ = daily_closes_cached(key, 365, |_start_ts| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        });
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn daily_closes_cached_refetches_when_the_cache_is_stale() {
        let key = "TEST:refetch_on_stale_cache";
        let calls = AtomicUsize::new(0);

        // Cache initial dont le dernier point est vieux de 5 jours (pas
        // "hier ou plus récent" -> pas frais).
        let old_day = Utc::now().date_naive() - chrono::Duration::days(5);
        let ts = Utc.from_utc_datetime(&old_day.and_hms_opt(0, 0, 0).unwrap()).timestamp();
        let _ = daily_closes_cached(key, 10, |_start_ts| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![(ts, 100.0)])
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let _ = daily_closes_cached(key, 10, |_start_ts| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        });
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
