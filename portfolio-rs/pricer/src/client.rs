//! src/market/client.rs — Service central pour les données de marché.
//!
//! Suppression de l'état global (Lazy<Mutex>) au profit d'une struct injectable.
//! Gère le cache des prix, les taux de change, et la résolution des tickers.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;
use thiserror::Error;

use crate::MarketKind;

use super::cache::{is_live_bucket, Resolution, ResolutionCache};

const BINANCE_API: &str = "https://data-api.binance.vision/api/v3/klines";
const YAHOO_CHART_API: &str = "https://query1.finance.yahoo.com/v8/finance/chart";
const YAHOO_SEARCH_API: &str = "https://query2.finance.yahoo.com/v1/finance/search";
const BINANCE_EXCHANGE_INFO: &str = "https://data-api.binance.vision/api/v3/exchangeInfo";

const FX_CACHE_TOLERANCE_SECS: i64 = 3 * 3_600;
const ACCEPTED_QUOTE_TYPES: &[&str] = &["EQUITY", "ETF"];

#[derive(Debug, Error)]
pub enum PriceError {
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
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

struct YahooSeries {
    points: Vec<(i64, f64)>,
    currency: String,
}

/// Service central pour les prix, taux de change et résolution de tickers.
/// Remplace les variables globales (Lazy<Mutex>) des anciens modules.
pub struct MarketDataClient {
    http_client: reqwest::blocking::Client,
    cache_1h: ResolutionCache,
    cache_1d: ResolutionCache,
    ticker_cache: Mutex<HashMap<String, Option<String>>>,
    binance_assets_cache: Mutex<Option<HashSet<String>>>,
}

impl Default for MarketDataClient {
    fn default() -> Self {
        Self::new()
    }
}

impl MarketDataClient {
    pub fn new() -> Self {
        Self {
            http_client: reqwest::blocking::Client::builder()
                .user_agent("Mozilla/5.0")
                .timeout(Duration::from_secs(10))
                .build()
                .expect("client HTTP"),
            cache_1h: ResolutionCache::empty(Resolution::Hour),
            cache_1d: ResolutionCache::empty(Resolution::Day),
            ticker_cache: Mutex::new(HashMap::new()),
            binance_assets_cache: Mutex::new(None),
        }
    }

    /// Charge les caches depuis le disque.
    pub fn init_caches(&mut self, dir: &Path) {
        self.cache_1h.init(&dir.join("price_cache_1h.bin"));
        self.cache_1d.init(&dir.join("price_cache_1d.bin"));
    }

    /// Sauvegarde les caches sur le disque.
    pub fn save_caches(&self, dir: &Path) {
        self.cache_1h.save(&dir.join("price_cache_1h.bin"));
        self.cache_1d.save(&dir.join("price_cache_1d.bin"));
    }

    // ---------------------------------------------------------------------
    // PRIX HISTORIQUES & FX
    // ---------------------------------------------------------------------

    pub fn historical_price_eur(&self, symbol: &str, time: DateTime<Utc>, kind: MarketKind, ticker: Option<&str>) -> f64 {
        let symbol = symbol.to_uppercase();
        let aligned_ts = Resolution::Hour.align(time.timestamp());

        if kind == MarketKind::Cash {
            return match symbol.as_str() {
                "EUR" | "EURI" => 1.0,
                "USD" | "USDT" | "USDC" | "BUSD" => 1.0 / self.eur_rate("USD", aligned_ts),
                "GBP" => 1.0 / self.eur_rate("GBP", aligned_ts),
                _ => 0.0,
            };
        }

        if let Some(price) = self.cache_1h.get_exact(&symbol, aligned_ts) {
            return price;
        }

        let price = match kind {
            MarketKind::Crypto => self.crypto_closes_eur(&symbol, aligned_ts * 1000, "1h", "1")
                .ok()
                .and_then(|v| v.into_iter().next())
                .map(|(_, p)| p)
                .unwrap_or(0.0),
            MarketKind::Stock => {
                let Some(ticker) = ticker else { return 0.0 };
                let day_str = Utc.timestamp_opt(aligned_ts, 0).unwrap().format("%Y-%m-%d").to_string();
                match self.yahoo_historical_price(ticker, &day_str) {
                    Ok((raw, currency)) => {
                        let (fx_currency, factor) = normalize_currency_for_fx(&currency);
                        let p = raw * factor;
                        if fx_currency != "EUR" { p / self.eur_rate(&fx_currency, aligned_ts) } else { p }
                    }
                    Err(_) => 0.0,
                }
            }
            MarketKind::Cash => unreachable!(),
        };

        if price > 0.0 {
            self.cache_1h.insert(&symbol, aligned_ts, price);
        }
        price
    }

    pub fn eur_rate(&self, currency: &str, ts: i64) -> f64 {
        let (currency, _) = normalize_currency_for_fx(currency);
        if currency == "EUR" {
            return 1.0;
        }

        let aligned_ts = Resolution::Hour.align(ts);
        let live = is_live_bucket(aligned_ts, Resolution::Hour);

        if let Some(rate) = self.lookup_fx_rate(&currency, aligned_ts, live) {
            return rate;
        }

        let pair = format!("EUR{currency}=X");
        let base = Utc.timestamp_opt(aligned_ts, 0).unwrap();
        let period1 = base - chrono::Duration::days(3);
        let period2 = (base + chrono::Duration::days(7)).min(Utc::now() + chrono::Duration::hours(1));

        if let Ok(series) = self.fetch_yahoo(&pair, period1.timestamp(), period2.timestamp(), "1h") {
            for &(t, r) in &series.points {
                if r > 0.0 {
                    self.cache_1h.insert(&currency, t, r);
                }
            }
            if let Some(rate) = closest_at_or_before(&series.points, aligned_ts) {
                if rate > 0.0 {
                    self.cache_1h.insert(&currency, aligned_ts, rate);
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

    fn lookup_fx_rate(&self, currency: &str, aligned_ts: i64, live: bool) -> Option<f64> {
        if live {
            self.cache_1h.get_exact(currency, aligned_ts)
        } else {
            self.cache_1h.get_within(currency, aligned_ts, FX_CACHE_TOLERANCE_SECS)
        }
    }

    fn fetch_yahoo(&self, ticker: &str, period1: i64, period2: i64, interval: &str) -> Result<YahooSeries, PriceError> {
        let url = format!("{YAHOO_CHART_API}/{ticker}");
        let resp = self.http_client
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

    pub fn yahoo_historical_price(&self, ticker: &str, day_str: &str) -> Result<(f64, String), PriceError> {
        let target = chrono::NaiveDate::parse_from_str(day_str, "%Y-%m-%d").map_err(|e| PriceError::Message(e.to_string()))?;
        let target_dt = Utc.from_utc_datetime(&target.and_hms_opt(0, 0, 0).unwrap());
        let series = self.fetch_yahoo(
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

    fn daily_closes_cached<F>(&self, cache_key: &str, days: i64, fetch: F) -> Result<Vec<(chrono::NaiveDate, f64)>, PriceError>
    where
        F: FnOnce(&MarketDataClient, i64) -> Result<Vec<(i64, f64)>, PriceError>,
    {
        let yesterday = Utc::now().date_naive() - chrono::Duration::days(1);
        let yesterday_ts = Resolution::Day.align(Utc.from_utc_datetime(&yesterday.and_hms_opt(0, 0, 0).unwrap()).timestamp());
        let cutoff_ts = Resolution::Day.align((Utc::now() - chrono::Duration::days(days)).timestamp());

        let fresh = self.cache_1d.latest(cache_key).map_or(false, |ts| ts >= yesterday_ts);
        let deep_enough = self.cache_1d.earliest(cache_key).map_or(false, |ts| ts <= cutoff_ts);

        if !(fresh && deep_enough) {
            let start_ts = cutoff_ts - Resolution::Day.seconds() * 5;
            for (ts, close) in fetch(self, start_ts)? {
                let day = Utc.timestamp_opt(ts, 0).unwrap().date_naive();
                self.cache_1d.insert_date(cache_key, day, close);
            }
        }

        Ok(self.cache_1d.range_days(cache_key, days))
    }

    pub fn yahoo_daily_closes(&self, ticker: &str, days: i64) -> Result<Vec<(chrono::NaiveDate, f64)>, PriceError> {
        let cache_key = format!("YHO:{ticker}");
        self.daily_closes_cached(&cache_key, days, |client, start_ts| {
            let series = client.fetch_yahoo(ticker, start_ts, Utc::now().timestamp(), "1d")?;
            let (fx_currency, price_factor) = normalize_currency_for_fx(&series.currency);
            Ok(series
                .points
                .into_iter()
                .map(|(ts, mut close)| {
                    close *= price_factor;
                    if fx_currency != "EUR" {
                        close /= client.eur_rate(&fx_currency, ts);
                    }
                    (ts, close)
                })
                .collect())
        })
    }

    fn fetch_binance_close(&self, pair: &str, start_ms: i64, interval: &str, limit: &str) -> Result<Vec<(i64, f64)>, PriceError> {
        let resp = self.http_client
            .get(BINANCE_API)
            .query(&[("symbol", pair), ("interval", interval), ("startTime", &start_ms.to_string()), ("limit", limit)])
            .send()?;
        
        if !resp.status().is_success() {
            return Ok(Vec::new()); 
        }
        
        let data: Vec<Value> = resp.json()?;
        Ok(data
            .iter()
            .filter_map(|k| {
                let ts = k.get(0)?.as_i64()? / 1000;
                let close = k.get(4)?.as_str()?.parse::<f64>().ok()?;
                Some((ts, close))
            })
            .collect())
    }

    fn crypto_closes_eur(&self, symbol: &str, start_ms: i64, interval: &str, limit: &str) -> Result<Vec<(i64, f64)>, PriceError> {
        let direct = self.fetch_binance_close(&format!("{symbol}EUR"), start_ms, interval, limit)?;
        if !direct.is_empty() {
            return Ok(direct);
        }

        let usdt = self.fetch_binance_close(&format!("{symbol}USDT"), start_ms, interval, limit)?;
        if usdt.is_empty() {
            return Ok(Vec::new());
        }
        
        let eur_usdt: HashMap<i64, f64> = self.fetch_binance_close("EURUSDT", start_ms, interval, limit)?.into_iter().collect();
        Ok(usdt.into_iter().filter_map(|(ts, p)| eur_usdt.get(&ts).map(|&fx| (ts, p / fx))).collect())
    }

    pub fn binance_daily_closes(&self, symbol: &str, days: i64) -> Result<Vec<(chrono::NaiveDate, f64)>, PriceError> {
        let cache_key = format!("BIN:{symbol}");
        self.daily_closes_cached(&cache_key, days, |client, start_ts| {
            client.crypto_closes_eur(symbol, start_ts * 1000, "1d", "1000")
        })
    }

    // ---------------------------------------------------------------------
    // RÉSOLUTION DES TICKERS
    // ---------------------------------------------------------------------

    pub fn resolve_ticker(&self, symbol: &str, kind: MarketKind) -> Option<String> {
        match kind {
            MarketKind::Crypto => self.ticker_for_crypto(symbol),
            MarketKind::Stock => self.ticker_for_stock(symbol),
            MarketKind::Cash => None,
        }
    }

    fn ticker_for_crypto(&self, symbol: &str) -> Option<String> {
        let known = self.binance_known_base_assets();
        let symbol = symbol.to_uppercase();
        if known.contains(&symbol) {
            Some(symbol)
        } else {
            None
        }
    }

    fn binance_known_base_assets(&self) -> HashSet<String> {
        let mut cache = self.binance_assets_cache.lock().unwrap();
        if let Some(set) = cache.as_ref() {
            return set.clone();
        }

        let set = (|| -> Option<HashSet<String>> {
            let resp = self.http_client.get(BINANCE_EXCHANGE_INFO).send().ok()?.error_for_status().ok()?;
            let data: Value = resp.json().ok()?;
            let symbols = data.get("symbols")?.as_array()?;
            Some(
                symbols
                    .iter()
                    .filter_map(|s| s.get("baseAsset").and_then(|b| b.as_str()).map(String::from))
                    .collect(),
            )
        })()
        .unwrap_or_default();

        *cache = Some(set.clone());
        set
    }

    fn ticker_for_stock(&self, symbol: &str) -> Option<String> {
        let symbol = symbol.to_uppercase();

        let mut ticker_cache = self.ticker_cache.lock().unwrap();
        if let Some(cached) = ticker_cache.get(&symbol) {
            return cached.clone();
        }

        if let Some((_, over)) = manual_ticker_overrides().iter().find(|(k, _)| *k == symbol) {
            let result = Some(over.to_string());
            ticker_cache.insert(symbol, result.clone());
            return result;
        }

        let mut yahoo_symbol_guess = symbol.clone();
        let mut suffix_applied = false;
        for (xtb_suf, yahoo_suf) in xtb_to_yahoo_suffix() {
            if symbol.ends_with(xtb_suf) {
                yahoo_symbol_guess = format!("{}{}", &symbol[..symbol.len() - xtb_suf.len()], yahoo_suf);
                suffix_applied = true;
                break;
            }
        }

        if suffix_applied {
            let result = Some(yahoo_symbol_guess);
            ticker_cache.insert(symbol, result.clone());
            return result;
        }

        let queries: Vec<&str> = if yahoo_symbol_guess != symbol {
            vec![yahoo_symbol_guess.as_str(), symbol.as_str()]
        } else {
            vec![yahoo_symbol_guess.as_str()]
        };

        let mut result: Option<String> = None;
        for query in queries {
            let attempt = (|| -> Option<String> {
                let resp = self.http_client
                    .get(YAHOO_SEARCH_API)
                    .query(&[("q", query), ("quotesCount", "5"), ("newsCount", "0")])
                    .send()
                    .ok()?
                    .error_for_status()
                    .ok()?;
                let data: Value = resp.json().ok()?;
                let quotes = data.get("quotes")?.as_array()?;
                best_quote(quotes, &yahoo_symbol_guess)
            })();
            if attempt.is_some() {
                result = attempt;
                break;
            }
        }

        ticker_cache.insert(symbol, result.clone());
        result
    }
}

// --- Fonctions pures de mapping (hors instance) ---

fn best_quote(quotes: &[Value], expected_symbol: &str) -> Option<String> {
    let candidates: Vec<&Value> = quotes
        .iter()
        .filter(|q| q.get("quoteType").and_then(|t| t.as_str()).map_or(false, |t| ACCEPTED_QUOTE_TYPES.contains(&t)))
        .collect();
    if candidates.is_empty() {
        return None;
    }

    let expected_upper = expected_symbol.to_uppercase();
    if let Some(exact) = candidates.iter().find(|q| {
        q.get("symbol").and_then(|s| s.as_str()).map(|s| s.to_uppercase()) == Some(expected_upper.clone())
    }) {
        return exact.get("symbol").and_then(|s| s.as_str()).map(String::from);
    }

    let expected_root = expected_upper.split('.').next().unwrap_or("").to_string();
    let same_root: Vec<&&Value> = candidates
        .iter()
        .filter(|q| {
            q.get("symbol")
                .and_then(|s| s.as_str())
                .map(|s| s.split('.').next().unwrap_or("").to_uppercase())
                == Some(expected_root.clone())
        })
        .collect();

    if same_root.len() == 1 {
        return same_root[0].get("symbol").and_then(|s| s.as_str()).map(String::from);
    }

    None
}

fn xtb_to_yahoo_suffix() -> &'static [(&'static str, &'static str)] {
    &[
        (".FR", ".PA"),
        (".NL", ".AS"),
        (".UK", ".L"),
        (".DE", ".DE"),
        (".US", ""),
        (".PL", ".WA"),
    ]
}

fn manual_ticker_overrides() -> &'static [(&'static str, &'static str)] {
    &[
        ("MSTR.US", "MSTR"),
        ("PRIM.US", "PRIM"),
        ("XFVT.DE", "XFVT.DE"),
        ("CNYA.DE", "CNYA.L"),
        ("STM.FR", "STM"),
    ]
}
