//! Portage de src/parse/binance.py.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use chrono::{NaiveDateTime, TimeZone, Utc};
use regex::Regex;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::schema::{Asset, AssetIdentifiers, AssetKind, Platform, Transaction, TransactionKind};
use crate::market::client::MarketDataClient; 

#[derive(Debug, Error)]
pub enum BinanceParseError {
    #[error("colonne manquante: {0}")]
    MissingColumn(String),
    #[error("erreur de lecture CSV: {0}")]
    Csv(String),
    #[error("format de montant invalide '{0}'")]
    InvalidAmount(String),
    #[error("format de date invalide '{0}'")]
    InvalidTime(String),
    #[error("side inconnu: {0}")]
    UnknownSide(String),
}

// On force l'utilisation de std::result::Result pour ne pas conflit avec anyhow::Result
pub fn parse_trades(
    path: &Path, 
    known_ids: &HashSet<String>,
    price_client: &MarketDataClient,
) -> std::result::Result<Vec<Transaction>, BinanceParseError> {
    let source_file = path.display().to_string();
    let mut out = Vec::new();

    let mut reader = csv::Reader::from_path(path).map_err(|e| BinanceParseError::Csv(e.to_string()))?;
    let headers = reader.headers().map_err(|e| BinanceParseError::Csv(e.to_string()))?.clone();

    for record in reader.records() {
        let record = match record {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[WARN] Ligne CSV illisible dans Binance Trades: {e}");
                continue;
            }
        };

        let row: HashMap<&str, &str> = headers.iter().zip(record.iter()).collect();
        
        macro_rules! get_col {
            ($k:expr) => {
                row.get($k).copied().ok_or_else(|| BinanceParseError::MissingColumn($k.to_string()))?
            };
        }

        let trade_id = synthetic_id(
            "binance-trade",
            &[get_col!("Time"), get_col!("Pair"), get_col!("Side"), get_col!("Price"), get_col!("Executed"), get_col!("Amount")],
        );

        if known_ids.contains(&trade_id) {
            continue;
        }

        let parsed_row = (|| -> std::result::Result<(), BinanceParseError> {
            let time_str = get_col!("Time");
            let naive = NaiveDateTime::parse_from_str(time_str, "%Y-%m-%d %H:%M:%S")
                .map_err(|_| BinanceParseError::InvalidTime(time_str.to_string()))?;
            let time = Utc.from_utc_datetime(&naive);

            let (base_qty, base_symbol) = split_amount(get_col!("Executed"))?;
            let (quote_amount, quote_symbol) = split_amount(get_col!("Amount"))?;
            let (fee_amount, fee_symbol) = split_amount(get_col!("Fee"))?;

            let side = get_col!("Side").to_uppercase();
            let kind = match side.as_str() {
                "BUY" => TransactionKind::Buy,
                "SELL" => TransactionKind::Sell,
                other => return Err(BinanceParseError::UnknownSide(other.to_string())),
            };

            let base_ref_currency = normalize_currency(&base_symbol).unwrap_or("USD").to_string();
            let base_asset = Asset {
                symbol: base_symbol.clone(),
                name: base_symbol.clone(),
                kind: asset_kind_for(&base_symbol),
                ref_currency: base_ref_currency,
                identifiers: AssetIdentifiers::default(),
            };

            let eur_price = price_client.historical_price_eur(&base_symbol, time, asset_kind_for(&base_symbol), None);
            let value_eur = base_qty * eur_price;
           
            out.push(Transaction {
                platform: Platform::Binance,
                account_label: "Spot".to_string(),
                kind,
                asset: base_asset,
                quantity: base_qty,
                price: Some(eur_price),
                amount: Some(quote_amount),
                quote_currency: Some(normalize_currency(&quote_symbol).map(String::from).unwrap_or(quote_symbol.clone())),
                time,
                value_eur,
                external_id: Some(trade_id.clone()),
                remark: None,
                source_file: source_file.clone(),
            });

            if fee_amount > 0.0 {
                let fee_asset = Asset {
                    symbol: fee_symbol.clone(),
                    name: fee_symbol.clone(),
                    kind: asset_kind_for(&fee_symbol),
                    ref_currency: normalize_currency(&fee_symbol).unwrap_or("USD").to_string(),
                    identifiers: AssetIdentifiers::default(),
                };
                let fee_price_eur = price_client.historical_price_eur(&fee_symbol, time, asset_kind_for(&fee_symbol), None);
                out.push(Transaction {
                    platform: Platform::Binance,
                    account_label: "Spot".to_string(),
                    kind: TransactionKind::Fee,
                    asset: fee_asset,
                    quantity: fee_amount,
                    price: Some(fee_price_eur),
                    amount: None,
                    quote_currency: None,
                    time,
                    value_eur: fee_amount * fee_price_eur,
                    external_id: None,
                    remark: Some(format!("Fee on {side} {base_symbol}")),
                    source_file: source_file.clone(),
                });
            }
            Ok(())
        })();

        if let Err(e) = parsed_row {
            eprintln!("[WARN] Ligne ignorée dans Binance (ID: {}) : {}", trade_id, e);
        }
    }

    Ok(out)
}

fn split_amount(raw: &str) -> std::result::Result<(f64, String), BinanceParseError> {
    let re = Regex::new(r"^([0-9.]+)([A-Za-z]+)$").unwrap();
    let caps = re.captures(raw.trim()).ok_or_else(|| BinanceParseError::InvalidAmount(raw.to_string()))?;
    let qty: f64 = caps[1].parse().map_err(|_| BinanceParseError::InvalidAmount(raw.to_string()))?;
    Ok((qty, caps[2].to_string()))
}
pub fn synthetic_id(prefix: &str, parts: &[&str]) -> String {
    let joined = parts.join("|");
    let digest = Sha256::digest(joined.as_bytes());
    format!("{prefix}-{}", hex::encode(&digest)[..16].to_string())
}

fn split_space_amount(raw: &str) -> Result<(f64, String)> {
    let mut parts = raw.trim().splitn(2, ' ');
    let qty_str = parts.next().ok_or_else(|| anyhow!("pas de symbole dans '{raw}'"))?;
    let symbol = parts.next().ok_or_else(|| anyhow!("pas de symbole dans '{raw}'"))?;
    Ok((qty_str.parse()?, symbol.to_string()))
}

fn normalize_currency(coin: &str) -> Option<&'static str> {
    match coin {
        "EUR" | "EURI" => Some("EUR"),
        "USDC" | "BUSD" | "USD" => Some("USD"),
        _ => None,
    }
}

fn asset_kind_for(symbol: &str) -> AssetKind {
    if normalize_currency(symbol).is_some() {
        AssetKind::Cash
    } else {
        AssetKind::Crypto
    }
}

fn parse_time(raw: &str) -> Result<chrono::DateTime<Utc>> {
    let naive = NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S")?;
    Ok(Utc.from_utc_datetime(&naive))
}

pub fn parse_converts(
    path: &Path, 
    known_ids: &HashSet<String>,
    price_client: &MarketDataClient
) -> Result<Vec<Transaction>> {
    let source_file = path.display().to_string();
    let mut out = Vec::new();

    let mut reader = csv::Reader::from_path(path).with_context(|| format!("lecture de {path:?}"))?;
    let headers = reader.headers()?.clone();

    for record in reader.records() {
       let record = record?;
        let row: HashMap<&str, &str> = headers.iter().zip(record.iter()).collect();
        let get = |k: &str| -> Result<&str> { row.get(k).copied().ok_or_else(|| anyhow!("colonne '{k}' manquante")) };

        if get("Status")? != "Successful" { continue; }

        let convert_id = synthetic_id(
            "binance-convert",
            &[get("Time")?, get("Wallet")?, get("Pair")?, get("Sell")?, get("Buy")?, get("Price")?],
        );

        if known_ids.contains(&format!("{convert_id}-sell")) && known_ids.contains(&format!("{convert_id}-buy")) {
            continue;
        }

        let time = parse_time(get("Time")?)?;
        let (sell_qty, sell_symbol) = split_space_amount(get("Sell")?)?;
        let (buy_qty, buy_symbol) = split_space_amount(get("Buy")?)?;

        let sell_asset = Asset {
            symbol: sell_symbol.clone(),
            name: sell_symbol.clone(),
            kind: asset_kind_for(&sell_symbol),
            ref_currency: normalize_currency(&sell_symbol).unwrap_or("USD").to_string(),
            identifiers: AssetIdentifiers::default(),
        };
        let buy_asset = Asset {
            symbol: buy_symbol.clone(),
            name: buy_symbol.clone(),
            kind: asset_kind_for(&buy_symbol),
            ref_currency: normalize_currency(&buy_symbol).unwrap_or("USD").to_string(),
            identifiers: AssetIdentifiers::default(),
        };

        let sell_price_eur = price_client.historical_price_eur(&sell_symbol, time, asset_kind_for(&sell_symbol), None);
        let sell_value_eur = sell_qty * sell_price_eur;

        let buy_price_eur = price_client.historical_price_eur(&buy_symbol, time, asset_kind_for(&buy_symbol), None);
        let buy_value_eur = buy_qty * buy_price_eur;

        out.push(Transaction {
            platform: Platform::Binance,
            account_label: get("Wallet")?.to_string(),
            kind: TransactionKind::Sell,
            asset: sell_asset,
            quantity: sell_qty,
            price: Some(sell_price_eur),
            amount: Some(sell_value_eur),
            value_eur: sell_value_eur,
            quote_currency: Some("EUR".to_string()),
            time,
            external_id: Some(format!("{convert_id}-sell")),
            remark: Some("Convert".to_string()),
            source_file: source_file.clone(),
        });
        out.push(Transaction {
            platform: Platform::Binance,
            account_label: get("Wallet")?.to_string(),
            kind: TransactionKind::Buy,
            asset: buy_asset,
            quantity: buy_qty,
            price: Some(buy_price_eur),
            amount: Some(buy_value_eur),
            value_eur: buy_value_eur,
            quote_currency: Some("EUR".to_string()),
            time,
            external_id: Some(format!("{convert_id}-buy")),
            remark: Some("Convert".to_string()),
            source_file: source_file.clone(),
        });
    }

    Ok(out)
}

pub fn parse_binance_sources(accounts_path: PathBuf, price_client: &MarketDataClient) -> Vec<Transaction> {
    let mut out = Vec::new();
    let trades_path = accounts_path.join("trades.csv");
    let converts_path = accounts_path.join("convert.csv");

    if trades_path.exists() {
        println!("Lecture Binance Trades : {trades_path:?}");
        let empty_ids = HashSet::new();
        match parse_trades(&trades_path, &empty_ids, price_client) {
            Ok(mut tx) => out.append(&mut tx),
            Err(e) => eprintln!("  [Erreur Binance Trades] {e}"),
        }
    } else {
        println!("[Omis] Fichier introuvable : {trades_path:?}");
    }

    if converts_path.exists() {
        println!("Lecture Binance Converts : {converts_path:?}");
        let empty_ids = HashSet::new();
        match parse_converts(&converts_path, &empty_ids, price_client) {
            Ok(mut tx) => out.append(&mut tx),
            Err(e) => eprintln!("  [Erreur Binance Converts] {e}"),
        }
    } else {
        println!("[Omis] Fichier introuvable : {converts_path:?}");
    }

    out
}