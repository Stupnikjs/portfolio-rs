//! Portage de src/market/prices.py. Utilise l'API publique de Binance
//! pour la crypto, et Yahoo Finance pour les actions.

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    Hour,
    Day,
    Week,
}

impl Resolution {
    fn seconds(self) -> i64 {
        match self {
            Resolution::Hour => 3_600,
            Resolution::Day => 86_400,
            Resolution::Week => 604_800,
        }
    }

    pub fn align(self, ts: i64) -> i64 {
        ts - ts.rem_euclid(self.seconds())
    }

    fn label(self) -> &'static str {
        match self {
            Resolution::Hour => "1h",
            Resolution::Day => "1d",
            Resolution::Week => "1w",
        }
    }
}

// --- Cache brut : identique quelle que soit la résolution ---
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PriceCache {
    data: HashMap<String, BTreeMap<i64, f64>>,
}

impl PriceCache {
    fn load(path: &Path) -> Self {
        std::fs::read(path).ok()
            .and_then(|b| bincode::deserialize(&b).ok())
            .unwrap_or_default()
    }

    fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(p) = path.parent() { std::fs::create_dir_all(p)?; }
        std::fs::write(path, bincode::serialize(self).expect("bincode serialize"))
    }

    fn get_closest(&self, symbol: &str, target_ts: i64) -> Option<f64> {
        self.data.get(&symbol.to_uppercase())?.range(..=target_ts).next_back().map(|(_, &v)| v)
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

// --- Une instance par résolution : aligne les timestamps automatiquement ---
pub struct ResolutionCache {
    resolution: Resolution,
    cache: Mutex<PriceCache>,
}

impl ResolutionCache {
    fn empty(resolution: Resolution) -> Self {
        Self { resolution, cache: Mutex::new(PriceCache::default()) }
    }

    pub fn init(&self, path: &Path) {
        *self.cache.lock().unwrap() = PriceCache::load(path);
    }

    pub fn save(&self, path: &Path) {
        if let Err(e) = self.cache.lock().unwrap().save(path) {
            eprintln!("Erreur sauvegarde cache {}: {e}", self.resolution.label());
        }
    }

    pub fn get_closest(&self, symbol: &str, ts: i64) -> Option<f64> {
        self.cache.lock().unwrap().get_closest(symbol, self.resolution.align(ts))
    }

    /// Dernier point connu en cache (pour ne fetcher que le manquant).
    pub fn latest(&self, symbol: &str) -> Option<i64> {
        self.cache.lock().unwrap().latest_ts(symbol)
    }

    /// Points connus depuis `days` jours, sous forme de dates (utile pour
    /// le cache 1d : reconstruit une série jour -> prix depuis le cache).
    pub fn range_days(&self, symbol: &str, days: i64) -> Vec<(chrono::NaiveDate, f64)> {
        let cutoff = self.resolution.align((Utc::now() - chrono::Duration::days(days)).timestamp());
        self.cache.lock().unwrap()
            .range_since(symbol, cutoff)
            .into_iter()
            .map(|(ts, price)| (Utc.timestamp_opt(ts, 0).unwrap().date_naive(), price))
            .collect()
    }

    pub fn insert(&self, symbol: &str, ts: i64, price: f64) {
        self.cache.lock().unwrap().insert(symbol, self.resolution.align(ts), price);
    }

    /// Insertion par date (jour), pour le cache 1d.
    pub fn insert_date(&self, symbol: &str, date: chrono::NaiveDate, price: f64) {
        let ts = Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).unwrap()).timestamp();
        self.insert(symbol, ts, price);
    }
}

static PRICE_CACHE_1H: Lazy<ResolutionCache> = Lazy::new(|| ResolutionCache::empty(Resolution::Hour));
static PRICE_CACHE_1D: Lazy<ResolutionCache> = Lazy::new(|| ResolutionCache::empty(Resolution::Day));

pub fn init_price_caches(dir: &Path) {
    PRICE_CACHE_1H.init(&dir.join("price_cache_1h.bin"));
    PRICE_CACHE_1D.init(&dir.join("price_cache_1d.bin"));
}

pub fn save_price_caches(dir: &Path) {
    PRICE_CACHE_1H.save(&dir.join("price_cache_1h.bin"));
    PRICE_CACHE_1D.save(&dir.join("price_cache_1d.bin"));
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

// --- cache best-effort en mémoire pour les lookups ponctuels (import XTB) ---
static YAHOO_PRICE_CACHE: Lazy<Mutex<HashMap<(String, String), (f64, String)>>> = Lazy::new(|| Mutex::new(HashMap::new()));

const FX_TICKERS: &[(&str, &str)] = &[
    ("USD", "EURUSD=X"),
    ("GBP", "EURGBP=X"),
];

const CRYPTO_QUOTES: &[&str] = &["USDC", "EUR", "USDT"];

fn fetch_binance_pair_close(pair: &str, start_ms: i64) -> Result<Option<f64>, PriceError> {
    let resp = client()
        .get(BINANCE_API)
        .query(&[
            ("symbol", pair),
            ("interval", "1h"),
            ("startTime", &start_ms.to_string()),
            ("limit", "1"),
        ])
        .send()?;

    match resp.status().as_u16() {
        200..=299 => {
            let data: Vec<Value> = resp.json()?;
            Ok(data.first()
                .and_then(|k| k.get(4))
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<f64>().ok()))
        }
        400 => {
            eprintln!("  [DEBUG binance] {pair}: paire inexistante (400)");
            Ok(None)
        }
        status => {
            eprintln!("  [DEBUG binance] {pair}: HTTP {status}");
            Ok(None)
        }
    }
}

/// Taux de change EUR -> `currency`, aligné à l'heure, cache 1h persistant.
pub fn eur_fx_rate_1h(currency: &str, aligned_ts: i64) -> f64 {
    if currency == "EUR" { return 1.0; }

    if let Some(rate) = PRICE_CACHE_1H.get_closest(currency, aligned_ts) {
        return rate;
    }

    let Some((_, ticker)) = FX_TICKERS.iter().find(|(c, _)| *c == currency) else {
        return fallback_fx_rate(currency);
    };

    if let Ok((rate, _)) = fetch_yahoo_fx_1h(ticker, aligned_ts) {
        if rate > 0.0 {
            PRICE_CACHE_1H.insert(currency, aligned_ts, rate);
            return rate;
        }
    }

    let day_str = Utc.timestamp_opt(aligned_ts, 0).unwrap().format("%Y-%m-%d").to_string();
    if let Ok((rate, _)) = yahoo_historical_price(ticker, &day_str) {
        if rate > 0.0 {
            PRICE_CACHE_1H.insert(currency, aligned_ts, rate);
            return rate;
        }
    }

    eprintln!("  [WARN fx] échec {currency} ts={aligned_ts}, fallback statique");
    fallback_fx_rate(currency)
}

/// Dernier filet de sécurité si Yahoo est injoignable.
fn fallback_fx_rate(currency: &str) -> f64 {
    match currency {
        "USD" => 1.08,
        "GBP" => 0.87,
        _ => 1.0,
    }
}

/// Récupère la dernière clôture Yahoo disponible <= day_str (import XTB).
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

pub fn yahoo_daily_closes(ticker: &str, days: i64) -> Result<Vec<(chrono::NaiveDate, f64)>, PriceError> {
    let cache_key = format!("YHO:{ticker}");
    let yesterday = Utc::now().date_naive() - chrono::Duration::days(1);
    let yesterday_ts = Resolution::Day.align(Utc.from_utc_datetime(&yesterday.and_hms_opt(0, 0, 0).unwrap()).timestamp());

    if let Some(latest) = PRICE_CACHE_1D.latest(&cache_key) {
        if latest >= yesterday_ts {
            return Ok(PRICE_CACHE_1D.range_days(&cache_key, days));
        }
    }

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

    for (ts, close) in timestamps.iter().zip(closes.iter()) {
        let Some(mut close) = *close else { continue };
        let dt = Utc.timestamp_opt(*ts, 0).single().ok_or_else(|| PriceError::Message("timestamp invalide".into()))?;
        let day = dt.date_naive();

        close *= price_factor;
        if fx_currency != "EUR" {
            let Some(fx_rate) = fx_series.get(&day) else { continue };
            close /= fx_rate;
        }
        PRICE_CACHE_1D.insert_date(&cache_key, day, close);
    }

    Ok(PRICE_CACHE_1D.range_days(&cache_key, days))
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
    let cache_key = format!("BIN:{symbol}");
    let yesterday = Utc::now().date_naive() - chrono::Duration::days(1);
    let yesterday_ts = Resolution::Day.align(Utc.from_utc_datetime(&yesterday.and_hms_opt(0, 0, 0).unwrap()).timestamp());

    if let Some(latest) = PRICE_CACHE_1D.latest(&cache_key) {
        if latest >= yesterday_ts {
            return Ok(PRICE_CACHE_1D.range_days(&cache_key, days));
        }
    }

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
    for kline in &data {
        let Some(open_time_ms) = kline.get(0).and_then(|v| v.as_i64()) else { continue };
        let Some(close) = kline.get(4).and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok()) else { continue };
        let dt = Utc.timestamp_millis_opt(open_time_ms).single().ok_or_else(|| PriceError::Message("timestamp invalide".into()))?;
        PRICE_CACHE_1D.insert_date(&cache_key, dt.date_naive(), close);
    }

    Ok(PRICE_CACHE_1D.range_days(&cache_key, days))
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
    let aligned_ts = Resolution::Hour.align(time.timestamp());

    if kind == AssetKind::Cash {
        return match symbol.as_str() {
            "EUR" | "EURI" => 1.0,
            "USD" | "USDT" | "USDC" | "BUSD" => 1.0 / eur_fx_rate_1h("USD", aligned_ts),
            "GBP" => 1.0 / eur_fx_rate_1h("GBP", aligned_ts),
            _ => 0.0,
        };
    }

    if let Some(price) = PRICE_CACHE_1H.get_closest(&symbol, aligned_ts) {
        return price;
    }

    let price = match kind {
        AssetKind::Stock => {
            let Some(ticker) = ticker else { return 0.0; };

            if let Ok((raw_price, currency)) = fetch_yahoo_fx_1h(ticker, aligned_ts) {
                let (fx_currency, price_factor) = normalize_currency_for_fx(&currency);
                let price = raw_price * price_factor;
                let eur_price = if fx_currency != "EUR" {
                    price / eur_fx_rate_1h(&fx_currency, aligned_ts)
                } else { price };
                if eur_price > 0.0 { return eur_price; }
            }

            let day_str = Utc.timestamp_opt(aligned_ts, 0).unwrap().format("%Y-%m-%d").to_string();
            if let Ok((raw_price, currency)) = yahoo_historical_price(ticker, &day_str) {
                let (fx_currency, price_factor) = normalize_currency_for_fx(&currency);
                let price = raw_price * price_factor;
                return if fx_currency != "EUR" {
                    price / eur_fx_rate_1h(&fx_currency, aligned_ts)
                } else { price };
            }
            0.0
        }
        AssetKind::Crypto => {
            match fetch_binance_1h(&symbol, aligned_ts) {
                Ok(p) => p,
                Err(e) => { eprintln!("  [DEBUG binance] {symbol}: {e}"); 0.0 }
            }
        }
        _ => 0.0,
    };

    if price > 0.0 {
        PRICE_CACHE_1H.insert(&symbol, aligned_ts, price);
    }

    price
}

fn fetch_binance_1h(symbol: &str, aligned_ts: i64) -> Result<f64, PriceError> {
    let start_ms = aligned_ts * 1000;

    // 1. Récupérer le prix de la crypto en USDC
    let quote = "USDC";
    let pair = format!("{symbol}{quote}");
    
    if let Some(close) = fetch_binance_pair_close(&pair, start_ms)? {
        // 2. Récupérer le taux de change pour convertir l'USDC en EUR (ex: EURUSDC)
        let fx_pair = format!("EUR{quote}");
        match fetch_binance_pair_close(&fx_pair, start_ms)? {
            Some(rate) if rate > 0.0 => {
                // 3. Effectuer la division pour obtenir le prix en EUR
                return Ok(close / rate);
            }
            _ => {
                eprintln!("  [DEBUG binance] {symbol}: prix {quote} trouvé mais pas de taux {fx_pair}");
            }
        }
    }

    Err(PriceError::Message(format!(
        "{symbol}: aucune paire valide sur Binance (USDC avec taux EUR)",
    )))
}

/// Récupère la bougie 1h exacte (ou la dernière disponible avant) pour une
/// paire FX/action Yahoo (ex: "EURUSD=X", "AAPL"). Pas de normalisation de
/// devise ici : c'est à l'appelant de gérer via normalize_currency_for_fx.
fn fetch_yahoo_fx_1h(pair_ticker: &str, aligned_ts: i64) -> Result<(f64, String), PriceError> {
    let target = Utc.timestamp_opt(aligned_ts, 0).unwrap();
    let period1 = (target - chrono::Duration::days(2)).timestamp();
    let period2 = (target + chrono::Duration::days(1)).timestamp();

    let url = format!("{YAHOO_CHART_API}/{pair_ticker}");
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
    let result = data
        .get("chart").and_then(|c| c.get("result")).and_then(|r| r.as_array()).and_then(|arr| arr.first())
        .ok_or_else(|| PriceError::Message(format!("Yahoo KO pour {pair_ticker}")))?;

    let currency = result.get("meta").and_then(|m| m.get("currency")).and_then(|c| c.as_str()).unwrap_or("USD").to_string();

    let timestamps: Vec<i64> = result.get("timestamp").and_then(|t| t.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_i64()).collect()).unwrap_or_default();
    let closes: Vec<Option<f64>> = result.get("indicators").and_then(|i| i.get("quote"))
        .and_then(|q| q.as_array()).and_then(|arr| arr.first()).and_then(|q0| q0.get("close"))
        .and_then(|c| c.as_array()).map(|arr| arr.iter().map(|v| v.as_f64()).collect()).unwrap_or_default();

    for (ts, close) in timestamps.iter().zip(closes.iter()) {
        if *ts == aligned_ts {
            if let Some(c) = *close { return Ok((c, currency)); }
        }
    }
    let mut best: Option<(i64, f64)> = None;
    for (ts, close) in timestamps.iter().zip(closes.iter()) {
        if *ts <= aligned_ts {
            if let Some(c) = *close {
                if best.map_or(true, |(bts, _)| *ts > bts) {
                    best = Some((*ts, c));
                }
            }
        }
    }
    best.map(|(_, c)| (c, currency)).ok_or_else(|| PriceError::Message(format!("Pas de taux FX pour {pair_ticker}")))
}