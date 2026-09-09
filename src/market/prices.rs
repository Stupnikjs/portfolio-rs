//! Portage de src/market/prices.py. Utilise l'API publique de Binance
//! pour la crypto, et Yahoo Finance pour les actions.

use std::collections::HashMap;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc, Timelike};
use once_cell::sync::Lazy;
use serde_json::Value;
use thiserror::Error;
use chrono::Duration as ChronoDuration;
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

// --- caches best-effort ---
static BINANCE_KLINES_CACHE: Lazy<Mutex<HashMap<(String, String), Vec<Value>>>> = Lazy::new(|| Mutex::new(HashMap::new()));
static YAHOO_PRICE_CACHE: Lazy<Mutex<HashMap<(String, String), (f64, String)>>> = Lazy::new(|| Mutex::new(HashMap::new()));
static BINANCE_KLINES_H4_CACHE: Lazy<Mutex<HashMap<(String, String), Vec<Value>>>> = Lazy::new(|| Mutex::new(HashMap::new()));
static YAHOO_H4_CACHE: Lazy<Mutex<HashMap<(String, String), (f64, String)>>> = Lazy::new(|| Mutex::new(HashMap::new()));

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

fn binance_klines_h4(symbol_pair: &str, hour_bucket_str: &str) -> Result<Vec<Value>, PriceError> {
    let key = (symbol_pair.to_string(), hour_bucket_str.to_string());
    if let Some(cached) = BINANCE_KLINES_H4_CACHE.lock().unwrap().get(&key) {
        return Ok(cached.clone());
    }

    thread::sleep(Duration::from_millis(100));

    let dt = chrono::NaiveDateTime::parse_from_str(hour_bucket_str, "%Y-%m-%d %H:%M:%S")
        .map_err(|e| PriceError::Message(e.to_string()))?;
    let start_ms = Utc.from_utc_datetime(&dt).timestamp_millis();

    let resp = client()
        .get(BINANCE_API)
        .query(&[
            ("symbol", symbol_pair),
            ("interval", "4h"),
            ("startTime", &start_ms.to_string()),
            ("limit", "1"),
        ])
        .send()?
        .error_for_status()?;

    let data: Vec<Value> = resp.json()?;
    BINANCE_KLINES_H4_CACHE.lock().unwrap().insert(key, data.clone());
    Ok(data)
}

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

    let mut candidates: Vec<(DateTime<Utc>, f64)> = Vec::new();
    for (ts, close) in timestamps.iter().zip(closes.iter()) {
        let Some(close) = close else { continue };
        let dt = Utc.timestamp_opt(*ts, 0).single().ok_or_else(|| PriceError::Message("timestamp invalide".into()))?;
        if dt <= target {
            candidates.push((dt, *close));
        }
    }

    if candidates.is_empty() {
        return Err(PriceError::Message(format!("Pas de clôture Yahoo disponible pour {ticker} au plus tard le {day_str}")));
    }

    let (_, price) = candidates.into_iter().max_by_key(|(dt, _)| *dt).unwrap();

    YAHOO_PRICE_CACHE.lock().unwrap().insert(key, (price, currency.clone()));
    Ok((price, currency))
}

pub fn yahoo_historical_price_h4(ticker: &str, hour_bucket_str: &str) -> Result<(f64, String), PriceError> {
    let key = (ticker.to_string(), hour_bucket_str.to_string());
    if let Some(cached) = YAHOO_H4_CACHE.lock().unwrap().get(&key) {
        return Ok(cached.clone());
    }

    let target = chrono::NaiveDateTime::parse_from_str(hour_bucket_str, "%Y-%m-%d %H:%M:%S")
        .map_err(|e| PriceError::Message(e.to_string()))?;
    let target_dt = Utc.from_utc_datetime(&target);

    // Fenêtre de 8h en arrière pour couvrir la borne H4 précédente même si weekend/férié
    let period1 = (target_dt - chrono::Duration::hours(8)).timestamp();
    let period2 = (target_dt + chrono::Duration::hours(4)).timestamp();

    let url = format!("{YAHOO_CHART_API}/{ticker}");
    let resp = client()
        .get(&url)
        .query(&[
            ("period1", period1.to_string()),
            ("period2", period2.to_string()),
            ("interval", "1h".to_string()), // Yahoo ne supporte pas 4h nativement partout, on prend 1h
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
        .ok_or_else(|| PriceError::Message(format!("Yahoo H4 n'a pas trouvé de résultat pour {ticker}")))?;

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

    let mut candidates: Vec<(DateTime<Utc>, f64)> = Vec::new();
    for (ts, close) in timestamps.iter().zip(closes.iter()) {
        let Some(close) = close else { continue };
        let dt = Utc.timestamp_opt(*ts, 0).single().ok_or_else(|| PriceError::Message("timestamp invalide".into()))?;
        if dt <= target_dt {
            candidates.push((dt, *close));
        }
    }

    if candidates.is_empty() {
        return Err(PriceError::Message(format!("Pas de clôture H4 Yahoo disponible pour {ticker} au {hour_bucket_str}")));
    }

    let (_, price) = candidates.into_iter().max_by_key(|(dt, _)| *dt).unwrap();

    YAHOO_H4_CACHE.lock().unwrap().insert(key, (price, currency.clone()));
    Ok((price, currency))
}

fn get_price_from_binance_daily(symbol: &str, time: DateTime<Utc>) -> Result<f64, PriceError> {
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

pub fn normalize_currency_for_fx(currency: &str) -> (String, f64) {
    if currency == "GBp" || currency == "GBX" {
        ("GBP".to_string(), 0.01)
    } else {
        (currency.to_string(), 1.0)
    }
}

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

    let day_str = time.format("%Y-%m-%d").to_string();
    
    // Arrondi à la borne inférieure H4 (00, 04, 08, 12, 16, 20)
    let h4_hour = (time.hour() / 4) * 4;
    let h4_time = time.with_hour(h4_hour).unwrap().with_minute(0).unwrap().with_second(0).unwrap();
    let h4_str = h4_time.format("%Y-%m-%d %H:%M:%S").to_string();

    match kind {
        AssetKind::Stock => {
            let Some(ticker) = ticker else {
                eprintln!("  [WARN] Pas de ticker Yahoo pour l'action {symbol}. Prix mis à 0.");
                return 0.0;
            };
            
            // 1. Tentative H4
            if let Ok((mut price, currency)) = yahoo_historical_price_h4(ticker, &h4_str) {
                if currency != "EUR" {
                    let (fx_currency, price_factor) = normalize_currency_for_fx(&currency);
                    price *= price_factor;
                    let fx_pair = format!("EUR{fx_currency}=X");
                    if let Ok((fx_price, _)) = yahoo_historical_price_h4(&fx_pair, &h4_str) {
                        return price / fx_price;
                    }
                } else {
                    return price;
                }
            }

            // 2. Fallback Daily (si > 2 ans ou erreur H4)
            match yahoo_historical_price(ticker, &day_str) {
                Ok((mut price, currency)) => {
                    if currency != "EUR" {
                        let (fx_currency, price_factor) = normalize_currency_for_fx(&currency);
                        price *= price_factor;
                        let fx_pair = format!("EUR{fx_currency}=X");
                        match yahoo_historical_price(&fx_pair, &day_str) {
                            Ok((fx_price, _)) => price / fx_price,
                            Err(_) => {
                                eprintln!("  [WARN] Prix Yahoo indisponible pour {ticker} au {day_str}.");
                                0.0
                            }
                        }
                    } else {
                        price
                    }
                }
                Err(_) => {
                    eprintln!("  [WARN] Prix Yahoo indisponible pour {ticker} au {day_str}.");
                    0.0
                }
            }
        }
        AssetKind::Crypto => {
            // 1. Tentative H4 directe EUR
            let h4_direct = (|| -> Result<f64, PriceError> {
                let data = binance_klines_h4(&format!("{symbol}EUR"), &h4_str)?;
                if !data.is_empty() {
                    if let Some(close) = data[0].get(4).and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok()) {
                        return Ok(close);
                    }
                }
                Err(PriceError::Message("H4 direct EUR failed".to_string()))
            })();
            if let Ok(price) = h4_direct { return price; }

            // 2. Tentative H4 fallback USDT
            let h4_usdt = (|| -> Result<f64, PriceError> {
                let usdt_data = binance_klines_h4(&format!("{symbol}USDT"), &h4_str)?;
                let eurusdt_data = binance_klines_h4("EURUSDT", &h4_str)?;
                if !usdt_data.is_empty() && !eurusdt_data.is_empty() {
                    let price_usdt: Option<f64> = usdt_data[0].get(4).and_then(|v| v.as_str()).and_then(|s| s.parse().ok());
                    let eurusdt_rate: Option<f64> = eurusdt_data[0].get(4).and_then(|v| v.as_str()).and_then(|s| s.parse().ok());
                    if let (Some(price_usdt), Some(eurusdt_rate)) = (price_usdt, eurusdt_rate) {
                        if eurusdt_rate > 0.0 {
                            return Ok(price_usdt / eurusdt_rate);
                        }
                    }
                }
                Err(PriceError::Message("H4 USDT fallback failed".to_string()))
            })();
            if let Ok(price) = h4_usdt { return price; }

            // 3. Fallback Daily
            match get_price_from_binance_daily(&symbol, time) {
                Ok(price) => price,
                Err(_) => {
                    eprintln!("  [WARN] Prix Binance indisponible pour {symbol} au {day_str}. Prix mis à 0.");
                    0.0
                }
            }
        }
        AssetKind::Cash => 0.0,
    }
}

// Récupère 90 jours de prix de clôture pour un actif
pub fn fetch_daily_closes(symbol: &str, kind: AssetKind, ticker: Option<&str>, days: i64) -> Vec<f64> {
    let mut closes = Vec::new();
    let now = Utc::now();

    // On récupère les prix en remontant de jour en jour
    for i in 0..days {
        let time = now - ChronoDuration::days(i);
        let price = historical_price_eur(symbol, time, kind, ticker);
        if price > 0.0 {
            closes.push(price);
        }
    }
    closes.reverse(); // Chronologique
    closes
}