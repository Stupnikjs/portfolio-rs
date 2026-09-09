//! Portage de src/parse/binance.py.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use chrono::{NaiveDateTime, TimeZone, Utc};
use regex::Regex;
use sha2::{Digest, Sha256};

use crate::market::prices::historical_price_eur;
use crate::schema::{Asset, AssetIdentifiers, AssetKind, Platform, Transaction, TransactionKind};

/// ID stable et déterministe pour le dédoublonnage, calculé sur les champs
/// bruts du CSV -- recalculer le même hash sur les mêmes données d'export
/// donne toujours le même external_id.
pub fn synthetic_id(prefix: &str, parts: &[&str]) -> String {
    let joined = parts.join("|");
    let digest = Sha256::digest(joined.as_bytes());
    format!("{prefix}-{}", hex::encode(&digest)[..16].to_string())
}

/// Sépare une cellule collée '<nombre><symbole>' (format Trade History).
fn split_amount(raw: &str) -> Result<(f64, String)> {
    let re = Regex::new(r"^([0-9.]+)([A-Za-z]+)$").unwrap();
    let caps = re.captures(raw.trim()).ok_or_else(|| anyhow!("pas de suffixe trouvé dans '{raw}'"))?;
    let qty: f64 = caps[1].parse()?;
    Ok((qty, caps[2].to_string()))
}

/// Sépare une cellule '<nombre> <symbole>' (format Convert), à ne pas
/// confondre avec `split_amount` (collé, format Trade History).
fn split_space_amount(raw: &str) -> Result<(f64, String)> {
    let mut parts = raw.trim().splitn(2, ' ');
    let qty_str = parts.next().ok_or_else(|| anyhow!("pas de symbole dans '{raw}'"))?;
    let symbol = parts.next().ok_or_else(|| anyhow!("pas de symbole dans '{raw}'"))?;
    Ok((qty_str.parse()?, symbol.to_string()))
}

fn normalize_currency(coin: &str) -> Option<&'static str> {
    match coin {
        "EUR" | "EURI" => Some("EUR"),
        "USDC" | "USDT" => Some("USD"),
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

/// Parse un export Binance 'Trade History' en transactions Buy/Sell + Fee.
///
/// Seule source Binance retenue -- le format 'Account Statement' est
/// abandonné (pas de prix d'exécution fiable, appariement heuristique trop
/// fragile).
pub fn parse_trades(path: &Path) -> Result<Vec<Transaction>> {
    let source_file = path.display().to_string();
    let mut out = Vec::new();

    let mut reader = csv::Reader::from_path(path).with_context(|| format!("lecture de {path:?}"))?;
    let headers = reader.headers()?.clone();

    for record in reader.records() {
        let record = record?;
        let row: HashMap<&str, &str> = headers.iter().zip(record.iter()).collect();
        let get = |k: &str| -> Result<&str> { row.get(k).copied().ok_or_else(|| anyhow!("colonne '{k}' manquante")) };

        let time = parse_time(get("Time")?)?;
        let (base_qty, base_symbol) = split_amount(get("Executed")?)?;
        let (quote_amount, quote_symbol) = split_amount(get("Amount")?)?;
        let (fee_amount, fee_symbol) = split_amount(get("Fee")?)?;

        let side = get("Side")?.to_uppercase();
        let kind = match side.as_str() {
            "BUY" => TransactionKind::Buy,
            "SELL" => TransactionKind::Sell,
            other => return Err(anyhow!("side inconnu: {other}")),
        };

        let base_ref_currency = normalize_currency(&base_symbol).unwrap_or("USD").to_string();
        let base_asset = Asset {
            symbol: base_symbol.clone(),
            name: base_symbol.clone(),
            kind: asset_kind_for(&base_symbol),
            ref_currency: base_ref_currency,
            identifiers: AssetIdentifiers::default(),
        };

        let quote_currency = normalize_currency(&quote_symbol).map(String::from).unwrap_or(quote_symbol.clone());
        let eur_price = historical_price_eur(&base_symbol, time, asset_kind_for(&base_symbol), None);
        let value_eur = base_qty * eur_price;
        let trade_id = synthetic_id(
            "binance-trade",
            &[get("Time")?, get("Pair")?, get("Side")?, get("Price")?, get("Executed")?, get("Amount")?],
        );

        out.push(Transaction {
            platform: Platform::Binance,
            account_label: "Spot".to_string(),
            kind,
            asset: base_asset,
            quantity: base_qty,
            price: Some(eur_price),
            amount: Some(quote_amount),
            quote_currency: Some(quote_currency),
            time,
            value_eur,
            external_id: Some(trade_id),
            remark: None,
            source_file: source_file.clone(),
        });

        if fee_amount > 0.0 {
            let fee_ref_currency = normalize_currency(&fee_symbol).unwrap_or("USD").to_string();
            let fee_asset = Asset {
                symbol: fee_symbol.clone(),
                name: fee_symbol.clone(),
                kind: asset_kind_for(&fee_symbol),
                ref_currency: fee_ref_currency,
                identifiers: AssetIdentifiers::default(),
            };
            // Le prix et la valeur du fee doivent être basés sur l'actif du
            // fee (ex: BNB), pas sur le base (ex: BTC).
            let fee_price_eur = historical_price_eur(&fee_symbol, time, asset_kind_for(&fee_symbol), None);
            let fee_value_eur = fee_amount * fee_price_eur;

            out.push(Transaction {
                platform: Platform::Binance,
                account_label: "Spot".to_string(),
                kind: TransactionKind::Fee,
                asset: fee_asset,
                quantity: fee_amount,
                price: Some(eur_price),
                amount: None,
                quote_currency: None,
                time,
                value_eur: fee_value_eur,
                external_id: None,
                remark: Some(format!("Fee on {side} {base_symbol}")),
                source_file: source_file.clone(),
            });
        }
    }

    Ok(out)
}

/// Parse un export Binance 'Convert History'. Chaque ligne réussie devient
/// deux transactions (Sell de l'actif cédé, Buy de l'actif reçu) -- les
/// conversions échouées/annulées (Status != 'Successful') sont ignorées.
pub fn parse_converts(path: &Path) -> Result<Vec<Transaction>> {
    let source_file = path.display().to_string();
    let mut out = Vec::new();

    let mut reader = csv::Reader::from_path(path).with_context(|| format!("lecture de {path:?}"))?;
    let headers = reader.headers()?.clone();

    for record in reader.records() {
        let record = record?;
        let row: HashMap<&str, &str> = headers.iter().zip(record.iter()).collect();
        let get = |k: &str| -> Result<&str> { row.get(k).copied().ok_or_else(|| anyhow!("colonne '{k}' manquante")) };

        if get("Status")? != "Successful" {
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

        let sell_price_eur = historical_price_eur(&sell_symbol, time, asset_kind_for(&sell_symbol), None);
        let buy_price_eur = historical_price_eur(&buy_symbol, time, asset_kind_for(&buy_symbol), None);
        let buy_value_eur = buy_qty * buy_price_eur;
        let sell_value_eur = sell_qty * sell_price_eur;

        let convert_id = synthetic_id(
            "binance-convert",
            &[get("Time")?, get("Wallet")?, get("Pair")?, get("Sell")?, get("Buy")?, get("Price")?],
        );

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
