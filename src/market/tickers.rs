//! Portage de src/market/tickers.py -- résolution du ticker externe
//! (AssetIdentifiers.ticker) pour un Asset :
//! - STOCK  -> recherche Yahoo Finance (endpoint non officiel).
//! - CRYPTO -> API publique Binance (exchangeInfo), confirme que le
//!   symbole est bien coté sur Binance et le renvoie tel quel.
//!
//! Best-effort : toute erreur réseau/format renvoie None plutôt que de
//! propager -- un ticker manquant ne doit jamais bloquer le pipeline.

use std::collections::HashMap;
use std::sync::Mutex;

use once_cell::sync::Lazy;
use serde_json::Value;

use crate::schema::AssetKind;

const YAHOO_SEARCH_API: &str = "https://query2.finance.yahoo.com/v1/finance/search";
const BINANCE_EXCHANGE_INFO: &str = "https://data-api.binance.vision/api/v3/exchangeInfo";

// Mapping des suffixes XTB -> Yahoo Finance.
fn xtb_to_yahoo_suffix() -> &'static [(&'static str, &'static str)] {
    &[
        (".FR", ".PA"), // Euronext Paris
        (".NL", ".AS"), // Euronext Amsterdam
        (".UK", ".L"),  // London Stock Exchange
        (".DE", ".DE"), // Xetra (identique)
        (".US", ""),    // pas de suffixe pour les US (ex: MSTR.US -> MSTR)
        (".PL", ".WA"), // Varsovie
    ]
}

// Corrections manuelles pour les cas où la recherche Yahoo se trompe
// systématiquement.
fn manual_ticker_overrides() -> &'static [(&'static str, &'static str)] {
    &[
        ("MSTR.US", "MSTR"),
        ("XFVT.DE", "XFVT.DE"), // plusieurs classes de parts homonymes -> ambigu pour la recherche Yahoo
    ]
}

const ACCEPTED_QUOTE_TYPES: &[&str] = &["EQUITY", "ETF"];

static BINANCE_ASSETS_CACHE: Lazy<Mutex<Option<std::collections::HashSet<String>>>> = Lazy::new(|| Mutex::new(None));
static STOCK_TICKER_CACHE: Lazy<Mutex<HashMap<String, Option<String>>>> = Lazy::new(|| Mutex::new(HashMap::new()));

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .user_agent("Mozilla/5.0")
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("client HTTP")
}

fn binance_known_base_assets() -> std::collections::HashSet<String> {
    let mut cache = BINANCE_ASSETS_CACHE.lock().unwrap();
    if let Some(set) = cache.as_ref() {
        return set.clone();
    }

    let set = (|| -> Option<std::collections::HashSet<String>> {
        let resp = client().get(BINANCE_EXCHANGE_INFO).send().ok()?.error_for_status().ok()?;
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

pub fn ticker_for_crypto(symbol: &str) -> Option<String> {
    let known = binance_known_base_assets();
    let symbol = symbol.to_uppercase();
    if known.contains(&symbol) {
        Some(symbol)
    } else {
        None
    }
}

/// Choisit le meilleur candidat parmi les résultats Yahoo :
/// 1. correspondance exacte du symbole complet (gère un même fonds coté
///    sur plusieurs places, ex: XFVT.DE / XFVT.MI / XFVT.SW / XFVT.L) ;
/// 2. à défaut, même radical (avant le point) et type EQUITY/ETF, pour
///    éviter les produits dérivés au nom proche (ex: MSTU/MSTX pour MSTR).
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

    None // ambigu (plusieurs places de cotation) -> mieux vaut échouer que se tromper
}

pub fn ticker_for_stock(symbol: &str) -> Option<String> {
    let symbol = symbol.to_uppercase();

    if let Some(cached) = STOCK_TICKER_CACHE.lock().unwrap().get(&symbol) {
        return cached.clone();
    }

    if let Some((_, over)) = manual_ticker_overrides().iter().find(|(k, _)| *k == symbol) {
        let result = Some(over.to_string());
        STOCK_TICKER_CACHE.lock().unwrap().insert(symbol, result.clone());
        return result;
    }

    let mut yahoo_symbol_guess = symbol.clone();
    for (xtb_suf, yahoo_suf) in xtb_to_yahoo_suffix() {
        if symbol.ends_with(xtb_suf) {
            yahoo_symbol_guess = format!("{}{}", &symbol[..symbol.len() - xtb_suf.len()], yahoo_suf);
            break;
        }
    }

    let queries: Vec<&str> = if yahoo_symbol_guess != symbol {
        vec![yahoo_symbol_guess.as_str(), symbol.as_str()]
    } else {
        vec![yahoo_symbol_guess.as_str()]
    };

    let mut result: Option<String> = None;
    for query in queries {
        let attempt = (|| -> Option<String> {
            let resp = client()
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

    STOCK_TICKER_CACHE.lock().unwrap().insert(symbol, result.clone());
    result
}

pub fn resolve_ticker(symbol: &str, kind: AssetKind) -> Option<String> {
    match kind {
        AssetKind::Crypto => ticker_for_crypto(symbol),
        AssetKind::Stock => ticker_for_stock(symbol),
        AssetKind::Cash => None,
    }
}
