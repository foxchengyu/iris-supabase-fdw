Class IrisFdw.Gateway.Handler Extends %CSP.REST
{

/// Base URL path for this REST service
Parameter HandleCorsRequest = 1;

/// Dispatch map: /iris-fdw/v1/*
XData UrlMap [ XMLNamespace = "http://www.intersystems.com/urlmap" ]
{
<Routes>
    <!-- Health check -->
    <Route Url="/ping" Method="GET" Call="Ping" />
    
    <!-- Execute SELECT query, return JSON rows -->
    <Route Url="/query" Method="POST" Call="Query" />
    
    <!-- Execute INSERT/UPDATE/DELETE -->
    <Route Url="/exec" Method="POST" Call="Exec" />
    
    <!-- Get schema metadata for IMPORT FOREIGN SCHEMA -->
    <Route Url="/schema/:name" Method="GET" Call="GetSchema" />
</Routes>
}

// ── /ping ─────────────────────────────────────────────────────────────
//
// GET /iris-fdw/v1/ping
// Returns gateway status and server info.
//
ClassMethod Ping() As %Status
{
    Set %response.ContentType = "application/json"
    
    Set obj = {}
    Set obj.status = "ok"
    Set obj.version = "1.0.0"
    Set obj.server = $ZV
    Set obj.namespace = $NAMESPACE
    Set obj.read_only = $Get(^FDW.Gateway("readOnly"), 0)
    
    Write obj.%ToJSON()
    Return $$$OK
}

// ── /query ────────────────────────────────────────────────────────────
//
// POST /iris-fdw/v1/query
// Body: { "query": "SELECT ...", "parameters": [...], "offset": 0, "max_rows": 100 }
//
// Executes a SELECT statement via Dynamic SQL and returns results as JSON.
// Only SELECT statements are allowed.
//
ClassMethod Query() As %Status
{
    Set %response.ContentType = "application/json"
    
    // Read request body
    Set data = {}.%FromJSON(%request.Content)
    If '$IsObject(data) {
        Set err = {}
        Set err.error = "Invalid JSON request body"
        Write err.%ToJSON()
        Return $$$OK
    }
    
    Set sql = data.query
    Set params = data.parameters
    Set offset = $Get(data.offset, 0)
    Set maxRows = $Get(data.max_rows, 100)
    
    // Validate: only SELECT allowed
    If '##class(IrisFdw.Gateway.Handler).IsSelect(sql) {
        Set err = {}
        Set err.error = "Only SELECT statements are allowed on /query"
        Write err.%ToJSON()
        Return $$$OK
    }
    
    // Prepare and execute
    Set stmt = ##class(%SQL.Statement).%New()
    Set sc = stmt.%Prepare(sql)
    If $$$ISERR(sc) {
        Set err = {}
        Set err.error = "SQL preparation failed: " _ $System.Status.GetErrorText(sc)
        Write err.%ToJSON()
        Return $$$OK
    }
    
    // Bind parameters if present
    // Simple positional parameter binding
    If $IsObject(params) {
        For i = 0:1:params.%Size() - 1 {
            Set val = params.%Get(i)
            Do stmt.%Metadata.parameters.SetAt(val, i + 1)
        }
    }
    
    // Execute
    Set rset = stmt.%Execute()
    If rset.%SQLCODE < 0 {
        Set err = {}
        Set err.error = "SQL Error [" _ rset.%SQLCODE _ "]: " _ rset.%Message
        Write err.%ToJSON()
        Return $$$OK
    }
    
    // Build result
    Set result = {}
    Set rows = []
    
    // Get column metadata
    Set colCount = rset.%ResultColumnCount
    Set colNames = []
    For c = 1:1:colCount {
        Do colNames.%Push(rset.%GetColumnName(c))
    }
    
    // Fetch rows
    Set rowCount = 0
    Set fetched = 0
    
    While rset.%Next() {
        // Handle offset (skip rows)
        If rowCount < offset {
            Set rowCount = rowCount + 1
            Continue
        }
        
        // Handle max_rows
        If fetched >= maxRows {
            Set result.has_more = 1
            Quit
        }
        
        Set row = {}
        For c = 1:1:colCount {
            Set colName = colNames.%Get(c - 1)
            Set val = rset.%GetData(c)
            
            // Handle SQL types
            If val = "" {
                Do row.%Set(colName, "")
            } ElseIf $IsObject(val) {
                // Stream or object - convert to string
                Do row.%Set(colName, val.%ToString())
            } Else {
                Do row.%Set(colName, val)
            }
        }
        
        Do rows.%Push(row)
        Set fetched = fetched + 1
        Set rowCount = rowCount + 1
    }
    
    Set result.rows = rows
    Set result.row_count = rowCount
    If '$Data(result.has_more) {
        Set result.has_more = 0
    }
    
    Write result.%ToJSON()
    Return $$$OK
}

// ── /exec ─────────────────────────────────────────────────────────────
//
// POST /iris-fdw/v1/exec
// Body: { "query": "INSERT/UPDATE/DELETE ...", "parameters": [...] }
//
// Executes a DML statement (INSERT/UPDATE/DELETE) via Dynamic SQL.
// SELECT is rejected on this endpoint.
//
ClassMethod Exec() As %Status
{
    Set %response.ContentType = "application/json"
    
    // Check read-only mode
    If $Get(^FDW.Gateway("readOnly"), 0) {
        Set err = {}
        Set err.error = "Gateway is in read-only mode"
        Write err.%ToJSON()
        Return $$$OK
    }
    
    // Read request body
    Set data = {}.%FromJSON(%request.Content)
    If '$IsObject(data) {
        Set err = {}
        Set err.error = "Invalid JSON request body"
        Write err.%ToJSON()
        Return $$$OK
    }
    
    Set sql = data.query
    
    // Validate: SELECT is not allowed on /exec
    If ##class(IrisFdw.Gateway.Handler).IsSelect(sql) {
        Set err = {}
        Set err.error = "SELECT statements are not allowed on /exec. Use /query instead."
        Write err.%ToJSON()
        Return $$$OK
    }
    
    // Execute
    Set stmt = ##class(%SQL.Statement).%New()
    Set sc = stmt.%Prepare(sql)
    If $$$ISERR(sc) {
        Set err = {}
        Set err.error = "SQL preparation failed: " _ $System.Status.GetErrorText(sc)
        Write err.%ToJSON()
        Return $$$OK
    }
    
    Set rset = stmt.%Execute()
    If rset.%SQLCODE < 0 {
        Set err = {}
        Set err.error = "SQL Error [" _ rset.%SQLCODE _ "]: " _ rset.%Message
        Write err.%ToJSON()
        Return $$$OK
    }
    
    Set result = {}
    Set result.rows_affected = rset.%ROWCOUNT
    Set result.row_id = rset.%ROWID
    
    Write result.%ToJSON()
    Return $$$OK
}

// ── /schema/:name ─────────────────────────────────────────────────────
//
// GET /iris-fdw/v1/schema/Demo
//
// Returns column metadata for all tables in the given schema.
// Used by IMPORT FOREIGN SCHEMA.
//
ClassMethod GetSchema(pSchema As %String) As %Status
{
    Set %response.ContentType = "application/json"
    
    Set result = {}
    Set result.schema = pSchema
    Set result.tables = []
    
    // Query INFORMATION_SCHEMA for table/column metadata
    Set sql = "SELECT TABLE_NAME, COLUMN_NAME, DATA_TYPE, " _
              "CHARACTER_MAXIMUM_LENGTH, IS_NULLABLE " _
              "FROM INFORMATION_SCHEMA.COLUMNS " _
              "WHERE TABLE_SCHEMA = ? " _
              "ORDER BY TABLE_NAME, ORDINAL_POSITION"
    
    Set stmt = ##class(%SQL.Statement).%New()
    Set sc = stmt.%Prepare(sql)
    If $$$ISERR(sc) {
        Set err = {}
        Set err.error = $System.Status.GetErrorText(sc)
        Write err.%ToJSON()
        Return $$$OK
    }
    
    Set rset = stmt.%Execute(pSchema)
    If rset.%SQLCODE < 0 {
        Set err = {}
        Set err.error = "SQL Error: " _ rset.%Message
        Write err.%ToJSON()
        Return $$$OK
    }
    
    Set currentTable = ""
    Set tableObj = ""
    Set columns = []
    
    While rset.%Next() {
        Set tableName = rset.%GetData(1)
        
        // New table?
        If tableName '= currentTable {
            If currentTable '= "" {
                Set tableObj.name = currentTable
                Set tableObj.columns = columns
                Do result.tables.%Push(tableObj)
            }
            Set currentTable = tableName
            Set tableObj = {}
            Set columns = []
        }
        
        Set col = {}
        Set col.name = rset.%GetData(2)
        Set col.type = ##class(IrisFdw.Gateway.Handler).MapType(rset.%GetData(3), rset.%GetData(4))
        Set col.nullable = (rset.%GetData(5) = "YES")
        Set col.is_primary_key = 0
        
        Do columns.%Push(col)
    }
    
    // Don't forget the last table
    If currentTable '= "" {
        Set tableObj.name = currentTable
        Set tableObj.columns = columns
        Do result.tables.%Push(tableObj)
    }
    
    Write result.%ToJSON()
    Return $$$OK
}

// ── Helper: check if SQL is a SELECT ──────────────────────────────────

ClassMethod IsSelect(sql As %String) As %Boolean [ Private ]
{
    Set trimmed = $ZStrip(sql, "<>W")
    Set upper = $ZConvert($Extract(trimmed, 1, 20), "U")
    // Check for SELECT, WITH (CTE), CALL
    If $Extract(upper, 1, 6) = "SELECT" { Return 1 }
    If $Extract(upper, 1, 4) = "WITH" { Return 1 }
    If $Extract(upper, 1, 4) = "CALL" { Return 1 }
    Return 0
}

// ── Helper: map IRIS SQL type to display type ────────────────────────

ClassMethod MapType(dataType As %String, maxLen As %String) As %String [ Private ]
{
    Set dt = $ZConvert(dataType, "U")
    
    If dt = "INTEGER" { Return "INTEGER" }
    If dt = "BIGINT" { Return "BIGINT" }
    If dt = "SMALLINT" { Return "SMALLINT" }
    If dt = "TINYINT" { Return "TINYINT" }
    If dt = "DOUBLE" { Return "DOUBLE" }
    If dt = "FLOAT" { Return "DOUBLE" }
    If dt = "REAL" { Return "DOUBLE" }
    If dt = "DECIMAL" { Return "NUMERIC" }
    If dt = "NUMERIC" { Return "NUMERIC" }
    If dt = "VARCHAR" { Return "VARCHAR(" _ maxLen _ ")" }
    If dt = "CHAR" { Return "CHAR(" _ maxLen _ ")" }
    If dt = "DATE" { Return "DATE" }
    If dt = "TIME" { Return "TIME" }
    If dt = "TIMESTAMP" { Return "TIMESTAMP" }
    If dt = "BOOLEAN" { Return "BOOLEAN" }
    
    // Default - return as-is
    Return dataType
}

}