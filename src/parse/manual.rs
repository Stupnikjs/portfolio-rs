//! Portage de src/parse/manual.py -- comble les trous non couverts par les
//! exports automatisés (ex: on-chain non tracé). Remplacé en bloc à chaque
//! run -- voir TxStore::replace_platform.

use std::fs;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::parse::binance::synthetic_id;
use crate::schema::{Asset, AssetIdentifiers, AssetKind, Platform, Transaction, TransactionKind};

fn parse_time(raw: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(&raw.replace('Z', "+00:00"))?.with_timezone(&Utc))
}

fn kind_from_str(s: &str) -> Result<TransactionKind> {
    Ok(match s {
        "Buy" => TransactionKind::Buy,
        "Sell" => TransactionKind::Sell,
        "Fee" => TransactionKind::Fee,
        "Deposit" => TransactionKind::Deposit,
        "Withdraw" => TransactionKind::Withdraw,
        other => return Err(anyhow!("TransactionKind inconnu: {other}")),
    })
}

pub fn parse_manual(path: &Path) -> Result<Vec<Transaction>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let raw = fs::read_to_string(path).with_context(|| format!("lecture de {path:?}"))?;
    let entries: Vec<Value> = serde_json::from_str(&raw)?;
    let mut out = Vec::new();

    for entry in entries {
        let symbol = entry.get("asset").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("champ 'asset' manquant"))?.to_string();
        let asset = Asset {
            symbol: symbol.clone(),
            name: symbol.clone(),
            kind: AssetKind::Crypto,
            ref_currency: "EUR".to_string(),
            identifiers: AssetIdentifiers::default(),
        };

        let raw_id = entry
            .get("id")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| synthetic_id("manual-auto", &[&serde_json::to_string(&entry).unwrap_or_default()]));

        let kind_str = entry.get("kind").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("champ 'kind' manquant"))?;

        out.push(Transaction {
            platform: Platform::Manual,
            account_label: entry.get("account_label").and_then(|v| v.as_str()).unwrap_or("Manuel").to_string(),
            kind: kind_from_str(kind_str)?,
            asset,
            quantity: entry.get("quantity").and_then(|v| v.as_f64()).ok_or_else(|| anyhow!("champ 'quantity' manquant"))?,
            price: entry.get("price").and_then(|v| v.as_f64()),
            value_eur: entry.get("value_eur").and_then(|v| v.as_f64()).ok_or_else(|| anyhow!("champ 'value_eur' manquant"))?,
            amount: entry.get("amount").and_then(|v| v.as_f64()),
            quote_currency: entry.get("quote_currency").and_then(|v| v.as_str()).map(String::from),
            time: parse_time(entry.get("time").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("champ 'time' manquant"))?)?,
            external_id: Some(format!("manual-{raw_id}")),
            remark: entry.get("remark").and_then(|v| v.as_str()).map(String::from),
            source_file: path.display().to_string(),
        });
    }

    Ok(out)
}
