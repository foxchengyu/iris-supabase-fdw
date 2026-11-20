#![allow(warnings)]
mod bindings;

use serde::Deserialize;
use serde_json::Value as JsonValue;

use bindings::{
    exports::supabase::wrappers::routines::Guest,
    supabase::wrappers::{
        http, time,
        types::{
            Cell, Context, FdwResult, ImportForeignSchemaStmt, ImportSchemaType, OptionsType, Row,
            TypeOid, Value,
        },
        utils,
    },
};

// ── Gateway response types ────────────────────────────────────────────

#[derive(Deserialize)]
struct QueryResponse {
    rows: Vec<JsonValue>,
    row_count: usize,
}

#[derive(Deserialize)]
struct ExecResponse {
    rows_affected: usize,
}

#[derive(Deserialize)]
struct ErrorResponse {
    error: String,
}

// Gateway GET /schema/:name response
#[derive(Deserialize)]
struct SchemaResponse {
    tables: Vec<SchemaTable>,
}

#[derive(Deserialize)]
struct SchemaTable {
    name: String,
    columns: Vec<SchemaColumn>,
}

#[derive(Deserialize)]
struct SchemaColumn {
    name: String,
    #[serde(rename = "type")]
    col_type: String,
}

// ── FDW struct ────────────────────────────────────────────────────────

struct IrisFdw {
    /// IRIS gateway base URL, e.g. https://iris.example.com/iris-fdw/v1
    api_url: String,
    /// "Basic <base64>" authorization header value
    auth_header: String,
    /// Cached result rows from the last /query call
    cached_rows: Vec<JsonValue>,
    /// Current position in cached_rows
    row_idx: usize,
    /// Total rows available (may be > cached_rows.len() in paged mode)
    total_row_count: usize,
    /// Offset for the next page fetch
    page_offset: usize,
    /// Max rows per page
    batch_size: usize,
    /// Whether we are in paged mode
    paged: bool,
    /// Debug mode – if true, log generated SQL
    debug: bool,
}

static mut INSTANCE: *mut IrisFdw = std::ptr::null_mut::<IrisFdw>();

// ── Helper: escape string for JSON body ───────────────────────────────

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            _ => out.push(c),
        }
    }
    out
}

// ── Helper: escape string for SQL literal (double single quotes) ──────

fn sql_escape(s: &str) -> String {
    s.replace('\'', "''")
}

// ── Helper: map IRIS SQL display type to a Postgres column type ───────

fn iris_type_to_pg(iris_type: &str) -> &'static str {
    let upper = iris_type.to_uppercase();
    let base = upper.split('(').next().unwrap_or("").trim();
    match base {
        "INTEGER" => "integer",
        "BIGINT" => "bigint",
        "SMALLINT" | "TINYINT" => "smallint",
        "DOUBLE" | "FLOAT" | "REAL" => "double precision",
        "DECIMAL" | "NUMERIC" => "numeric",
        "DATE" => "date",
        "TIMESTAMP" | "DATETIME" => "timestamp",
        // TIME has no Cell counterpart in the host; read it as text
        "TIME" => "text",
        "BOOLEAN" | "BIT" => "boolean",
        _ => "text",
    }
}

// ── Helper: build Basic Auth header ───────────────────────────────────

fn build_auth_header(username: &str, password: &str) -> String {
    let creds = format!("{}:{}", username, password);
    // Simple base64 encoding using a lookup table
    let encoded = base64_encode(&creds);
    format!("Basic {}", encoded)
}

fn base64_encode(input: &str) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = input.as_bytes();
    let mut result = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let b0 = bytes[i];
        let b1 = if i + 1 < bytes.len() { bytes[i + 1] } else { 0 };
        let b2 = if i + 2 < bytes.len() { bytes[i + 2] } else { 0 };
        let triple = ((b0 as u32) << 16) | ((b1 as u32) << 8) | (b2 as u32);
        result.push(CHARS[((triple >> 18) & 0x3f) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3f) as usize] as char);
        if i + 1 < bytes.len() {
            result.push(CHARS[((triple >> 6) & 0x3f) as usize] as char);
        } else {
            result.push('=');
        }
        if i + 2 < bytes.len() {
            result.push(CHARS[(triple & 0x3f) as usize] as char);
        } else {
            result.push('=');
        }
        i += 3;
    }
    result
}

// ── Helper: HTTP POST with JSON body ──────────────────────────────────

fn post_json(url: &str, auth_header: &str, body: &str) -> Result<JsonValue, String> {
    let headers: Vec<(String, String)> = vec![
        ("Content-Type".to_owned(), "application/json".to_owned()),
        ("Authorization".to_owned(), auth_header.to_owned()),
        ("User-Agent".to_owned(), "iris-fdw/0.1.0".to_owned()),
    ];

    let req = http::Request {
        method: http::Method::Post,
        url: url.to_owned(),
        headers,
        body: body.to_owned(),
    };

    let resp = http::post(&req).map_err(|e| format!("HTTP POST failed: {}", e))?;

    if resp.status_code >= 400 {
        return Err(format!(
            "IRIS gateway returned HTTP {}: {}",
            resp.status_code, resp.body
        ));
    }

    let json: JsonValue =
        serde_json::from_str(&resp.body).map_err(|e| format!("JSON parse error: {}", e))?;

    // Gateway-level error (e.g. SQL error, permission denied)
    if let Some(err) = json.get("error").and_then(|e| e.as_str()) {
        return Err(format!("IRIS gateway error: {}", err));
    }

    Ok(json)
}

// ── Helper: HTTP GET returning parsed JSON ─────────────────────────────

fn get_json(url: &str, auth_header: &str) -> Result<JsonValue, String> {
    let headers: Vec<(String, String)> = vec![
        ("Accept".to_owned(), "application/json".to_owned()),
        ("Authorization".to_owned(), auth_header.to_owned()),
        ("User-Agent".to_owned(), "iris-fdw/0.1.0".to_owned()),
    ];

    let req = http::Request {
        method: http::Method::Get,
        url: url.to_owned(),
        headers,
        body: String::new(),
    };

    let resp = http::get(&req).map_err(|e| format!("HTTP GET failed: {}", e))?;

    if resp.status_code >= 400 {
        return Err(format!(
            "IRIS gateway returned HTTP {}: {}",
            resp.status_code, resp.body
        ));
    }

    let json: JsonValue =
        serde_json::from_str(&resp.body).map_err(|e| format!("JSON parse error: {}", e))?;

    if let Some(err) = json.get("error").and_then(|e| e.as_str()) {
        return Err(format!("IRIS gateway error: {}", err));
    }

    Ok(json)
}

// ── Helper: translate Postgres qual operator to IRIS SQL ──────────────

fn translate_operator(pg_op: &str) -> Result<&str, String> {
    match pg_op {
        "=" => Ok("="),
        "<>" | "!=" => Ok("<>"),
        "<" => Ok("<"),
        "<=" => Ok("<="),
        ">" => Ok(">"),
        ">=" => Ok(">="),
        "~~" | "like" => Ok("LIKE"),
        "!~~" | "not like" => Ok("NOT LIKE"),
        "~~*" | "ilike" => Ok("LIKE"), // fallback, postgres re-checks
        "!~~*" | "not ilike" => Ok("NOT LIKE"),
        "IS NULL" | "is null" => Ok("IS NULL"),
        "IS NOT NULL" | "is not null" => Ok("IS NOT NULL"),
        _ => Err(format!("operator '{}' not supported for pushdown", pg_op)),
    }
}

// ── Helper: format a cell value for SQL ────────────────────────────────

fn cell_to_sql(val: &Value) -> Result<String, String> {
    match val {
        Value::Cell(cell) => match cell {
            Cell::Bool(b) => Ok(if *b {
                "TRUE".to_string()
            } else {
                "FALSE".to_string()
            }),
            Cell::I8(n) => Ok(n.to_string()),
            Cell::I16(n) => Ok(n.to_string()),
            Cell::I32(n) => Ok(n.to_string()),
            Cell::I64(n) => Ok(n.to_string()),
            Cell::F32(n) => Ok(n.to_string()),
            Cell::F64(n) | Cell::Numeric(n) => Ok(n.to_string()),
            Cell::String(s) => Ok(format!("'{}'", sql_escape(s))),
            Cell::Date(secs) => {
                // secs since epoch, convert to 'YYYY-MM-DD'
                let days = secs / 86400;
                let (y, m, d) = days_to_date(days);
                Ok(format!("'{:04}-{:02}-{:02}'", y, m, d))
            }
            Cell::Timestamp(usecs) | Cell::Timestamptz(usecs) => {
                let secs = usecs / 1_000_000;
                let days = secs / 86400;
                let day_secs = secs % 86400;
                let (y, m, d) = days_to_date(days);
                let h = day_secs / 3600;
                let min = (day_secs % 3600) / 60;
                let s = day_secs % 60;
                let us = usecs % 1_000_000;
                Ok(format!(
                    "'{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:06}'",
                    y, m, d, h, min, s, us
                ))
            }
            Cell::Json(s) => Ok(format!("'{}'", sql_escape(s))),
            // UUID and any other type are passed through as quoted literals
            Cell::Uuid(s) | Cell::Other(s) => Ok(format!("'{}'", sql_escape(s))),
        },
        Value::Array(items) => {
            let parts: Vec<String> = items
                .iter()
                .map(|c| cell_to_sql(&Value::Cell(c.clone())))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(format!("({})", parts.join(", ")))
        }
    }
}

fn days_to_date(unix_days: i64) -> (i64, i64, i64) {
    // Convert days since Unix epoch (1970-01-01) to (year, month, day)
    let mut y = 1970i64;
    let mut remaining = unix_days;
    loop {
        let year_days = if is_leap(y) { 366 } else { 365 };
        if remaining < year_days {
            break;
        }
        remaining -= year_days;
        y += 1;
    }
    let month_days: [i64; 12] = if is_leap(y) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut m = 1i64;
    for &md in month_days.iter() {
        if remaining < md {
            break;
        }
        remaining -= md;
        m += 1;
    }
    (y, m, remaining + 1)
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0)
}

// ── FDW lifecycle ─────────────────────────────────────────────────────

impl IrisFdw {
    fn init_instance() {
        let instance = IrisFdw {
            api_url: String::new(),
            auth_header: String::new(),
            cached_rows: Vec::new(),
            row_idx: 0,
            total_row_count: 0,
            page_offset: 0,
            batch_size: 100,
            paged: false,
            debug: false,
        };
        unsafe {
            INSTANCE = Box::leak(Box::new(instance));
        }
    }

    fn this_mut() -> &'static mut Self {
        unsafe { &mut (*INSTANCE) }
    }
}

// ── Guest trait implementation ────────────────────────────────────────

impl Guest for IrisFdw {
    fn host_version_requirement() -> String {
        "^0.1.0".to_string()
    }

    fn init(ctx: &Context) -> FdwResult {
        Self::init_instance();
        let this = Self::this_mut();

        let opts = ctx.get_options(OptionsType::Server);
        this.api_url = opts.require_or("api_url", "http://localhost:52773/iris-fdw/v1");

        // Read credentials
        let username = opts.require_or("username", "_SYSTEM");
        let password = if let Some(secret_id) = opts.get("password_id") {
            utils::get_vault_secret(&secret_id).unwrap_or_else(|| "SYS".to_string())
        } else {
            opts.require_or("password", "SYS")
        };

        this.auth_header = build_auth_header(&username, &password);
        this.debug = opts.get("debug").map(|v| v == "true").unwrap_or(false);

        if let Some(bs) = opts.get("batch_size") {
            if let Ok(n) = bs.parse::<usize>() {
                this.batch_size = n;
            }
        }

        utils::report_info(&format!(
            "iris_fdw: connected to IRIS gateway at {}",
            this.api_url
        ));

        Ok(())
    }

    // ── Scan (SELECT) ─────────────────────────────────────────────

    fn begin_scan(ctx: &Context) -> FdwResult {
        let this = Self::this_mut();

        let table_opts = ctx.get_options(OptionsType::Table);
        let object = table_opts.require("object")?;

        // Build SELECT column list from target columns
        let columns = ctx.get_columns();
        let col_names: Vec<String> = columns.iter().map(|c| c.name()).collect();
        let select_list = if col_names.is_empty() {
            "*".to_string()
        } else {
            col_names
                .iter()
                .map(|n| format!("\"{}\"", n))
                .collect::<Vec<_>>()
                .join(", ")
        };

        // Build WHERE clause from pushdown quals
        let quals = ctx.get_quals();
        let mut where_parts: Vec<String> = Vec::new();

        for qual in quals.iter() {
            let field = qual.field();
            let operator = qual.operator();
            let use_or = qual.use_or();

            if let Ok(sql_op) = translate_operator(&operator) {
                // IS NULL and IS NOT NULL don't have a value
                let val_sql = if sql_op == "IS NULL" || sql_op == "IS NOT NULL" {
                    format!("\"{}\" {}", field, sql_op)
                } else {
                    format!("\"{}\" {} {}", field, sql_op, cell_to_sql(&qual.value())?)
                };

                if use_or && !where_parts.is_empty() {
                    let last = where_parts.pop().unwrap();
                    where_parts.push(format!("({} OR {})", last, val_sql));
                } else {
                    where_parts.push(val_sql);
                }
            }
            // If operator not translatable, Postgres will re-check locally
        }

        // Build ORDER BY from sorts
        let sorts = ctx.get_sorts();
        let order_parts: Vec<String> = sorts
            .iter()
            .map(|s| {
                let dir = if s.reversed() { "DESC" } else { "ASC" };
                format!("\"{}\" {}", s.field(), dir)
            })
            .collect();

        // Check for limit/offset
        let limit = ctx.get_limit();
        let (ir_count, ir_offset) = if this.paged {
            (Some(this.batch_size as i64), Some(this.page_offset as i64))
        } else if let Some(ref lim) = limit {
            (Some(lim.count()), Some(lim.offset()))
        } else {
            (Some(this.batch_size as i64), None)
        };

        // Assemble SQL
        let mut sql = format!("SELECT {} FROM {}", select_list, object);

        if !where_parts.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&where_parts.join(" AND "));
        }

        if !order_parts.is_empty() {
            sql.push_str(" ORDER BY ");
            sql.push_str(&order_parts.join(", "));
        }

        // Use IRIS TOP clause for row limiting
        if let Some(count) = ir_count {
            if count > 0 && count <= 50000 {
                // Replace "SELECT" or "SELECT DISTINCT" with "SELECT TOP n"
                if let Some(rest) = sql.strip_prefix("SELECT ") {
                    sql = format!("SELECT TOP {} {}", count, rest);
                }
            }
        }

        // Note: IRIS doesn't use OFFSET directly in TOP queries.
        // For paging, we use TOP n with skip in the WHERE clause (by PK)
        // or the gateway handles offset in a subquery.
        // For simplicity, we pass offset to the gateway as a separate parameter.

        if this.debug {
            utils::report_info(&format!("iris_fdw: SQL = {}", sql));
        }

        // Build JSON request body
        let request_body = format!(
            r#"{{"query":"{}","parameters":[],"offset":{},"max_rows":{}}}"#,
            json_escape(&sql),
            ir_offset.unwrap_or(0),
            ir_count.unwrap_or(this.batch_size as i64)
        );

        // POST to gateway
        let url = format!("{}/query", this.api_url);
        let json = post_json(&url, &this.auth_header, &request_body)?;

        // Parse response
        let response: QueryResponse = serde_json::from_value(json.clone())
            .map_err(|e| format!("Failed to parse query response: {}", e))?;

        if this.debug {
            utils::report_info(&format!(
                "iris_fdw: got {} rows (total: {})",
                response.rows.len(),
                response.row_count
            ));
        }

        this.cached_rows = response.rows;
        this.row_idx = 0;
        this.total_row_count = response.row_count;
        this.page_offset += this.cached_rows.len();

        Ok(())
    }

    fn iter_scan(ctx: &Context, row: &Row) -> Result<Option<u32>, String> {
        let this = Self::this_mut();

        if this.row_idx >= this.cached_rows.len() {
            return Ok(None);
        }

        let src_row = &this.cached_rows[this.row_idx];

        // Push cells in foreign table column order.
        // Look up each target column by name in the JSON object
        // (case-insensitive match to handle IRIS uppercase conventions).
        let columns = ctx.get_columns();
        if let Some(obj) = src_row.as_object() {
            for tgt_col in columns.iter() {
                let tgt_col_name = tgt_col.name();
                // Try exact match first, then case-insensitive
                let val = obj
                    .get(&tgt_col_name)
                    .or_else(|| {
                        let lower = tgt_col_name.to_lowercase();
                        obj.iter().find_map(|(k, v)| {
                            if k.to_lowercase() == lower {
                                Some(v)
                            } else {
                                None
                            }
                        })
                    });
                let cell = match val {
                    Some(JsonValue::Null) => None, // explicit JSON null -> SQL NULL
                    Some(v) => Some(json_to_cell(v, &tgt_col.type_oid())),
                    None => None, // column not in JSON, push NULL
                };
                row.push(cell.as_ref());
            }
        } else {
            // Single value or unexpected format
            for tgt_col in columns.iter() {
                let cell = match src_row {
                    JsonValue::Null => None,
                    v => Some(json_to_cell(v, &tgt_col.type_oid())),
                };
                row.push(cell.as_ref());
            }
        }

        this.row_idx += 1;

        Ok(Some(0))
    }

    fn re_scan(_ctx: &Context) -> FdwResult {
        let this = Self::this_mut();
        this.row_idx = 0;
        Ok(())
    }

    fn end_scan(_ctx: &Context) -> FdwResult {
        let this = Self::this_mut();
        this.cached_rows.clear();
        this.row_idx = 0;
        this.page_offset = 0;
        this.paged = false;
        Ok(())
    }

    // ── Modify (INSERT / UPDATE / DELETE) ──────────────────────────

    fn begin_modify(_ctx: &Context) -> FdwResult {
        Ok(())
    }

    fn insert(ctx: &Context, row: &Row) -> FdwResult {
        let this = Self::this_mut();
        let table_opts = ctx.get_options(OptionsType::Table);
        let object = table_opts.require("object")?;

        let columns = ctx.get_columns();
        let cols: Vec<String> = columns.iter().map(|c| c.name()).collect();

        let cells = row.cells();
        let vals: Vec<String> = cells
            .iter()
            .map(|c| match c {
                Some(cell) => cell_to_sql(&Value::Cell(cell.clone())),
                None => Ok("NULL".to_string()),
            })
            .collect::<Result<Vec<_>, _>>()?;

        let sql = format!(
            r#"INSERT INTO "{}" ("{}") VALUES ({})"#,
            object,
            cols.join(r#"", ""#),
            vals.join(", ")
        );

        if this.debug {
            utils::report_info(&format!("iris_fdw: INSERT SQL = {}", sql));
        }

        let request_body = format!(r#"{{"query":"{}","parameters":[]}}"#, json_escape(&sql));
        let url = format!("{}/exec", this.api_url);
        let json = post_json(&url, &this.auth_header, &request_body)?;

        let _response: ExecResponse = serde_json::from_value(json)
            .map_err(|e| format!("Failed to parse exec response: {}", e))?;

        Ok(())
    }

    fn update(ctx: &Context, _rowid: Cell, new_row: &Row) -> FdwResult {
        let this = Self::this_mut();
        let table_opts = ctx.get_options(OptionsType::Table);
        let object = table_opts.require("object")?;

        let columns = ctx.get_columns();
        let cells = new_row.cells();

        let set_parts: Vec<String> = columns
            .iter()
            .enumerate()
            .map(|(i, c)| match cells.get(i) {
                Some(Some(cell)) => {
                    format!("\"{}\" = {}", c.name(), cell_to_sql(&Value::Cell(cell.clone())).unwrap_or("NULL".to_string()))
                }
                _ => format!("\"{}\" = NULL", c.name()),
            })
            .collect();

        // Get quals for WHERE clause
        let quals = ctx.get_quals();
        let where_parts: Vec<String> = quals
            .iter()
            .filter_map(|q| {
                translate_operator(&q.operator()).ok().map(|op| {
                    format!(
                        "\"{}\" {} {}",
                        q.field(),
                        op,
                        cell_to_sql(&q.value()).unwrap_or("NULL".to_string())
                    )
                })
            })
            .collect();

        let sql = if where_parts.is_empty() {
            // Without a WHERE, we'd update all rows – refuse for safety
            return Err("UPDATE requires a WHERE condition for safety".to_string());
        } else {
            format!(
                r#"UPDATE "{}" SET {} WHERE {}"#,
                object,
                set_parts.join(", "),
                where_parts.join(" AND ")
            )
        };

        if this.debug {
            utils::report_info(&format!("iris_fdw: UPDATE SQL = {}", sql));
        }

        let request_body = format!(r#"{{"query":"{}","parameters":[]}}"#, json_escape(&sql));
        let url = format!("{}/exec", this.api_url);
        post_json(&url, &this.auth_header, &request_body)?;

        Ok(())
    }

    fn delete(ctx: &Context, _rowid: Cell) -> FdwResult {
        let this = Self::this_mut();
        let table_opts = ctx.get_options(OptionsType::Table);
        let object = table_opts.require("object")?;

        // Get quals for WHERE clause
        let quals = ctx.get_quals();
        let where_parts: Vec<String> = quals
            .iter()
            .filter_map(|q| {
                translate_operator(&q.operator()).ok().map(|op| {
                    format!(
                        "\"{}\" {} {}",
                        q.field(),
                        op,
                        cell_to_sql(&q.value()).unwrap_or("NULL".to_string())
                    )
                })
            })
            .collect();

        let sql = if where_parts.is_empty() {
            return Err("DELETE requires a WHERE condition for safety".to_string());
        } else {
            format!(r#"DELETE FROM "{}" WHERE {}"#, object, where_parts.join(" AND "))
        };

        if this.debug {
            utils::report_info(&format!("iris_fdw: DELETE SQL = {}", sql));
        }

        let request_body = format!(r#"{{"query":"{}","parameters":[]}}"#, json_escape(&sql));
        let url = format!("{}/exec", this.api_url);
        post_json(&url, &this.auth_header, &request_body)?;

        Ok(())
    }

    fn end_modify(_ctx: &Context) -> FdwResult {
        Ok(())
    }

    // ── IMPORT FOREIGN SCHEMA support ─────────────────────────────

    fn import_foreign_schema(
        _ctx: &Context,
        stmt: ImportForeignSchemaStmt,
    ) -> Result<Vec<String>, String> {
        let this = Self::this_mut();

        // Fetch remote schema metadata from the gateway
        let url = format!("{}/schema/{}", this.api_url, stmt.remote_schema);
        let json = get_json(&url, &this.auth_header)?;

        let schema: SchemaResponse = serde_json::from_value(json)
            .map_err(|e| format!("schema response parse error: {}", e))?;

        let mut ret = Vec::new();
        for table in &schema.tables {
            // Apply LIMIT TO / EXCEPT filters (IRIS table names are uppercase)
            let included = match stmt.list_type {
                ImportSchemaType::All => true,
                ImportSchemaType::LimitTo => stmt
                    .table_list
                    .iter()
                    .any(|t| t.eq_ignore_ascii_case(&table.name)),
                ImportSchemaType::Except => !stmt
                    .table_list
                    .iter()
                    .any(|t| t.eq_ignore_ascii_case(&table.name)),
            };
            if !included {
                continue;
            }

            // Column names are quoted with the exact IRIS dictionary case,
            // because IRIS quoted identifiers are case-sensitive
            let cols = table
                .columns
                .iter()
                .map(|c| format!("    \"{}\" {}", c.name, iris_type_to_pg(&c.col_type)))
                .collect::<Vec<_>>()
                .join(",\n");

            ret.push(format!(
                "create foreign table if not exists {}.{} (\n{}\n)\nserver {} options (object '{}.{}')",
                stmt.local_schema,
                table.name.to_lowercase(),
                cols,
                stmt.server_name,
                stmt.remote_schema,
                table.name,
            ));
        }

        Ok(ret)
    }
}

// ── Helper: convert JSON value to Cell (type-aware) ───────────────────
//
// Host conventions (wrappers/src/fdw/wasm_fdw/bindings/v1.rs):
//   Cell::Date        = seconds since Unix epoch (1970-01-01)
//   Cell::Timestamp   = microseconds since Unix epoch
//   Cell::Timestamptz = microseconds since Unix epoch
//   Cell::Numeric     = f64
//   Cell::Json        = valid JSON text (host runs serde_json::from_str)

fn json_to_cell(val: &JsonValue, oid: &TypeOid) -> Cell {
    match oid {
        TypeOid::Bool => Cell::Bool(match val {
            JsonValue::Bool(b) => *b,
            JsonValue::Number(n) => n.as_i64().map(|v| v != 0).unwrap_or(false),
            JsonValue::String(s) => {
                matches!(s.to_lowercase().as_str(), "1" | "t" | "true" | "yes" | "on")
            }
            _ => false,
        }),
        TypeOid::I8 => Cell::I8(json_to_i64(val) as i8),
        TypeOid::I16 => Cell::I16(json_to_i64(val) as i16),
        TypeOid::I32 => Cell::I32(json_to_i64(val) as i32),
        TypeOid::I64 => Cell::I64(json_to_i64(val)),
        TypeOid::F32 => Cell::F32(json_to_f64(val) as f32),
        TypeOid::F64 => Cell::F64(json_to_f64(val)),
        TypeOid::Numeric => Cell::Numeric(json_to_f64(val)),
        TypeOid::String => Cell::String(json_to_string(val)),
        TypeOid::Date => Cell::Date(match val {
            // IRIS gateway returns ODBC date string "YYYY-MM-DD"
            JsonValue::String(s) => parse_date_days(s).map(|d| d * 86400).unwrap_or(0),
            JsonValue::Number(n) => n.as_i64().unwrap_or(0),
            _ => 0,
        }),
        TypeOid::Timestamp => Cell::Timestamp(match val {
            JsonValue::String(s) => parse_timestamp_usecs(s).unwrap_or(0),
            JsonValue::Number(n) => n.as_i64().unwrap_or(0),
            _ => 0,
        }),
        TypeOid::Timestamptz => Cell::Timestamptz(match val {
            JsonValue::String(s) => parse_timestamp_usecs(s).unwrap_or(0),
            JsonValue::Number(n) => n.as_i64().unwrap_or(0),
            _ => 0,
        }),
        TypeOid::Json => match val {
            JsonValue::String(s) => {
                // Host parses this as JSON; wrap plain strings as JSON strings
                if serde_json::from_str::<JsonValue>(s).is_ok() {
                    Cell::Json(s.clone())
                } else {
                    Cell::Json(format!("\"{}\"", json_escape(s)))
                }
            }
            other => Cell::Json(other.to_string()),
        },
        // UUID and unknown types are passed through as strings
        TypeOid::Uuid => Cell::Uuid(json_to_string(val)),
        TypeOid::Other(_) => Cell::String(json_to_string(val)),
    }
}

fn json_to_i64(val: &JsonValue) -> i64 {
    match val {
        JsonValue::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f as i64))
            .unwrap_or(0),
        JsonValue::String(s) => s.trim().parse::<i64>().unwrap_or(0),
        JsonValue::Bool(b) => i64::from(*b),
        _ => 0,
    }
}

fn json_to_f64(val: &JsonValue) -> f64 {
    match val {
        JsonValue::Number(n) => n.as_f64().unwrap_or(0.0),
        JsonValue::String(s) => s.trim().parse::<f64>().unwrap_or(0.0),
        JsonValue::Bool(b) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        _ => 0.0,
    }
}

fn json_to_string(val: &JsonValue) -> String {
    match val {
        JsonValue::String(s) => s.clone(),
        JsonValue::Null => String::new(),
        other => other.to_string(),
    }
}

/// Parse "YYYY-MM-DD" to days since Unix epoch (1970-01-01).
fn parse_date_days(s: &str) -> Option<i64> {
    let date_part = s.split([' ', 'T']).next()?;
    let mut it = date_part.split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: i64 = it.next()?.parse().ok()?;
    let d: i64 = it.next()?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, m, d))
}

/// Parse "YYYY-MM-DD[ HH:MM[:SS[.ffffff]]]" to microseconds since Unix epoch.
/// IRIS ODBC timestamps are in server local time; no timezone conversion.
fn parse_timestamp_usecs(s: &str) -> Option<i64> {
    let mut parts = s.trim().splitn(2, [' ', 'T']);
    let date_part = parts.next()?;
    let time_part = parts.next().unwrap_or("00:00:00");

    let mut di = date_part.split('-');
    let y: i64 = di.next()?.parse().ok()?;
    let m: i64 = di.next()?.parse().ok()?;
    let d: i64 = di.next()?.parse().ok()?;

    // Split off fractional seconds (ignore any trailing timezone designator)
    let time_part = time_part.trim_end_matches('Z');
    let (hms, micros) = match time_part.split_once('.') {
        Some((hms, frac)) => {
            let digits: String = frac.chars().take_while(|c| c.is_ascii_digit()).collect();
            let mut us = digits.parse::<i64>().unwrap_or(0);
            for _ in digits.len()..6 {
                us *= 10;
            }
            (hms, us)
        }
        None => (time_part, 0),
    };

    let mut ti = hms.split(':');
    let hh: i64 = ti.next().unwrap_or("0").parse().ok()?;
    let mi: i64 = ti.next().unwrap_or("0").parse().ok()?;
    let ss: i64 = ti.next().unwrap_or("0").parse().ok()?;

    let days = days_from_civil(y, m, d);
    Some(
        days * 86_400_000_000
            + hh * 3_600_000_000
            + mi * 60_000_000
            + ss * 1_000_000
            + micros,
    )
}

/// Days-from-civil algorithm (proleptic Gregorian calendar).
/// Returns days since 1970-01-01.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (m + 9) % 12; // [0, 11]
    let doy = (153 * mp + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

bindings::export!(IrisFdw with_types_in bindings);