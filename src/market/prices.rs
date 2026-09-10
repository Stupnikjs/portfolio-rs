//! Portage de src/market/prices.py. Utilise l'API publique de Binance
//! pour la crypto, et Yahoo Finance pour les actions.

use std::collections::HashMap;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use once_cell::sync::Lazy;
use serde_json::Value;
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

// --- caches best-effort, équivalents des @lru_cache Python ---
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
///
/// Important : day_str peut tomber un week-end ou un jour férié. On
/// demande une fenêtre de plusieurs jours et on prend la dernière bougie
/// disponible avant ou à la date cible.
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
    // Ne surtout pas faire .to_uppercase() : GBp != GBP.
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

/// Récupère la série de clôtures quotidiennes Yahoo sur les `days` derniers
/// jours (bornes incluses). Contrairement à `yahoo_historical_price`, on
/// veut ici tout l'historique pour calculer des rendements, pas une seule
/// valeur -- une seule requête HTTP couvre toute la fenêtre.
pub fn yahoo_daily_closes(ticker: &str, days: i64) -> Result<Vec<(chrono::NaiveDate, f64)>, PriceError> {
    let end = Utc::now();
    let start = end - chrono::Duration::days(days + 5); // marge pour jours fériés/week-ends
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
            let Some(fx_rate) = fx_series.get(&day) else { continue }; // pas de FX ce jour-là -> on saute le point plutôt que fausser
            close /= fx_rate;
        }
        out.push((day, close));
    }

    Ok(out)
}

/// Variante brute sans conversion EUR, utilisée en interne pour récupérer
/// la série FX elle-même (éviter une récursion infinie).
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

/// Série de clôtures quotidiennes EUR pour une crypto sur les `days`
/// derniers jours. Seule la paire directe SYMBOLEUR est utilisée -- la
/// reconstruction par cross-rate (SYMBOLUSDT / EURUSDT) jour par jour
/// n'est pas gérée ici pour la corrélation (elle l'est déjà pour le prix
/// ponctuel via `get_price_from_binance`) : si la paire EUR n'existe pas
/// sur Binance, la crypto est simplement absente de la matrice.
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


/// multiplicatif à appliquer au prix). Nécessaire car Yahoo exprime
/// certains titres londoniens en pence ("GBp") plutôt qu'en livres
/// ("GBP") -- il n'existe pas de paire "EURGBp=X".
pub fn normalize_currency_for_fx(currency: &str) -> (String, f64) {
    if currency == "GBp" || currency == "GBX" {
        ("GBP".to_string(), 0.01)
    } else {
        (currency.to_string(), 1.0)
    }
}

/// Retourne le prix en EUR de `symbol` à la date `time` en fonction de son
/// `kind`. Best-effort : erreurs réseau/format renvoient 0.0 avec un
/// warning plutôt que de propager -- un prix manquant ne doit jamais
/// bloquer le pipeline.
pub fn historical_price_eur(symbol: &str, time: DateTime<Utc>, kind: AssetKind, ticker: Option<&str>) -> f64 {
    let symbol = symbol.to_uppercase();

    if kind == AssetKind::Cash {
        match symbol.as_str() {
            "EUR" | "EURI" => return 1.0,
            "USD" | "USDT" | "USDC" | "BUSD" => return 0.92, // fixe, à remplacer par un vrai appel API si besoin
            "GBP" => return 1.15,
            _ => {}
        }
    }

    let day_str = time.format("%Y-%m-%d").to_string();

    match kind {
        AssetKind::Stock => {
            let Some(ticker) = ticker else {
                eprintln!("  [WARN] Pas de ticker Yahoo pour l'action {symbol}. Prix mis à 0.");
                return 0.0;
            };
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
        AssetKind::Crypto => match get_price_from_binance(&symbol, time) {
            Ok(price) => price,
            Err(_) => {
                eprintln!("  [WARN] Prix Binance indisponible pour {symbol} au {day_str}. Prix mis à 0.");
                0.0
            }
        },
        AssetKind::Cash => {
            eprintln!("  [WARN] Type d'actif non géré pour {symbol}: {kind:?}");
            0.0
        }
    }
}
