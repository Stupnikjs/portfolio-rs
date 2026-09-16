//! src/market/prices.rs — version simplifiée

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::Duration;
use std::path::Path;

use chrono::{DateTime, TimeZone, Utc};
use once_cell::sync::Lazy;
use serde_json::Value;
use serde::{Serialize, Deserialize};
use thiserror::Error;

use crate::schema::AssetKind;

const BINANCE_API: &str = "https://data-api.binance.vision/api/v3/klines";
const YAHOO_CHART_API: &str = "https://query1.finance.yahoo.com/v8/finance/chart";

#[derive(Debug, Error)]
pub enum PriceError {
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
}

// ---------------------------------------------------------------------
// Cache (inchangé dans son principe : une BTreeMap<ts, prix> par symbole,
// une instance par résolution)
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Resolution { Hour, Day }

impl Resolution {
    fn seconds(self) -> i64 { match self { Resolution::Hour => 3_600, Resolution::Day => 86_400 } }
    fn align(self, ts: i64) -> i64 { ts - ts.rem_euclid(self.seconds()) }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PriceCache { data: HashMap<String, BTreeMap<i64, f64>> }

impl PriceCache {
    fn load(path: &Path) -> Self {
        std::fs::read(path).ok().and_then(|b| bincode::deserialize(&b).ok()).unwrap_or_default()
    }
    fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(p) = path.parent() { std::fs::create_dir_all(p)?; }
        std::fs::write(path, bincode::serialize(self).expect("bincode serialize"))
    }
    fn get_closest(&self, symbol: &str, ts: i64) -> Option<f64> {
        self.data.get(&symbol.to_uppercase())?.range(..=ts).next_back().map(|(_, &v)| v)
    }
    fn latest_ts(&self, symbol: &str) -> Option<i64> {
        self.data.get(&symbol.to_uppercase())?.keys().next_back().copied()
    }
    fn range_since(&self, symbol: &str, since_ts: i64) -> Vec<(i64, f64)> {
        self.data.get(&symbol.to_uppercase())
            .map(|m| m.range(since_ts..).map(|(&k, &v)| (k, v)).collect())
            .unwrap_or_default()
    }
    fn insert(&mut self, symbol: &str, ts: i64, price: f64) {
        self.data.entry(symbol.to_uppercase()).or_default().insert(ts, price);
    }
}

struct ResolutionCache { resolution: Resolution, cache: Mutex<PriceCache> }

impl ResolutionCache {
    fn empty(resolution: Resolution) -> Self { Self { resolution, cache: Mutex::new(PriceCache::default()) } }
    fn init(&self, path: &Path) { *self.cache.lock().unwrap() = PriceCache::load(path); }
    fn save(&self, path: &Path) {
        if let Err(e) = self.cache.lock().unwrap().save(path) {
            eprintln!("Erreur sauvegarde cache: {e}");
        }
    }
    fn get_closest(&self, symbol: &str, ts: i64) -> Option<f64> {
        self.cache.lock().unwrap().get_closest(symbol, self.resolution.align(ts))
    }
    fn latest(&self, symbol: &str) -> Option<i64> { self.cache.lock().unwrap().latest_ts(symbol) }
    fn range_days(&self, symbol: &str, days: i64) -> Vec<(chrono::NaiveDate, f64)> {
        let cutoff = self.resolution.align((Utc::now() - chrono::Duration::days(days)).timestamp());
        self.cache.lock().unwrap().range_since(symbol, cutoff).into_iter()
            .map(|(ts, price)| (Utc.timestamp_opt(ts, 0).unwrap().date_naive(), price)).collect()
    }
    fn insert(&self, symbol: &str, ts: i64, price: f64) {
        self.cache.lock().unwrap().insert(symbol, self.resolution.align(ts), price);
    }
    fn insert_date(&self, symbol: &str, date: chrono::NaiveDate, price: f64) {
        let ts = Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).unwrap()).timestamp();
        self.insert(symbol, ts, price);
    }
}

static CACHE_1H: Lazy<ResolutionCache> = Lazy::new(|| ResolutionCache::empty(Resolution::Hour));
static CACHE_1D: Lazy<ResolutionCache> = Lazy::new(|| ResolutionCache::empty(Resolution::Day));

pub fn init_price_caches(dir: &Path) {
    CACHE_1H.init(&dir.join("price_cache_1h.bin"));
    CACHE_1D.init(&dir.join("price_cache_1d.bin"));
}
pub fn save_price_caches(dir: &Path) {
    CACHE_1H.save(&dir.join("price_cache_1h.bin"));
    CACHE_1D.save(&dir.join("price_cache_1d.bin"));
}

fn client() -> &'static reqwest::blocking::Client {
    static CLIENT: Lazy<reqwest::blocking::Client> = Lazy::new(|| {
        reqwest::blocking::Client::builder().user_agent("Mozilla/5.0")
            .timeout(Duration::from_secs(10)).build().expect("client HTTP")
    });
    &CLIENT
}

// ---------------------------------------------------------------------
// UN SEUL point de fetch Yahoo : tout le monde passe par là.
// ---------------------------------------------------------------------

struct YahooSeries { points: Vec<(i64, f64)>, currency: String }

fn fetch_yahoo(ticker: &str, period1: i64, period2: i64, interval: &str) -> Result<YahooSeries, PriceError> {
    let url = format!("{YAHOO_CHART_API}/{ticker}");
    let resp = client().get(&url)
        .query(&[
            ("period1", period1.to_string()),
            ("period2", period2.to_string()),
            ("interval", interval.to_string()),
            ("events", "history".to_string()),
        ])
        .send()?.error_for_status()?;

    let data: Value = resp.json()?;
    let result = data.get("chart").and_then(|c| c.get("result")).and_then(|r| r.as_array())
        .and_then(|a| a.first())
        .ok_or_else(|| PriceError::Message(format!("Yahoo KO pour {ticker}")))?;

    let currency = result.get("meta").and_then(|m| m.get("currency"))
        .and_then(|c| c.as_str()).unwrap_or("USD").to_string();

    let timestamps: Vec<i64> = result.get("timestamp").and_then(|t| t.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_i64()).collect()).unwrap_or_default();
    let closes: Vec<Option<f64>> = result.get("indicators").and_then(|i| i.get("quote"))
        .and_then(|q| q.as_array()).and_then(|a| a.first()).and_then(|q0| q0.get("close"))
        .and_then(|c| c.as_array()).map(|a| a.iter().map(|v| v.as_f64()).collect()).unwrap_or_default();

    let points = timestamps.into_iter().zip(closes)
        .filter_map(|(ts, c)| c.map(|c| (ts, c)))
        .collect();

    Ok(YahooSeries { points, currency })
}

pub fn normalize_currency_for_fx(currency: &str) -> (String, f64) {
    match currency {
        "GBp" | "GBX" => ("GBP".to_string(), 0.01),
        other => (other.to_string(), 1.0),
    }
}

/// Taux EUR -> `currency`, avec cache 1h + repli statique. Point d'entrée
/// UNIQUE pour toute conversion de devise (remplace eur_fx_rate_1h +
/// fallback_fx_rate + la logique dupliquée ailleurs).
pub fn eur_rate(currency: &str, ts: i64) -> f64 {
    let (currency, _) = normalize_currency_for_fx(currency);
    if currency == "EUR" { return 1.0; }

    let aligned_ts = Resolution::Hour.align(ts);
    if let Some(rate) = CACHE_1H.get_closest(&currency, aligned_ts) {
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
    match currency.as_str() { "USD" => 1.08, "GBP" => 0.87, _ => 1.0 }
}

fn closest_at_or_before(points: &[(i64, f64)], ts: i64) -> Option<f64> {
    points.iter().filter(|(t, _)| *t <= ts).max_by_key(|(t, _)| *t).map(|(_, p)| *p)
}

// ---------------------------------------------------------------------
// Historique daily (actions ET conversion FX en une passe)
// ---------------------------------------------------------------------

pub fn yahoo_daily_closes(ticker: &str, days: i64) -> Result<Vec<(chrono::NaiveDate, f64)>, PriceError> {
    let cache_key = format!("YHO:{ticker}");
    let yesterday = Utc::now().date_naive() - chrono::Duration::days(1);
    let yesterday_ts = Resolution::Day.align(Utc.from_utc_datetime(&yesterday.and_hms_opt(0,0,0).unwrap()).timestamp());

    if CACHE_1D.latest(&cache_key).map_or(false, |ts| ts >= yesterday_ts) {
        return Ok(CACHE_1D.range_days(&cache_key, days));
    }

    let end = Utc::now();
    let start = end - chrono::Duration::days(days + 5);
    let series = fetch_yahoo(ticker, start.timestamp(), end.timestamp(), "1d")?;
    let (fx_currency, price_factor) = normalize_currency_for_fx(&series.currency);

    for (ts, mut close) in series.points {
        close *= price_factor;
        if fx_currency != "EUR" {
            close /= eur_rate(&fx_currency, ts);
        }
        let day = Utc.timestamp_opt(ts, 0).unwrap().date_naive();
        CACHE_1D.insert_date(&cache_key, day, close);
    }

    Ok(CACHE_1D.range_days(&cache_key, days))
}

/// Dernière clôture Yahoo <= day_str (import XTB, coût d'acquisition...).
pub fn yahoo_historical_price(ticker: &str, day_str: &str) -> Result<(f64, String), PriceError> {
    let target = chrono::NaiveDate::parse_from_str(day_str, "%Y-%m-%d")
        .map_err(|e| PriceError::Message(e.to_string()))?;
    let target_dt = Utc.from_utc_datetime(&target.and_hms_opt(0,0,0).unwrap());
    let series = fetch_yahoo(ticker, (target_dt - chrono::Duration::days(7)).timestamp(),
        (target_dt + chrono::Duration::days(1)).timestamp(), "1d")?;

    series.points.iter()
        .filter(|(ts, _)| Utc.timestamp_opt(*ts, 0).unwrap().date_naive() <= target)
        .max_by_key(|(ts, _)| *ts)
        .map(|(_, p)| (*p, series.currency.clone()))
        .ok_or_else(|| PriceError::Message(format!("Pas de clôture pour {ticker} au {day_str}")))
}

// ---------------------------------------------------------------------
// Crypto : paire EUR directe d'abord, USDT en pont sinon (PLUS d'USDC
// comme intermédiaire — EURUSDC n'est pas une paire fiable sur Binance)
// ---------------------------------------------------------------------

fn fetch_binance_close(pair: &str, start_ms: i64, interval: &str, limit: &str) -> Result<Vec<(i64, f64)>, PriceError> {
    let resp = client().get(BINANCE_API)
        .query(&[("symbol", pair), ("interval", interval), ("startTime", &start_ms.to_string()), ("limit", limit)])
        .send()?;
    if !resp.status().is_success() { return Ok(Vec::new()); } // paire inexistante -> vide, pas d'erreur
    let data: Vec<Value> = resp.json()?;
    Ok(data.iter().filter_map(|k| {
        let ts = k.get(0)?.as_i64()?;
        let close = k.get(4)?.as_str()?.parse::<f64>().ok()?;
        Some((ts, close))
    }).collect())
}

fn crypto_price_eur_at(symbol: &str, aligned_ts: i64) -> Option<f64> {
    let start_ms = aligned_ts * 1000;

    // 1. paire directe SYMBOLEUR (BTC, ETH, majors...)
    if let Some((_, p)) = fetch_binance_close(&format!("{symbol}EUR"), start_ms, "1h", "1").ok()?.into_iter().next() {
        return Some(p);
    }

    // 2. pont via USDT (EURUSDT est une paire liquide, contrairement à EURUSDC)
    let usdt_price = fetch_binance_close(&format!("{symbol}USDT"), start_ms, "1h", "1").ok()?.into_iter().next()?.1;
    let eur_usdt = fetch_binance_close("EURUSDT", start_ms, "1h", "1").ok()?.into_iter().next()?.1;
    if eur_usdt > 0.0 { Some(usdt_price / eur_usdt) } else { None }
}

pub fn binance_daily_closes(symbol: &str, days: i64) -> Result<Vec<(chrono::NaiveDate, f64)>, PriceError> {
    let cache_key = format!("BIN:{symbol}");
    let yesterday = Utc::now().date_naive() - chrono::Duration::days(1);
    let yesterday_ts = Resolution::Day.align(Utc.from_utc_datetime(&yesterday.and_hms_opt(0,0,0).unwrap()).timestamp());

    if CACHE_1D.latest(&cache_key).map_or(false, |ts| ts >= yesterday_ts) {
        return Ok(CACHE_1D.range_days(&cache_key, days));
    }

    let end_ms = Utc::now().timestamp_millis();
    let start_ms = (Utc::now() - chrono::Duration::days(days + 2)).timestamp_millis();
    // essai direct EUR d'abord (une seule requête si dispo)
    let direct = fetch_binance_close(&format!("{symbol}EUR"), start_ms, "1d", "1000")?;
    let points = if !direct.is_empty() {
        direct
    } else {
        let usdt = fetch_binance_close(&format!("{symbol}USDT"), start_ms, "1d", "1000")?;
        let eur_usdt: HashMap<i64, f64> = fetch_binance_close("EURUSDT", start_ms, "1d", "1000")?.into_iter().collect();
        usdt.into_iter().filter_map(|(ts, p)| eur_usdt.get(&ts).map(|&fx| (ts, p / fx))).collect()
    };

    for (ts, close) in points {
        let day = Utc.timestamp_millis_opt(ts).single().unwrap().date_naive();
        CACHE_1D.insert_date(&cache_key, day, close);
    }
    Ok(CACHE_1D.range_days(&cache_key, days))
}

// ---------------------------------------------------------------------
// Point d'entrée unique utilisé par le reste du code
// ---------------------------------------------------------------------

pub fn historical_price_eur(symbol: &str, time: DateTime<Utc>, kind: AssetKind, ticker: Option<&str>) -> f64 {
    let symbol = symbol.to_uppercase();
    let aligned_ts = Resolution::Hour.align(time.timestamp());

    if kind == AssetKind::Cash {
        return match symbol.as_str() {
            "EUR" | "EURI" => 1.0,
            "USD" | "USDT" | "USDC" | "BUSD" => 1.0 / eur_rate("USD", aligned_ts),
            "GBP" => 1.0 / eur_rate("GBP", aligned_ts),
            _ => 0.0,
        };
    }

    if let Some(price) = CACHE_1H.get_closest(&symbol, aligned_ts) {
        return price;
    }

    let price = match kind {
        AssetKind::Crypto => crypto_price_eur_at(&symbol, aligned_ts).unwrap_or(0.0),
        AssetKind::Stock => {
            let Some(ticker) = ticker else { return 0.0 };
            let day_str = Utc.timestamp_opt(aligned_ts, 0).unwrap().format("%Y-%m-%d").to_string();
            match yahoo_historical_price(ticker, &day_str) {
                Ok((raw, currency)) => {
                    let (fx_currency, factor) = normalize_currency_for_fx(&currency);
                    let p = raw * factor;
                    if fx_currency != "EUR" { p / eur_rate(&fx_currency, aligned_ts) } else { p }
                }
                Err(_) => 0.0,
            }
        }
        AssetKind::Cash => unreachable!(),
    };

    if price > 0.0 { CACHE_1H.insert(&symbol, aligned_ts, price); }
    price
}