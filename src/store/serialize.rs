//! Portage de src/store/serialize.py : TxStore (conteneur en mémoire) +
//! chargement/sauvegarde JSON. 

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use crate::schema::{Asset, AssetIdentifiers, AssetKind, Platform, Transaction, TransactionKind};
// CORRECTION ICI : on importe bien seed_price_cache et on ajoute le point-virgule


const TX_STORE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AssetMeta {
    name: String,
    kind: AssetKind,
    ref_currency: String,
    identifiers: AssetIdentifiers,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TransactionDto {
    platform: Platform,
    account_label: String,
    kind: TransactionKind,
    asset: String,
    quantity: f64,
    price: Option<f64>,
    value_eur: f64,
    amount: Option<f64>,
    quote_currency: Option<String>,
    time: String,
    external_id: Option<String>,
    remark: Option<String>,
    source_file: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct WalletPayload {
    version: u32,
    updated_at: String,
    assets: HashMap<String, AssetMeta>,
    transactions: Vec<TransactionDto>,
}

#[derive(Debug, Default)]
pub struct TxStore {
    pub assets: HashMap<String, Asset>,
    pub transactions: Vec<Transaction>,
    pub known_external_ids: HashSet<String>,
}

impl TxStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn find_or_create_asset(
        &mut self,
        symbol: &str,
        name: &str,
        kind: AssetKind,
        ref_currency: &str,
        identifiers: AssetIdentifiers,
    ) -> Asset {
        if let Some(existing) = self.assets.get(symbol) {
            return existing.clone();
        }
        let asset = Asset {
            symbol: symbol.to_string(),
            name: name.to_string(),
            kind,
            ref_currency: ref_currency.to_string(),
            identifiers,
        };
        self.assets.insert(symbol.to_string(), asset.clone());
        asset
    }

    pub fn add_transactions(&mut self, new_transactions: Vec<Transaction>) -> usize {
        let mut added = 0;
        for tx in new_transactions {
            if let Some(ext_id) = &tx.external_id {
                if self.known_external_ids.contains(ext_id) {
                    continue;
                }
            }

          
            let local_asset = self.find_or_create_asset(
                &tx.asset.symbol,
                &tx.asset.name,
                tx.asset.kind,
                &tx.asset.ref_currency,
                tx.asset.identifiers.clone(),
            );

            let ext_id = tx.external_id.clone();
            let mut stored_tx = tx;
            stored_tx.asset = local_asset;
            self.transactions.push(stored_tx);

            if let Some(ext_id) = ext_id {
                self.known_external_ids.insert(ext_id);
            }
            added += 1;
        }
        added
    }
}

fn transaction_to_dto(tx: &Transaction) -> TransactionDto {
    TransactionDto {
        platform: tx.platform,
        account_label: tx.account_label.clone(),
        kind: tx.kind,
        asset: tx.asset.symbol.clone(),
        quantity: tx.quantity,
        price: tx.price,
        value_eur: tx.value_eur,
        amount: tx.amount,
        quote_currency: tx.quote_currency.clone(),
        time: tx.time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        external_id: tx.external_id.clone(),
        remark: tx.remark.clone(),
        source_file: tx.source_file.clone(),
    }
}

pub fn save_wallet(tx_store: &TxStore, path: &Path) -> Result<()> {
    let assets_out: HashMap<String, AssetMeta> = tx_store
        .assets
        .iter()
        .map(|(symbol, asset)| {
            (
                symbol.clone(),
                AssetMeta {
                    name: asset.name.clone(),
                    kind: asset.kind,
                    ref_currency: asset.ref_currency.clone(),
                    identifiers: asset.identifiers.clone(),
                },
            )
        })
        .collect();

    let mut transactions_out: Vec<TransactionDto> = tx_store.transactions.iter().map(transaction_to_dto).collect();
    transactions_out.sort_by(|a, b| a.time.cmp(&b.time));

    let payload = WalletPayload {
        version: TX_STORE_VERSION,
        updated_at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        assets: assets_out,
        transactions: transactions_out,
    };

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(&payload)?;
    fs::write(path, json).with_context(|| format!("écriture de {:?}", path))?;
    Ok(())
}