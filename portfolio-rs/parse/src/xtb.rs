//! Portage de src/parse/xtb.py -- positions ouvertes/fermées et
//! opérations de cash XTB, lues depuis les exports xlsx.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;

use anyhow::{anyhow, Context, Result};
use calamine::{open_workbook, Data, Reader, Xlsx};
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use once_cell::sync::Lazy;

use pricer::{MarketDataClient, normalize_currency_for_fx};
use model::schema::{Asset, AssetIdentifiers, AssetKind, Platform, Transaction, TransactionKind, CashFlowKind};


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
    pub occurrence: usize,
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
                external_id: Some(self.ext_id("buy")),
                remark: None,
                source_file: self.source_file.clone(),
                cash_flow: None,
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
                external_id: Some(self.ext_id("sell")),
                remark: None,
                source_file: self.source_file.clone(),
                cash_flow: None,
            },
        ]
    }
    fn ext_id(&self, leg: &str) -> String {
        if self.occurrence == 0 {
            format!("{}-{leg}", self.position_id)
        } else {
            format!("{}#{}-{leg}", self.position_id, self.occurrence)
        }
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
            cash_flow: None,
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

fn fx_rate_to_eur(market_client: &MarketDataClient, currency: &str, day_str: &str) -> f64 {
    if currency == "EUR" {
        return 1.0;
    }
    let key = (currency.to_string(), day_str.to_string());
    if let Some(cached) = FX_RATE_CACHE.lock().unwrap().get(&key) {
        return *cached;
    }
    let (fx_currency, price_factor) = normalize_currency_for_fx(currency);
    let rate = match market_client.yahoo_historical_price(&format!("EUR{fx_currency}=X"), day_str) {
        Ok((fx_price, _)) if fx_price != 0.0 => price_factor / fx_price,
        _ => 1.0,
    };
    FX_RATE_CACHE.lock().unwrap().insert(key, rate);
    rate
}

fn real_trading_currency(market_client: &MarketDataClient, symbol: &str, day_str: &str) -> String {
    let key = (symbol.to_string(), day_str.to_string());
    if let Some(cached) = TRADING_CURRENCY_CACHE.lock().unwrap().get(&key) {
        return cached.clone();
    }
    let currency = market_client.resolve_ticker(symbol, AssetKind::Stock.to_market_kind())
        .and_then(|ticker| market_client.yahoo_historical_price(&ticker, day_str).ok())
        .map(|(_, currency)| currency)
        .unwrap_or_else(|| infer_currency(symbol));
    TRADING_CURRENCY_CACHE.lock().unwrap().insert(key, currency.clone());
    currency
}

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

fn find_header_row(rows: &[Vec<Data>], required_col: &str) -> Result<usize> {
    let required_lower = required_col.trim().to_lowercase();
    for (i, row) in rows.iter().enumerate() {
        if row.iter().any(|cell| cell_str(cell).to_lowercase() == required_lower) {
            return Ok(i);
        }
    }
    Err(anyhow!("en-tête introuvable (colonne '{required_col}' non trouvée)"))
}

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

pub fn parse_closed_positions(path: &Path, sheet_name: &str, known_ids: &std::collections::HashSet<String>) -> Result<Vec<XtbClosedPosition>> {
    let source_file = path.display().to_string();
    let rows = load_rows(path, sheet_name)?;
    let header_idx = find_header_row(&rows, "Position ID")?;
    let col_map = column_map(&rows[header_idx]);
    let mut out = Vec::new();

    let mut seen: HashMap<String, usize> = HashMap::new();
    for row in &rows[header_idx + 1..] {
        let position_id = match cell_f64(col(row, &col_map, "Position ID")?) {
            Some(id) => (id as i64).to_string(),
            None => continue,
        };
        let occurrence = {
            let n = seen.entry(position_id.clone()).or_insert(0);
            let current = *n;
            *n += 1;
            current
        };
        let buy_id = format!("{}-buy", position_id);
        let sell_id = format!("{}-sell", position_id);
        if known_ids.contains(&buy_id) && known_ids.contains(&sell_id) {
            continue;
        }
       
        out.push(XtbClosedPosition {
            position_id,
            occurrence,
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

pub fn parse_open_positions(path: &Path, sheet_name: &str, known_ids: &HashSet<String>, market_client: &MarketDataClient) -> Result<Vec<XtbOpenPosition>> {
    let source_file = path.display().to_string();
    let rows = load_rows(path, sheet_name)?;
    let header_idx = find_header_row(&rows, "Instrument/Position")?;
    let col_map = column_map(&rows[header_idx]);
    let mut out = Vec::new();

    for row in &rows[header_idx + 1..] {
        let side = cell_str(col(row, &col_map, "Type")?);
        if side.is_empty() { continue; }
        let position_id = cell_str(col(row, &col_map, "Instrument/Position")?);
        if position_id.is_empty() { continue; }

        if known_ids.contains(&position_id) {
            continue;
        }

        let symbol = cell_str(col(row, &col_map, "Ticker")?);
        let open_time = cell_datetime(col(row, &col_map, "Open time (UTC)")?)?;
        let open_price = cell_f64(col(row, &col_map, "Open price")?).unwrap_or(0.0);
        let volume = cell_f64(col(row, &col_map, "Volume")?).unwrap_or(0.0);

        let day_str = open_time.format("%Y-%m-%d").to_string();
        let currency = real_trading_currency(market_client, &symbol, &day_str);
        let fx_rate = fx_rate_to_eur(market_client, &currency, &day_str);
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

fn cash_kind_for(op_type: &str) -> Option<(TransactionKind, CashFlowKind)> {
    use CashFlowKind::*;
    use TransactionKind as K;
    match op_type {
        // Sur le PEA, les versements portent ce libellé : ce sont des apports externes comme "Deposit".
        "Deposit" | "PEA deposit" => Some((K::Deposit, ExternalDeposit)),
        "Withdrawal" => Some((K::Withdraw, ExternalWithdrawal)), // nom à confirmer sur ton export
        "Free funds interest" | "Dividend" => Some((K::Deposit, Income)),
        "Stock purchase" => Some((K::Withdraw, TradeLeg)),
        "Stock sell" | "Fractional shares" => Some((K::Deposit, TradeLeg)),
        "Withholding tax" | "Tax IFTT" => Some((K::Fee, Tax)),
        _ => None,
    }
}

/// Types d'opérations ignorés à l'import (aucun : le solde doit être la somme de tout le relevé).
const SKIP_CASH_TYPES: &[&str] = &[];

pub fn parse_cash_operations(path: &Path, sheet_name: &str) -> Result<Vec<Transaction>> {
    let source_file = path.display().to_string();
    let rows = load_rows(path, sheet_name)?;
    let header_idx = find_header_row(&rows, "ID")?;
    let col_map = column_map(&rows[header_idx]);
    let mut out = Vec::new();
    // Diagnostic : nombre d'opérations et somme signée par type (y compris les types ignorés).
    let mut by_type: BTreeMap<String, (usize, f64)> = BTreeMap::new();

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
            let amount = cell_f64(col(row, &col_map, "Amount")?).unwrap_or(0.0);
            let e = by_type.entry(op_type.clone()).or_insert((0, 0.0));
            e.0 += 1;
            e.1 += amount;
            continue;
        }

        let (kind, cash_flow) = cash_kind_for(&op_type)
    .ok_or_else(|| anyhow!("Type d'opération Cash non géré: {op_type:?}"))?;
        let amount = cell_f64(col(row, &col_map, "Amount")?).unwrap_or(0.0);
        let e = by_type.entry(op_type.clone()).or_insert((0, 0.0));
        e.0 += 1;
        e.1 += amount;
        let op_id = cell_str(col(row, &col_map, "ID")?);
        let comment = if has_col(&col_map, "Comment") { cell_str(col(row, &col_map, "Comment")?) } else { String::new() };

        out.push(Transaction {
            platform: Platform::Xtb,
            account_label: "XTB".to_string(),
            kind: kind,
            cash_flow: Some(cash_flow),
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

    print_cash_diagnostic(&source_file, &by_type);
    Ok(out)
}

/// Récapitulatif des opérations de cash d'un export : à comparer au solde affiché par XTB.
fn print_cash_diagnostic(source_file: &str, by_type: &BTreeMap<String, (usize, f64)>) {
    println!("  [Cash XTB] {source_file}");
    let (mut kept, mut ignored) = (0.0, 0.0);
    for (op_type, (n, sum)) in by_type {
        let skipped = SKIP_CASH_TYPES.contains(&op_type.as_str());
        if skipped {
            ignored += sum;
        } else {
            kept += sum;
        }
        println!("    {op_type:<22} {n:>5} ops {sum:>12.2} EUR{}", if skipped { "   <- IGNORÉ à l'import" } else { "" });
    }
    println!("    solde reconstruit {kept:>12.2} EUR   (montants ignorés : {ignored:.2} EUR)");
}

pub fn parse_xtb_file(path: &Path, market_client: &MarketDataClient) -> Vec<Transaction> {
    let mut out = Vec::new();
    if !path.exists() {
        println!("[Omis] Fichier introuvable : {path:?}");
        return out;
    }

    println!("Lecture XTB : {path:?}");
    let empty_ids = HashSet::new();

    match find_sheet_by_prefix(path, "Closed Position") {
        Ok(sheet) => match parse_closed_positions(path, &sheet, &empty_ids) {
            Ok(positions) => {
                for pos in positions {
                    out.extend(pos.to_transactions());
                }
            }
            Err(e) => println!("  [Erreur XTB Closed] {e}"),
        },
        Err(e) => println!("  [Erreur XTB Closed] {e}"),
    }

    match find_sheet_by_prefix(path, "Open Position") {
        Ok(sheet) => match parse_open_positions(path, &sheet, &empty_ids, market_client) {
            Ok(positions) => out.extend(positions.iter().map(|p| p.to_transaction())),
            Err(e) => println!("  [Erreur XTB Open] {e}"),
        },
        Err(e) => println!("  [Erreur XTB Open] {e}"),
    }

    match find_sheet_by_prefix(path, "Cash") {
        Ok(sheet) => match parse_cash_operations(path, &sheet) {
            Ok(mut tx) => out.append(&mut tx),
            Err(e) => println!("  [Erreur XTB Cash] {e}"),
        },
        Err(e) => println!("  [Erreur XTB Cash] {e}"),
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use rust_xlsxwriter::{ExcelDateTime, Format, Workbook};

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
    }

    /// Chemin de fichier temporaire unique par test (pas de dépendance `tempfile`).
    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("importers-xtb-{}-{name}.xlsx", std::process::id()))
    }

    enum When {
        Text(&'static str),
        Excel(u16, u8, u8, u16, u8, u8),
    }

    /// Écrit un export XTB minimal : une ligne de préambule, l'en-tête, puis les opérations.
    fn write_cash_workbook(path: &Path, sheet: &str, ops: &[(&str, &str, f64, When, &str)]) {
        let mut workbook = Workbook::new();
        let date_format = Format::new().set_num_format("dd/mm/yyyy hh:mm:ss");
        let ws = workbook.add_worksheet();
        ws.set_name(sheet).unwrap();
        ws.write_string(0, 0, "Account").unwrap();
        ws.write_string(0, 1, "123456").unwrap();
        for (c, h) in ["ID", "Type", "Time", "Amount", "Comment"].iter().enumerate() {
            ws.write_string(2, c as u16, *h).unwrap();
        }
        for (i, (id, kind, amount, when, comment)) in ops.iter().enumerate() {
            let row = 3 + i as u32;
            ws.write_string(row, 0, *id).unwrap();
            ws.write_string(row, 1, *kind).unwrap();
            match when {
                When::Text(s) => {
                    ws.write_string(row, 2, *s).unwrap();
                }
                When::Excel(y, mo, d, h, mi, s) => {
                    let dt = ExcelDateTime::from_ymd(*y, *mo, *d).unwrap().and_hms(*h, *mi, *s).unwrap();
                    ws.write_datetime_with_format(row, 2, &dt, &date_format).unwrap();
                }
            }
            ws.write_number(row, 3, *amount).unwrap();
            if !comment.is_empty() {
                ws.write_string(row, 4, *comment).unwrap();
            }
        }
        workbook.save(path).unwrap();
    }

    #[test]
    fn cash_operations_are_read_from_a_real_xlsx_file() {
        let path = temp_path("cash-ok");
        write_cash_workbook(
            &path,
            "Cash Operations 2025",
            &[
                ("1001", "Deposit", 1000.0, When::Text("01/03/2025 10:00:00"), "virement"),
                ("1002", "Dividend", 12.5, When::Excel(2025, 3, 15, 9, 30, 0), ""),
                ("1003", "Stock purchase", -500.0, When::Text("16/03/2025 11:00:00"), ""),
                ("1004", "Withholding tax", -3.2, When::Text("16/03/2025 11:05:00"), ""),
                ("1005", "PEA deposit", 200.0, When::Text("17/03/2025 08:00:00"), ""), // versement PEA : apport externe
                ("", "Total", 0.0, When::Text("17/03/2025 08:00:00"), ""),              // ligne de total ignorée
            ],
        );

        let sheet = find_sheet_by_prefix(&path, "Cash").unwrap();
        assert_eq!(sheet, "Cash Operations 2025");
        let tx = parse_cash_operations(&path, &sheet).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(tx.len(), 5);
        let kinds: Vec<_> = tx.iter().map(|t| t.kind).collect();
        assert_eq!(
            kinds,
            [TransactionKind::Deposit, TransactionKind::Deposit, TransactionKind::Withdraw, TransactionKind::Fee, TransactionKind::Deposit]
        );
        let flows: Vec<_> = tx.iter().map(|t| t.cash_flow).collect();
        assert_eq!(
            flows,
            [
                Some(CashFlowKind::ExternalDeposit),
                Some(CashFlowKind::Income),
                Some(CashFlowKind::TradeLeg),
                Some(CashFlowKind::Tax),
                Some(CashFlowKind::ExternalDeposit)
            ]
        );

        // quantité et valeur toujours positives, montant signé conservé
        assert_eq!(tx[2].quantity, 500.0);
        assert_eq!(tx[2].value_eur, 500.0);
        assert_eq!(tx[2].amount, Some(-500.0));
        assert_eq!(tx[3].value_eur, 3.2);

        assert_eq!(tx[0].external_id.as_deref(), Some("xtb-cash-1001"));
        assert_eq!(tx[0].time, utc(2025, 3, 1, 10, 0, 0));
        assert_eq!(tx[1].time, utc(2025, 3, 15, 9, 30, 0)); // cellule date Excel (calamine `dates`)
        assert_eq!(tx[0].remark.as_deref(), Some("virement"));
        assert_eq!(tx[1].remark, None);

        assert!(tx.iter().all(|t| t.platform == Platform::Xtb && t.asset.symbol == "EUR" && t.asset.kind == AssetKind::Cash));
    }

    #[test]
    fn an_unknown_cash_operation_type_fails_the_whole_sheet_with_a_clear_message() {
        let path = temp_path("cash-unknown");
        write_cash_workbook(
            &path,
            "Cash Operations",
            &[
                ("1", "Deposit", 100.0, When::Text("01/03/2025 10:00:00"), ""),
                ("2", "Mystery fee", -1.0, When::Text("02/03/2025 10:00:00"), ""),
            ],
        );
        let err = parse_cash_operations(&path, "Cash Operations").unwrap_err().to_string();
        let _ = std::fs::remove_file(&path);
        assert!(err.contains("non géré") && err.contains("Mystery fee"), "message inattendu : {err}");
    }

    #[test]
    fn find_sheet_by_prefix_reports_a_missing_sheet() {
        let path = temp_path("sheet-missing");
        write_cash_workbook(&path, "Cash Operations", &[]);
        let err = find_sheet_by_prefix(&path, "Open Position").unwrap_err().to_string();
        let _ = std::fs::remove_file(&path);
        assert!(err.contains("Open Position"), "message inattendu : {err}");
    }

    #[test]
    fn cash_operation_types_map_to_a_kind_and_a_cash_flow() {
        use CashFlowKind::*;
        use TransactionKind as K;
        assert_eq!(cash_kind_for("Deposit"), Some((K::Deposit, ExternalDeposit)));
        assert_eq!(cash_kind_for("Withdrawal"), Some((K::Withdraw, ExternalWithdrawal)));
        assert_eq!(cash_kind_for("Free funds interest"), Some((K::Deposit, Income)));
        assert_eq!(cash_kind_for("Dividend"), Some((K::Deposit, Income)));
        assert_eq!(cash_kind_for("Stock purchase"), Some((K::Withdraw, TradeLeg)));
        assert_eq!(cash_kind_for("Stock sell"), Some((K::Deposit, TradeLeg)));
        assert_eq!(cash_kind_for("Fractional shares"), Some((K::Deposit, TradeLeg)));
        assert_eq!(cash_kind_for("Withholding tax"), Some((K::Fee, Tax)));
        assert_eq!(cash_kind_for("Tax IFTT"), Some((K::Fee, Tax)));
        assert_eq!(cash_kind_for("PEA deposit"), Some((K::Deposit, ExternalDeposit)));
        assert!(SKIP_CASH_TYPES.is_empty());
        assert_eq!(cash_kind_for("anything else"), None);
    }

    #[test]
    fn cell_helpers_convert_the_cell_types_calamine_produces() {
        assert_eq!(cell_str(&Data::Empty), "");
        assert_eq!(cell_str(&Data::String("  Deposit ".into())), "Deposit");
        assert_eq!(cell_str(&Data::Int(42)), "42");
        assert_eq!(cell_f64(&Data::Float(1.5)), Some(1.5));
        assert_eq!(cell_f64(&Data::Int(2)), Some(2.0));
        assert_eq!(cell_f64(&Data::String(" 3.25 ".into())), Some(3.25));
        assert_eq!(cell_f64(&Data::String("abc".into())), None);
        assert_eq!(cell_f64(&Data::Empty), None);
        assert_eq!(cell_datetime(&Data::String("01/03/2025 10:00:00".into())).unwrap(), utc(2025, 3, 1, 10, 0, 0));
        assert!(cell_datetime(&Data::String("2025-03-01".into())).is_err());
        assert!(cell_datetime(&Data::Empty).is_err());
    }

    #[test]
    fn header_row_and_columns_are_found_case_insensitively() {
        let rows = vec![
            vec![Data::String("Account".into()), Data::String("123".into())],
            vec![Data::String("id".into()), Data::String("TYPE".into()), Data::Empty, Data::String("Amount".into())],
        ];
        assert_eq!(find_header_row(&rows, "ID").unwrap(), 1);
        assert!(find_header_row(&rows, "Symbol").is_err());

        let map = column_map(&rows[1]);
        assert_eq!(map.get("type"), Some(&1));
        assert!(!map.contains_key("")); // les en-têtes vides sont ignorés
        let data_row = vec![Data::String("7".into()), Data::String("Deposit".into()), Data::Empty, Data::Float(9.0)];
        assert_eq!(cell_str(col(&data_row, &map, " Type ").unwrap()), "Deposit");
        let err = col(&data_row, &map, "Nope").unwrap_err().to_string();
        assert!(err.contains("Nope") && err.contains("amount"), "message inattendu : {err}");
        assert!(has_col(&map, "AMOUNT") && !has_col(&map, "Comment"));
    }

    #[test]
    fn the_trading_currency_is_inferred_from_the_ticker_suffix() {
        assert_eq!(infer_currency("SAN.FR"), "EUR");
        assert_eq!(infer_currency("SAP.DE"), "EUR");
        assert_eq!(infer_currency("BP.UK"), "GBP");
        assert_eq!(infer_currency("AAPL.US"), "USD");
        assert_eq!(infer_currency("PLAIN"), "EUR");
    }
}