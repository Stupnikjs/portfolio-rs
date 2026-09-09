//! Portage de src/parse/xtb.py -- positions ouvertes/fermées et
//! opérations de cash XTB, lues depuis les exports xlsx.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use anyhow::{anyhow, Context, Result};
use calamine::{open_workbook, Data, Reader, Xlsx};
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use once_cell::sync::Lazy;

use crate::market::prices::{normalize_currency_for_fx, yahoo_historical_price};
use crate::market::tickers::resolve_ticker;
use crate::schema::{Asset, AssetIdentifiers, AssetKind, Platform, Transaction, TransactionKind};

#[derive(Debug, Clone)]
pub struct XtbClosedPosition {
    pub position_id: String,
    pub symbol: String,
    pub volume: f64,
    pub open_time: DateTime<Utc>,
    pub open_price: f64,
    pub close_time: DateTime<Utc>,
    pub close_price: f64,
    pub purchase_value: f64,
    pub sale_value: f64,
    pub source_file: String,
}

impl XtbClosedPosition {
    pub fn to_transactions(&self) -> Vec<Transaction> {
        let currency = infer_currency(&self.symbol);
        let asset = Asset {
            symbol: self.symbol.clone(),
            name: self.symbol.clone(),
            kind: AssetKind::Stock,
            ref_currency: currency,
            identifiers: AssetIdentifiers::default(),
        };
        vec![
            Transaction {
                platform: Platform::Xtb,
                account_label: "XTB".to_string(),
                kind: TransactionKind::Buy,
                asset: asset.clone(),
                quantity: self.volume,
                price: Some(self.open_price),
                amount: Some(self.purchase_value),
                quote_currency: None,
                time: self.open_time,
                value_eur: self.purchase_value,
                external_id: Some(format!("{}-buy", self.position_id)),
                remark: None,
                source_file: self.source_file.clone(),
            },
            Transaction {
                platform: Platform::Xtb,
                account_label: "XTB".to_string(),
                kind: TransactionKind::Sell,
                asset,
                quantity: self.volume,
                price: Some(self.close_price),
                amount: Some(self.sale_value),
                quote_currency: None,
                time: self.close_time,
                value_eur: self.sale_value,
                external_id: Some(format!("{}-sell", self.position_id)),
                remark: None,
                source_file: self.source_file.clone(),
            },
        ]
    }
}

#[derive(Debug, Clone)]
pub struct XtbOpenPosition {
    pub position_id: String,
    pub symbol: String,
    pub volume: f64,
    pub open_time: DateTime<Utc>,
    pub open_price: f64,
    pub purchase_value: f64,
    pub comment: Option<String>,
    pub source_file: String,
}

impl XtbOpenPosition {
    pub fn to_transaction(&self) -> Transaction {
        let currency = infer_currency(&self.symbol);
        let asset = Asset {
            symbol: self.symbol.clone(),
            name: self.symbol.clone(),
            kind: AssetKind::Stock,
            ref_currency: currency,
            identifiers: AssetIdentifiers::default(),
        };
        Transaction {
            platform: Platform::Xtb,
            account_label: "XTB".to_string(),
            kind: TransactionKind::Buy,
            asset,
            quantity: self.volume,
            price: Some(self.open_price),
            amount: Some(self.purchase_value),
            quote_currency: None,
            time: self.open_time,
            value_eur: self.purchase_value,
            external_id: Some(self.position_id.clone()),
            remark: self.comment.clone(),
            source_file: self.source_file.clone(),
        }
    }
}

fn infer_currency(symbol: &str) -> String {
    let suffix = symbol.rsplit('.').next().unwrap_or("");
    match suffix {
        "DE" | "NL" | "FR" | "ES" | "IT" => "EUR",
        "UK" => "GBP",
        "US" => "USD",
        _ => "EUR",
    }
    .to_string()
}

static FX_RATE_CACHE: Lazy<Mutex<HashMap<(String, String), f64>>> = Lazy::new(|| Mutex::new(HashMap::new()));
static TRADING_CURRENCY_CACHE: Lazy<Mutex<HashMap<(String, String), String>>> = Lazy::new(|| Mutex::new(HashMap::new()));

/// Facteur multiplicatif -> EUR à la date donnée, via Yahoo. Best-effort :
/// en cas d'échec réseau/format, renvoie 1.0 (dégradé) plutôt que de
/// lever une erreur qui casserait tout l'import.
fn fx_rate_to_eur(currency: &str, day_str: &str) -> f64 {
    if currency == "EUR" {
        return 1.0;
    }
    let key = (currency.to_string(), day_str.to_string());
    if let Some(cached) = FX_RATE_CACHE.lock().unwrap().get(&key) {
        return *cached;
    }
    let (fx_currency, price_factor) = normalize_currency_for_fx(currency);
    let rate = match yahoo_historical_price(&format!("EUR{fx_currency}=X"), day_str) {
        Ok((fx_price, _)) if fx_price != 0.0 => price_factor / fx_price,
        _ => 1.0,
    };
    FX_RATE_CACHE.lock().unwrap().insert(key, rate);
    rate
}

/// Devise de cotation RÉELLE de `symbol`, lue chez Yahoo -- ne jamais la
/// deviner depuis le suffixe XTB seul (repli sur l'heuristique si la
/// résolution Yahoo échoue).
fn real_trading_currency(symbol: &str, day_str: &str) -> String {
    let key = (symbol.to_string(), day_str.to_string());
    if let Some(cached) = TRADING_CURRENCY_CACHE.lock().unwrap().get(&key) {
        return cached.clone();
    }
    let currency = resolve_ticker(symbol, AssetKind::Stock)
        .and_then(|ticker| yahoo_historical_price(&ticker, day_str).ok())
        .map(|(_, currency)| currency)
        .unwrap_or_else(|| infer_currency(symbol));
    TRADING_CURRENCY_CACHE.lock().unwrap().insert(key, currency.clone());
    currency
}

/// Trouve dynamiquement le nom d'onglet commençant par un préfixe donné,
/// utile pour 'OPEN POSITION <date>' dont le nom exact change à chaque export.
pub fn find_sheet_by_prefix(path: &Path, prefix: &str) -> Result<String> {
    let workbook: Xlsx<_> = open_workbook(path).with_context(|| format!("ouverture de {path:?}"))?;
    workbook
        .sheet_names()
        .iter()
        .find(|name| name.starts_with(prefix))
        .cloned()
        .ok_or_else(|| anyhow!("aucun onglet commençant par '{prefix}'"))
}

fn cell_str(value: &Data) -> String {
    match value {
        Data::Empty => String::new(),
        Data::String(s) => s.trim().to_string(),
        Data::Float(f) => f.to_string(),
        Data::Int(i) => i.to_string(),
        Data::Bool(b) => b.to_string(),
        other => format!("{other:?}").trim().to_string(),
    }
}

fn cell_f64(value: &Data) -> Option<f64> {
    match value {
        Data::Float(f) => Some(*f),
        Data::Int(i) => Some(*i as f64),
        Data::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Excel/xlsx stocke les dates comme un nombre de jours depuis 1899-12-30 ;
/// calamine expose déjà ce nombre via `as_f64` pour les cellules formatées
/// date, donc on repasse par cette base -- équivalent du comportement
/// openpyxl (qui renvoie directement un datetime Python).
fn cell_datetime(value: &Data) -> Result<DateTime<Utc>> {
    match value {
        Data::DateTime(excel_dt) => {
            let naive = excel_dt.as_datetime().ok_or_else(|| anyhow!("date Excel invalide: {excel_dt:?}"))?;
            Ok(Utc.from_utc_datetime(&naive))
        }
        Data::String(s) => {
            let naive = NaiveDateTime::parse_from_str(s.trim(), "%d/%m/%Y %H:%M:%S")
                .with_context(|| format!("format de date inattendu: {s:?}"))?;
            Ok(Utc.from_utc_datetime(&naive))
        }
        other => Err(anyhow!("format de date inattendu: {other:?}")),
    }
}

/// Trouve l'index de la ligne d'en-tête -- celle qui contient
/// `required_col`. Les exports XTB ont un nombre variable de lignes de
/// titre/résumé de compte au-dessus du vrai tableau.
fn find_header_row(rows: &[Vec<Data>], required_col: &str) -> Result<usize> {
    let required_lower = required_col.trim().to_lowercase();
    for (i, row) in rows.iter().enumerate() {
        if row.iter().any(|cell| cell_str(cell).to_lowercase() == required_lower) {
            return Ok(i);
        }
    }
    Err(anyhow!("en-tête introuvable (colonne '{required_col}' non trouvée)"))
}

/// Nom de colonne (normalisé en minuscules) -> index. Insensible à la
/// casse : les exports XTB ont déjà changé la casse de certains en-têtes
/// d'une version à l'autre.
fn column_map(header_row: &[Data]) -> HashMap<String, usize> {
    header_row
        .iter()
        .enumerate()
        .filter_map(|(i, cell)| {
            let s = cell_str(cell);
            if s.is_empty() {
                None
            } else {
                Some((s.to_lowercase(), i))
            }
        })
        .collect()
}

fn col<'a>(row: &'a [Data], col_map: &HashMap<String, usize>, name: &str) -> Result<&'a Data> {
    let idx = col_map
        .get(&name.trim().to_lowercase())
        .ok_or_else(|| anyhow!("colonne '{name}' introuvable. Colonnes disponibles: {:?}", {
            let mut keys: Vec<&String> = col_map.keys().collect();
            keys.sort();
            keys
        }))?;
    row.get(*idx).ok_or_else(|| anyhow!("cellule manquante pour la colonne '{name}'"))
}

fn has_col(col_map: &HashMap<String, usize>, name: &str) -> bool {
    col_map.contains_key(&name.trim().to_lowercase())
}

fn load_rows(path: &Path, sheet_name: &str) -> Result<Vec<Vec<Data>>> {
    let mut workbook: Xlsx<_> = open_workbook(path).with_context(|| format!("ouverture de {path:?}"))?;
    let range = workbook
        .worksheet_range(sheet_name)
        .with_context(|| format!("lecture de l'onglet '{sheet_name}'"))?;
    Ok(range.rows().map(|r| r.to_vec()).collect())
}

/// Parse l'onglet 'Closed Positions' d'un export XTB xlsx.
pub fn parse_closed_positions(path: &Path, sheet_name: &str) -> Result<Vec<XtbClosedPosition>> {
    let source_file = path.display().to_string();
    let rows = load_rows(path, sheet_name)?;
    let header_idx = find_header_row(&rows, "Position ID")?;
    let col_map = column_map(&rows[header_idx]);
    let mut out = Vec::new();

    for row in &rows[header_idx + 1..] {
        let position_id = match cell_f64(col(row, &col_map, "Position ID")?) {
            Some(id) => id,
            None => continue, // ligne "Total" ou vide
        };

        out.push(XtbClosedPosition {
            position_id: (position_id as i64).to_string(),
            symbol: cell_str(col(row, &col_map, "Ticker")?),
            volume: cell_f64(col(row, &col_map, "Volume")?).unwrap_or(0.0),
            open_time: cell_datetime(col(row, &col_map, "Open Time (UTC)")?)?,
            open_price: cell_f64(col(row, &col_map, "Open Price")?).unwrap_or(0.0),
            close_time: cell_datetime(col(row, &col_map, "Close Time (UTC)")?)?,
            close_price: cell_f64(col(row, &col_map, "Close Price")?).unwrap_or(0.0),
            purchase_value: cell_f64(col(row, &col_map, "Purchase Value")?).unwrap_or(0.0),
            sale_value: cell_f64(col(row, &col_map, "Sale Value")?).unwrap_or(0.0),
            source_file: source_file.clone(),
        });
    }

    Ok(out)
}

/// Parse l'onglet 'Open Positions' d'un export XTB xlsx.
///
/// Le tableau contient deux types de lignes : des lignes "résumé" par
/// instrument (colonne Type vide) et des lignes "détail" par position
/// individuelle (Type='BUY'). Seules les secondes sont retenues.
pub fn parse_open_positions(path: &Path, sheet_name: &str) -> Result<Vec<XtbOpenPosition>> {
    let source_file = path.display().to_string();
    let rows = load_rows(path, sheet_name)?;
    let header_idx = find_header_row(&rows, "Instrument/Position")?;
    let col_map = column_map(&rows[header_idx]);
    let mut out = Vec::new();

    for row in &rows[header_idx + 1..] {
        let side = cell_str(col(row, &col_map, "Type")?);
        if side.is_empty() {
            continue;
        }
        let position_id = cell_str(col(row, &col_map, "Instrument/Position")?);
        if position_id.is_empty() {
            continue;
        }

        let symbol = cell_str(col(row, &col_map, "Ticker")?);
        let open_time = cell_datetime(col(row, &col_map, "Open time (UTC)")?)?;
        let open_price = cell_f64(col(row, &col_map, "Open price")?).unwrap_or(0.0);
        let volume = cell_f64(col(row, &col_map, "Volume")?).unwrap_or(0.0);

        // Coût réel d'acquisition (Volume x Open price), converti en EUR à
        // la date d'ouverture -- PAS la colonne "Value" (valeur de marché
        // au moment de l'export) : utiliser celle-ci comme purchase_value
        // faussait silencieusement tout le cost basis FIFO à chaque
        // réimport (le "coût d'achat" devenait la valeur de marché du jour
        // d'export).
        let day_str = open_time.format("%Y-%m-%d").to_string();
        let currency = real_trading_currency(&symbol, &day_str);
        let fx_rate = fx_rate_to_eur(&currency, &day_str);
        let acquisition_cost_eur = volume * open_price * fx_rate;

        out.push(XtbOpenPosition {
            position_id,
            symbol,
            volume,
            open_time,
            open_price,
            purchase_value: acquisition_cost_eur,
            comment: None, // colonne "Comment" absente du nouvel export
            source_file: source_file.clone(),
        });
    }

    Ok(out)
}

// Mapping des types d'opération 'Cash Operations' -> TransactionKind. Si un
// nouveau type apparaît dans un futur export, on renvoie une erreur
// explicite plutôt que de l'ignorer silencieusement.
fn cash_kind_for(op_type: &str) -> Option<TransactionKind> {
    match op_type {
        "Deposit" | "Free funds interest" => Some(TransactionKind::Deposit),
        // jambe cash de l'achat/vente -- le titre est déjà géré par
        // parse_open_positions/parse_closed_positions.
        "Stock purchase" => Some(TransactionKind::Withdraw),
        "Stock sell" => Some(TransactionKind::Deposit),
        "Dividend" => Some(TransactionKind::Deposit),
        "Withholding tax" => Some(TransactionKind::Fee), // retenue à la source sur dividende
        "Tax IFTT" => Some(TransactionKind::Fee),         // taxe française sur transactions financières
        "Fractional shares" => Some(TransactionKind::Deposit), // compensation cash pour rompus d'actions
        _ => None,
    }
}

// Ignoré : mouvement interne entre sous-comptes XTB, pas une entrée/sortie
// réelle de patrimoine (vu sur le principal et sur le PEA).
const SKIP_CASH_TYPES: &[&str] = &["PEA deposit"];

/// Parse l'onglet 'Cash Operations' d'un export XTB xlsx en transactions de
/// cash EUR (dépôts, dividendes, taxes, achats/ventes de titres...).
pub fn parse_cash_operations(path: &Path, sheet_name: &str) -> Result<Vec<Transaction>> {
    let source_file = path.display().to_string();
    let rows = load_rows(path, sheet_name)?;
    let header_idx = find_header_row(&rows, "ID")?;
    let col_map = column_map(&rows[header_idx]);
    let mut out = Vec::new();

    let eur_asset = Asset {
        symbol: "EUR".to_string(),
        name: "EUR".to_string(),
        kind: AssetKind::Cash,
        ref_currency: "EUR".to_string(),
        identifiers: AssetIdentifiers::default(),
    };

    for row in &rows[header_idx + 1..] {
        let op_type = cell_str(col(row, &col_map, "Type")?);
        if op_type.is_empty() || op_type == "Total" {
            continue;
        }
        if SKIP_CASH_TYPES.contains(&op_type.as_str()) {
            continue;
        }

        let kind = cash_kind_for(&op_type).ok_or_else(|| anyhow!("Type d'opération Cash non géré: {op_type:?}"))?;
        let amount = cell_f64(col(row, &col_map, "Amount")?).unwrap_or(0.0);
        let op_id = cell_str(col(row, &col_map, "ID")?);
        let comment = if has_col(&col_map, "Comment") { cell_str(col(row, &col_map, "Comment")?) } else { String::new() };

        out.push(Transaction {
            platform: Platform::Xtb,
            account_label: "XTB".to_string(),
            kind,
            asset: eur_asset.clone(),
            quantity: amount.abs(),
            price: Some(1.0),
            value_eur: amount.abs(),
            amount: Some(amount),
            quote_currency: Some("EUR".to_string()),
            time: cell_datetime(col(row, &col_map, "Time")?)?,
            external_id: Some(format!("xtb-cash-{op_id}")),
            remark: if comment.is_empty() { None } else { Some(comment) },
            source_file: source_file.clone(),
        });
    }

    Ok(out)
}

