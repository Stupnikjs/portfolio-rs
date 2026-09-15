//! Portage de src/market/prices.py. Utilise l'API publique de Binance
//! pour la crypto, et Yahoo Finance pour les actions.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::thread;
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

// --- CACHE PERSISTANT 1H (BINAIRE) ---
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PriceCache {
    pub data: HashMap<String, BTreeMap<i64, f64>>,
}

impl PriceCache {
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| bincode::deserialize(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(p) = path.parent() { std::fs::create_dir_all(p)?; }
        let bytes = bincode::serialize(self).expect("bincode serialize");
        std::fs::write(path, bytes)
    }

    pub fn get_closest(&self, symbol: &str, target_ts: i64) -> Option<f64> {
        let map = self.data.get(&symbol.to_uppercase())?;
        map.range(..=target_ts).next_back().map(|(_, &v)| v)
    }

    pub fn insert(&mut self, symbol: &str, ts: i64, price: f64) {
        self.data.entry(symbol.to_uppercase()).or_default().insert(ts, price);
    }
}

static PRICE_CACHE: Lazy<Mutex<PriceCache>> = Lazy::new(|| Mutex::new(PriceCache::default()));

pub fn init_price_cache(path: &Path) {
    let cache = PriceCache::load(path);
    *PRICE_CACHE.lock().unwrap() = cache;
}


pub fn save_price_cache(path: &Path) {
    if let Err(e) = PRICE_CACHE.lock().unwrap().save(path) {
        eprintln!("Erreur sauvegarde price_cache.bin: {e}");
    }
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

// --- caches best-effort en mémoire pour les tx ponctuelles (FX, etc) ---
static BINANCE_KLINES_CACHE: Lazy<Mutex<HashMap<(String, String), Vec<Value>>>> = Lazy::new(|| Mutex::new(HashMap::new()));
static YAHOO_PRICE_CACHE: Lazy<Mutex<HashMap<(String, String), (f64, String)>>> = Lazy::new(|| Mutex::new(HashMap::new()));

fn binance_klines(symbol_pair: &str, day_str: &str) -> Result<Vec<Value>, PriceError> {
    let key = (symbol_pair.to_string(), day_str.to_string());
    if let Some(cached) = BINANCE_KLINES_CACHE.lock().unwrap().get(&key) {
        return Ok(cached.clone());
    }

    thread::sleep(Duration::from_millis(100));

    let dt = chrono::NaiveDate::parse_from_str(day_str, "%Y-%m-%d")
        .map_err(|e| PriceError::Message(e.to_string()))?
        .and_hms_opt(0, 0, 0)
        .unwrap();
    let start_ms = Utc.from_utc_datetime(&dt).timestamp_millis();

    let resp = client()
        .get(BINANCE_API)
        .query(&[
            ("symbol", symbol_pair),
            ("interval", "1d"),
            ("startTime", &start_ms.to_string()),
            ("limit", "1"),
        ])
        .send()?
        .error_for_status()?;

    let data: Vec<Value> = resp.json()?;
    BINANCE_KLINES_CACHE.lock().unwrap().insert(key, data.clone());
    Ok(data)
}

/// Récupère la dernière clôture Yahoo disponible <= day_str.
pub fn yahoo_historical_price(ticker: &str, day_str: &str) -> Result<(f64, String), PriceError> {
    let key = (ticker.to_string(), day_str.to_string());
    if let Some(cached) = YAHOO_PRICE_CACHE.lock().unwrap().get(&key) {
        return Ok(cached.clone());
    }

    let target_date = chrono::NaiveDate::parse_from_str(day_str, "%Y-%m-%d").map_err(|e| PriceError::Message(e.to_string()))?;
    let target = Utc.from_utc_datetime(&target_date.and_hms_opt(0, 0, 0).unwrap());

    let period1 = (target - chrono::Duration::days(7)).timestamp();
    let period2 = (target + chrono::Duration::days(1)).timestamp();
    let url = format!("{YAHOO_CHART_API}/{ticker}");
    let resp = client()
        .get(&url)
        .query(&[
            ("period1", period1.to_string()),
            ("period2", period2.to_string()),
            ("interval", "1d".to_string()),
            ("events", "history".to_string()),
        ])
        .send()?
        .error_for_status()?;

    let data: Value = resp.json()?;
    let result = data
        .get("chart")
        .and_then(|c| c.get("result"))
        .and_then(|r| r.as_array())
        .and_then(|arr| arr.first())
        .ok_or_else(|| PriceError::Message(format!("Yahoo n'a pas trouvé de résultat pour {ticker}")))?;

    let meta = result.get("meta").cloned().unwrap_or(Value::Null);
    let currency = meta.get("currency").and_then(|c| c.as_str()).unwrap_or("USD").to_string();

    let timestamps: Vec<i64> = result
        .get("timestamp")
        .and_then(|t| t.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_i64()).collect())
        .unwrap_or_default();
    let closes: Vec<Option<f64>> = result
        .get("indicators")
        .and_then(|i| i.get("quote"))
        .and_then(|q| q.as_array())
        .and_then(|arr| arr.first())
        .and_then(|q0| q0.get("close"))
        .and_then(|c| c.as_array())
        .map(|arr| arr.iter().map(|v| v.as_f64()).collect())
        .unwrap_or_default();

    let mut candidates: Vec<(chrono::NaiveDate, f64)> = Vec::new();
    for (ts, close) in timestamps.iter().zip(closes.iter()) {
        let Some(close) = close else { continue };
        let dt = Utc.timestamp_opt(*ts, 0).single().ok_or_else(|| PriceError::Message("timestamp invalide".into()))?;
        let date = dt.date_naive();
        if date <= target_date {
            candidates.push((date, close.clone()));
        }
    }

    if candidates.is_empty() {
        return Err(PriceError::Message(format!("Pas de clôture Yahoo disponible pour {ticker} au plus tard le {day_str}")));
    }

    let (_, price) = candidates.into_iter().max_by_key(|(date, _)| *date).unwrap();

    YAHOO_PRICE_CACHE.lock().unwrap().insert(key, (price, currency.clone()));
    Ok((price, currency))
}

fn get_price_from_binance(symbol: &str, time: DateTime<Utc>) -> Result<f64, PriceError> {
    let day_str = time.format("%Y-%m-%d").to_string();

    match binance_klines(&format!("{symbol}EUR"), &day_str) {
        Ok(data) if !data.is_empty() => {
            if let Some(close) = data[0].get(4).and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok()) {
                return Ok(close);
            }
        }
        Err(PriceError::Http(e)) if e.status().map(|s| s.as_u16()) != Some(400) => {
            return Err(PriceError::Message(format!("API Binance KO pour {symbol}EUR")));
        }
        _ => {}
    }

    let usdt_data = binance_klines(&format!("{symbol}USDT"), &day_str);
    let eurusdt_data = binance_klines("EURUSDT", &day_str);
    if let (Ok(usdt_data), Ok(eurusdt_data)) = (usdt_data, eurusdt_data) {
        if !usdt_data.is_empty() && !eurusdt_data.is_empty() {
            let price_usdt: Option<f64> = usdt_data[0].get(4).and_then(|v| v.as_str()).and_then(|s| s.parse().ok());
            let eurusdt_rate: Option<f64> = eurusdt_data[0].get(4).and_then(|v| v.as_str()).and_then(|s| s.parse().ok());
            if let (Some(price_usdt), Some(eurusdt_rate)) = (price_usdt, eurusdt_rate) {
                if eurusdt_rate > 0.0 {
                    return Ok(price_usdt / eurusdt_rate);
                }
            }
        }
    }

    Err(PriceError::Message(format!("Binance n'a pas trouvé le prix pour {symbol} au {day_str}")))
}

pub fn yahoo_daily_closes(ticker: &str, days: i64) -> Result<Vec<(chrono::NaiveDate, f64)>, PriceError> {
    let end = Utc::now();
    let start = end - chrono::Duration::days(days + 5);
    let period1 = start.timestamp();
    let period2 = end.timestamp();

    let url = format!("{YAHOO_CHART_API}/{ticker}");
    let resp = client()
        .get(&url)
        .query(&[
            ("period1", period1.to_string()),
            ("period2", period2.to_string()),
            ("interval", "1d".to_string()),
            ("events", "history".to_string()),
        ])
        .send()?
        .error_for_status()?;

    let data: Value = resp.json()?;
    let result = data
        .get("chart")
        .and_then(|c| c.get("result"))
        .and_then(|r| r.as_array())
        .and_then(|arr| arr.first())
        .ok_or_else(|| PriceError::Message(format!("Yahoo n'a pas trouvé de résultat pour {ticker}")))?;

    let currency = result.get("meta").and_then(|m| m.get("currency")).and_then(|c| c.as_str()).unwrap_or("USD").to_string();
    let (fx_currency, price_factor) = normalize_currency_for_fx(&currency);
    let fx_series: HashMap<chrono::NaiveDate, f64> = if fx_currency != "EUR" {
        yahoo_daily_closes_raw(&format!("EUR{fx_currency}=X"))?.into_iter().collect()
    } else {
        HashMap::new()
    };

    let timestamps: Vec<i64> = result
        .get("timestamp")
        .and_then(|t| t.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_i64()).collect())
        .unwrap_or_default();
    let closes: Vec<Option<f64>> = result
        .get("indicators")
        .and_then(|i| i.get("quote"))
        .and_then(|q| q.as_array())
        .and_then(|arr| arr.first())
        .and_then(|q0| q0.get("close"))
        .and_then(|c| c.as_array())
        .map(|arr| arr.iter().map(|v| v.as_f64()).collect())
        .unwrap_or_default();

    let mut out = Vec::new();
    for (ts, close) in timestamps.iter().zip(closes.iter()) {
        let Some(mut close) = *close else { continue };
        let dt = Utc.timestamp_opt(*ts, 0).single().ok_or_else(|| PriceError::Message("timestamp invalide".into()))?;
        let day = dt.date_naive();

        close *= price_factor;
        if fx_currency != "EUR" {
            let Some(fx_rate) = fx_series.get(&day) else { continue };
            close /= fx_rate;
        }
        out.push((day, close));
    }

    Ok(out)
}

fn yahoo_daily_closes_raw(ticker: &str) -> Result<Vec<(chrono::NaiveDate, f64)>, PriceError> {
    let end = Utc::now();
    let start = end - chrono::Duration::days(95);
    let url = format!("{YAHOO_CHART_API}/{ticker}");
    let resp = client()
        .get(&url)
        .query(&[
            ("period1", start.timestamp().to_string()),
            ("period2", end.timestamp().to_string()),
            ("interval", "1d".to_string()),
            ("events", "history".to_string()),
        ])
        .send()?
        .error_for_status()?;
    let data: Value = resp.json()?;
    let result = data
        .get("chart")
        .and_then(|c| c.get("result"))
        .and_then(|r| r.as_array())
        .and_then(|arr| arr.first())
        .ok_or_else(|| PriceError::Message(format!("Yahoo n'a pas trouvé de résultat pour {ticker}")))?;
    let timestamps: Vec<i64> = result
        .get("timestamp")
        .and_then(|t| t.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_i64()).collect())
        .unwrap_or_default();
    let closes: Vec<Option<f64>> = result
        .get("indicators")
        .and_then(|i| i.get("quote"))
        .and_then(|q| q.as_array())
        .and_then(|arr| arr.first())
        .and_then(|q0| q0.get("close"))
        .and_then(|c| c.as_array())
        .map(|arr| arr.iter().map(|v| v.as_f64()).collect())
        .unwrap_or_default();
    let mut out = Vec::new();
    for (ts, close) in timestamps.iter().zip(closes.iter()) {
        let Some(close) = *close else { continue };
        let dt = Utc.timestamp_opt(*ts, 0).single().ok_or_else(|| PriceError::Message("timestamp invalide".into()))?;
        out.push((dt.date_naive(), close));
    }
    Ok(out)
}

pub fn binance_daily_closes(symbol: &str, days: i64) -> Result<Vec<(chrono::NaiveDate, f64)>, PriceError> {
    let end_ms = Utc::now().timestamp_millis();
    let start_ms = (Utc::now() - chrono::Duration::days(days + 2)).timestamp_millis();

    let resp = client()
        .get(BINANCE_API)
        .query(&[
            ("symbol", format!("{symbol}EUR")),
            ("interval", "1d".to_string()),
            ("startTime", start_ms.to_string()),
            ("endTime", end_ms.to_string()),
            ("limit", "1000".to_string()),
        ])
        .send()?
        .error_for_status()?;

    let data: Vec<Value> = resp.json()?;
    let mut out = Vec::new();
    for kline in data {
        let Some(open_time_ms) = kline.get(0).and_then(|v| v.as_i64()) else { continue };
        let Some(close) = kline.get(4).and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok()) else { continue };
        let dt = Utc.timestamp_millis_opt(open_time_ms).single().ok_or_else(|| PriceError::Message("timestamp invalide".into()))?;
        out.push((dt.date_naive(), close));
    }
    Ok(out)
}

pub fn normalize_currency_for_fx(currency: &str) -> (String, f64) {
    if currency == "GBp" || currency == "GBX" {
        ("GBP".to_string(), 0.01)
    } else {
        (currency.to_string(), 1.0)
    }
}

// --- NOUVELLE FONCTION HISTORICAL_PRICE_EUR AVEC CACHE 1H ---
pub fn historical_price_eur(symbol: &str, time: DateTime<Utc>, kind: AssetKind, ticker: Option<&str>) -> f64 {
    let symbol = symbol.to_uppercase();

    if kind == AssetKind::Cash {
        match symbol.as_str() {
            "EUR" | "EURI" => return 1.0,
            "USD" | "USDT" | "USDC" | "BUSD" => return 0.92,
            "GBP" => return 1.15,
            _ => {}
        }
    }

    // Aligne sur l'heure pile (1h)
    let aligned_ts = time.timestamp() - (time.timestamp() % 3600);

    // 1. Vérifier le cache
    {
        let cache = PRICE_CACHE.lock().unwrap();
        if let Some(price) = cache.get_closest(&symbol, aligned_ts) {
            return price;
        }
    }

    // 2. Si absent, fetch et insertion
    let price = match kind {
        AssetKind::Stock => {
            let Some(ticker) = ticker else { return 0.0; };
            match fetch_yahoo_1h(ticker, aligned_ts) {
                Ok(p) => p,
                Err(_) => 0.0
            }
        }
        AssetKind::Crypto => {
            match fetch_binance_1h(&symbol, aligned_ts) {
                Ok(p) => p,
                Err(_) => 0.0
            }
        }
        _ => 0.0
    };

    if price > 0.0 {
        PRICE_CACHE.lock().unwrap().insert(&symbol, aligned_ts, price);
    }

    price
}

fn fetch_binance_1h(symbol: &str, aligned_ts: i64) -> Result<f64, PriceError> {
    let start_ms = aligned_ts * 1000;
    let resp = client()
        .get(BINANCE_API)
        .query(&[
            ("symbol", format!("{symbol}EUR")),
            ("interval", "1h".to_string()),
            ("startTime", start_ms.to_string()),
            ("limit", "1".to_string()),
        ])
        .send()?
        .error_for_status()?;

    let data: Vec<Value> = resp.json()?;
    if let Some(kline) = data.first() {
        if let Some(close) = kline.get(4).and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok()) {
            return Ok(close);
        }
    }
    
    // Fallback USDT
    let resp_usdt = client()
        .get(BINANCE_API)
        .query(&[
            ("symbol", format!("{symbol}USDT")),
            ("interval", "1h".to_string()),
            ("startTime", start_ms.to_string()),
            ("limit", "1".to_string()),
        ])
        .send()?;

    let data_usdt: Vec<Value> = resp_usdt.json()?;
    if let Some(kline) = data_usdt.first() {
        if let Some(close_usdt) = kline.get(4).and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok()) {
            let resp_eur = client().get(BINANCE_API)
                .query(&[("symbol", "EURUSDT".to_string()), ("interval", "1h".to_string()), ("startTime", start_ms.to_string()), ("limit", "1".to_string())]).send()?;
            let data_eur: Vec<Value> = resp_eur.json()?;
            if let Some(kline_eur) = data_eur.first() {
                if let Some(rate_eur) = kline_eur.get(4).and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok()) {
                    if rate_eur > 0.0 { return Ok(close_usdt / rate_eur); }
                }
            }
        }
    }
    Err(PriceError::Message("Prix Binance 1h introuvable".into()))
}

fn fetch_yahoo_1h(ticker: &str, aligned_ts: i64) -> Result<f64, PriceError> {
    let target = Utc.timestamp_opt(aligned_ts, 0).unwrap();
    let period1 = (target - chrono::Duration::days(2)).timestamp();
    let period2 = (target + chrono::Duration::days(1)).timestamp();
    
    let url = format!("{YAHOO_CHART_API}/{ticker}");
    let resp = client()
        .get(&url)
        .query(&[
            ("period1", period1.to_string()),
            ("period2", period2.to_string()),
            ("interval", "1h".to_string()),
            ("events", "history".to_string()),
        ])
        .send()?
        .error_for_status()?;

    let data: Value = resp.json()?;
    let result = data.get("chart").and_then(|c| c.get("result")).and_then(|r| r.as_array()).and_then(|arr| arr.first())
        .ok_or_else(|| PriceError::Message("Yahoo KO".into()))?;

    let currency = result.get("meta").and_then(|m| m.get("currency")).and_then(|c| c.as_str()).unwrap_or("USD");
    let (fx_currency, price_factor) = normalize_currency_for_fx(currency);
    let fx_rate = if fx_currency != "EUR" {
        yahoo_historical_price(&format!("EUR{fx_currency}=X"), &target.format("%Y-%m-%d").to_string()).ok().map(|(p, _)| p).unwrap_or(1.0)
    } else { 1.0 };

    let timestamps: Vec<i64> = result.get("timestamp").and_then(|t| t.as_array()).map(|arr| arr.iter().filter_map(|v| v.as_i64()).collect()).unwrap_or_default();
    let closes: Vec<Option<f64>> = result.get("indicators").and_then(|i| i.get("quote")).and_then(|q| q.as_array()).and_then(|arr| arr.first()).and_then(|q0| q0.get("close")).and_then(|c| c.as_array()).map(|arr| arr.iter().map(|v| v.as_f64()).collect()).unwrap_or_default();

    // Cherche la bougie 1h exacte
    for (ts, close) in timestamps.iter().zip(closes.iter()) {
        if *ts == aligned_ts {
            if let Some(c) = *close {
                return Ok((c * price_factor) / fx_rate);
            }
        }
    }

    // Si marché fermé, prend la dernière bougie disponible avant aligned_ts
    let mut best_price = None;
    let mut best_ts = 0;
    for (ts, close) in timestamps.iter().zip(closes.iter()) {
        if *ts <= aligned_ts && *ts > best_ts {
            if let Some(c) = *close {
                best_price = Some(c);
                best_ts = *ts;
            }
        }
    }

    if let Some(c) = best_price {
        return Ok((c * price_factor) / fx_rate);
    }

    Err(PriceError::Message("Prix Yahoo 1h introuvable".into()))
}