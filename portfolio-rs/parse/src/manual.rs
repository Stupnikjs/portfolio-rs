//! Portage de src/parse/manual.py -- comble les trous non couverts par les
//! exports automatisés (ex: on-chain non tracé). Remplacé en bloc à chaque
//! run -- voir TxStore::replace_platform.

use std::fs;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::binance::synthetic_id;
use model::schema::{Asset, AssetIdentifiers, AssetKind, Platform, Transaction, TransactionKind};

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
            cash_flow: None,
        });
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn write_temp(name: &str, content: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("importers-manual-{}-{name}.json", std::process::id()));
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn a_missing_file_means_no_manual_entries() {
        let path = std::env::temp_dir().join("importers-manual-does-not-exist.json");
        assert!(parse_manual(&path).unwrap().is_empty());
    }

    #[test]
    fn a_complete_entry_becomes_a_manual_crypto_transaction() {
        let path = write_temp(
            "complete",
            r#"[{"id":"abc","asset":"ETH","kind":"Buy","quantity":0.5,"price":2000.0,"value_eur":1000.0,
                 "amount":1000.0,"quote_currency":"EUR","time":"2025-03-01T10:00:00Z",
                 "account_label":"Ledger","remark":"on-chain"}]"#,
        );
        let tx = parse_manual(&path).unwrap();
        let _ = fs::remove_file(&path);

        assert_eq!(tx.len(), 1);
        let t = &tx[0];
        assert_eq!(t.platform, Platform::Manual);
        assert_eq!(t.kind, TransactionKind::Buy);
        assert_eq!((t.asset.symbol.as_str(), t.asset.kind), ("ETH", AssetKind::Crypto));
        assert_eq!((t.quantity, t.price, t.value_eur, t.amount), (0.5, Some(2000.0), 1000.0, Some(1000.0)));
        assert_eq!(t.quote_currency.as_deref(), Some("EUR"));
        assert_eq!(t.account_label, "Ledger");
        assert_eq!(t.remark.as_deref(), Some("on-chain"));
        assert_eq!(t.external_id.as_deref(), Some("manual-abc"));
        assert_eq!(t.time, chrono::Utc.with_ymd_and_hms(2025, 3, 1, 10, 0, 0).unwrap());
        assert_eq!(t.cash_flow, None);
    }

    #[test]
    fn optional_fields_default_and_a_missing_id_gets_a_stable_synthetic_one() {
        let json = r#"[{"asset":"BTC","kind":"Deposit","quantity":1.0,"value_eur":20000.0,"time":"2025-01-01T00:00:00Z"}]"#;
        let (p1, p2) = (write_temp("auto-1", json), write_temp("auto-2", json));
        let (a, b) = (parse_manual(&p1).unwrap(), parse_manual(&p2).unwrap());
        let _ = (fs::remove_file(&p1), fs::remove_file(&p2));

        assert_eq!(a[0].account_label, "Manuel");
        assert_eq!((a[0].price, a[0].amount, a[0].remark.clone()), (None, None, None));
        let id = a[0].external_id.clone().unwrap();
        assert!(id.starts_with("manual-manual-auto-"), "id inattendu : {id}");
        assert_eq!(a[0].external_id, b[0].external_id); // même contenu => même id
    }

    #[test]
    fn invalid_entries_are_rejected_with_the_offending_field() {
        let cases = [
            (r#"[{"asset":"BTC","kind":"Swap","quantity":1.0,"value_eur":1.0,"time":"2025-01-01T00:00:00Z"}]"#, "TransactionKind inconnu"),
            (r#"[{"kind":"Buy","quantity":1.0,"value_eur":1.0,"time":"2025-01-01T00:00:00Z"}]"#, "asset"),
            (r#"[{"asset":"BTC","quantity":1.0,"value_eur":1.0,"time":"2025-01-01T00:00:00Z"}]"#, "kind"),
            (r#"[{"asset":"BTC","kind":"Buy","value_eur":1.0,"time":"2025-01-01T00:00:00Z"}]"#, "quantity"),
            (r#"[{"asset":"BTC","kind":"Buy","quantity":1.0,"time":"2025-01-01T00:00:00Z"}]"#, "value_eur"),
            (r#"[{"asset":"BTC","kind":"Buy","quantity":1.0,"value_eur":1.0}]"#, "time"),
        ];
        for (i, (json, expected)) in cases.iter().enumerate() {
            let path = write_temp(&format!("invalid-{i}"), json);
            let err = parse_manual(&path).unwrap_err().to_string();
            let _ = fs::remove_file(&path);
            assert!(err.contains(expected), "cas {i} : message inattendu : {err}");
        }
    }

    #[test]
    fn a_file_that_is_not_a_json_array_is_an_error() {
        let path = write_temp("not-json", "not json");
        assert!(parse_manual(&path).is_err());
        let _ = fs::remove_file(&path);
    }
}
