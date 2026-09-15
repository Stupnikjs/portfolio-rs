//! Portage de src/store/serialize.py : TxStore (conteneur en mémoire) +
//! chargement/sauvegarde JSON. Format compatible avec l'ancien
//! serialized_tx.json Python (assets en dict par symbole, transactions
//! en liste plate avec champ "asset" = symbole).

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::path::Path;
use std::io::Write;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::schema::{Asset, AssetIdentifiers, AssetKind, Platform, Transaction, TransactionKind};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum TxEvent {
    /// tx complète auto-portée (Asset inclus, pas juste le symbole) :
    /// chaque ligne du log doit pouvoir se relire seule.
    Add { tx: Transaction },
    /// Ne supprime jamais une ligne : marque juste une tx antérieure
    /// comme périmée (correction XTB, édition manuelle...).
    Tombstone { external_id: String, reason: Option<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TxLogRecord {
    recorded_at: String,   // horodatage d'écriture (audit), != tx.time
    batch_id: String,      // uuid généré une fois par run de main.rs
    event: TxEvent,
}

fn append_events(path: &Path, records: &[TxLogRecord]) -> Result<()> {
    if let Some(parent) = path.parent() { fs::create_dir_all(parent)?; }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    for r in records {
        writeln!(file, "{}", serde_json::to_string(r)?)?;
    }
    Ok(())
}

/// Conteneur en mémoire pour l'ensemble des transactions et actifs connus.
/// Equivalent de la classe TxStore côté Python (anciennement Wallet).
#[derive(Debug, Default)]
pub struct TxStore {
    pub assets: HashMap<String, Asset>,
    pub transactions: Vec<Transaction>,
    known_external_ids: HashSet<String>,
}

impl TxStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Retourne l'Asset canonique pour ce symbole, le créant si besoin.
    /// Si un Asset existe déjà, ses métadonnées font foi (dédup, remplace
    /// l'ancien AssetRegistry.find_or_create).
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

    /// Fusionne des transactions fraîchement parsées dans le wallet.
    /// Chaque `tx.asset` (venant d'un parseur, instance locale au run)
    /// est retraduit vers l'Asset canonique via son symbole. Retourne le
    /// nombre de transactions effectivement ajoutées (hors doublons).
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

   pub fn diff_platform(tx_store: &TxStore, platform: Platform, fresh: &[Transaction]) -> (Vec<String>, Vec<Transaction>) {
    let fresh_ids: HashSet<&str> = fresh.iter().filter_map(|t| t.external_id.as_deref()).collect();

    let to_tombstone = tx_store.transactions.iter()
        .filter(|t| t.platform == platform)
        .filter_map(|t| t.external_id.clone())
        .filter(|id| !fresh_ids.contains(id.as_str()))
        .collect();

    let to_add = fresh.iter()
        .filter(|t| t.external_id.as_deref().map_or(true, |id| !tx_store.known_external_ids.contains(id)))
        .cloned()
        .collect();

    (to_tombstone, to_add)
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

 fn dto_to_transaction(dto: TransactionDto, tx_store: &mut TxStore, assets_meta: &HashMap<String, AssetMeta>) -> Result<Transaction> {
    let meta = assets_meta
        .get(&dto.asset)
        .with_context(|| format!("métadonnées manquantes pour l'actif '{}'", dto.asset))?;
    let asset = tx_store.find_or_create_asset(
        &dto.asset,
        &meta.name,
        meta.kind,
        &meta.ref_currency,
        meta.identifiers.clone(),
    );
    let time: DateTime<Utc> = DateTime::parse_from_rfc3339(&dto.time)
        .with_context(|| format!("horodatage invalide: {}", dto.time))?
        .with_timezone(&Utc);

    Ok(Transaction {
        platform: dto.platform,
        account_label: dto.account_label,
        kind: dto.kind,
        asset,
        quantity: dto.quantity,
        price: dto.price,
        value_eur: dto.value_eur,
        amount: dto.amount,
        quote_currency: dto.quote_currency,
        time,
        external_id: dto.external_id,
        remark: dto.remark,
        source_file: dto.source_file,
    })
}

   pub fn load_tx_store(path: &Path) -> Result<TxStore> {
    let mut tx_store = TxStore::new();
    if !path.exists() { return Ok(tx_store); }

    for (i, line) in fs::read_to_string(path)?.lines().enumerate() {
    if line.trim().is_empty() { continue; }
    let record: TxLogRecord = serde_json::from_str(line)
        .with_context(|| format!("ligne {} invalide", i + 1))?;
    match record.event {
        TxEvent::Add { tx } => {
            tx_store.add_transactions(vec![tx]);
        }
        TxEvent::Tombstone { external_id, .. } => {
            tx_store.transactions.retain(|t| t.external_id.as_deref() != Some(external_id.as_str()));
            tx_store.known_external_ids.remove(&external_id);
        }
    }
    }
    Ok(tx_store)
    }

/// Réécrit le serialized_tx.json en entier (pas d'append) -- garantit un
/// fichier toujours cohérent avec l'état en mémoire.
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
    // Ordre chronologique stable -> diffs git lisibles.
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
