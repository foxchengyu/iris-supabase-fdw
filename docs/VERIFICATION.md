# Verification Plan — iris-supabase-fdw

## Phase 1: Gateway Unit Tests

### 1.1 Health Check
```objectscript
// Test: Gateway responds on /ping
// Expected: HTTP 200, JSON with status="ok"

Do ##class(%Net.HttpRequest).%New()
Set req.Server = "localhost"
Set req.Port = 52773
Set req.Https = 0
Set req.Location = "/iris-fdw/v1/ping"

Set sc = req.Get("ping")
Write req.HttpResponse.Data.Read()
// Should return: {"status":"ok","version":"1.0.0","server":"IRIS ...","namespace":"...","read_only":false}
```

### 1.2 Query Execution (SELECT)
```objectscript
// Test: POST /query with a simple SELECT
Set req = ##class(%Net.HttpRequest).%New()
Set req.Server = "localhost"
Set req.Port = 52773
Set req.ContentType = "application/json"
Set req.Location = "/iris-fdw/v1/query"

Set body = "{""query"":""SELECT TOP 3 ID, Name FROM Sample.Person"",""parameters"":[],""offset"":0,""max_rows"":100}"
Do req.EntityBody.Write(body)

Set sc = req.Post("query")
Set resp = {}.%FromJSON(req.HttpResponse.Data)
// Expected: resp.rows is an array, resp.row_count >= 0
Write "Rows: ", resp.rows.%Size(), !
Write "Count: ", resp.row_count, !
```

### 1.3 Query with Parameters
```objectscript
// Test: POST /query with parameterized query
Set body = "{""query"":""SELECT ID, Name FROM Sample.Person WHERE Name = ?"",""parameters"":[""Smith,John""],""offset"":0,""max_rows"":10}"
// Expected: matching rows returned
```

### 1.4 Query with Offset
```objectscript
// Test: Paging - second page
Set body = "{""query"":""SELECT ID, Name FROM Sample.Person ORDER BY ID"",""parameters"":[],""offset"":5,""max_rows"":5}"
// Expected: rows 6-10, skipping first 5
```

### 1.5 Exec (INSERT/UPDATE/DELETE)
```objectscript
// Test: POST /exec with INSERT
Set req = ##class(%Net.HttpRequest).%New()
Set req.Server = "localhost"
Set req.Port = 52773
Set req.ContentType = "application/json"
Set req.Location = "/iris-fdw/v1/exec"

Set body = "{""query"":""INSERT INTO Sample.TestPerson (Name) VALUES ('Test User')""}"
Do req.EntityBody.Write(body)

Set sc = req.Post("exec")
Set resp = {}.%FromJSON(req.HttpResponse.Data)
// Expected: resp.rows_affected = 1
```

### 1.6 SQL Injection Prevention
```objectscript
// Test: /query rejects non-SELECT statements
Set body = "{""query"":""DROP TABLE Sample.Person"",""parameters"":[]}"
// Expected: error response, "Only SELECT statements are allowed"

// Test: /exec rejects SELECT statements
Set body = "{""query"":""SELECT * FROM Sample.Person"",""parameters"":[]}"
// Expected: error response
```

### 1.7 Read-Only Mode
```objectscript
// First enable read-only
Set ^FDW.Gateway("readOnly") = 1

// Test: /exec is rejected
Set body = "{""query"":""INSERT INTO Sample.TestPerson (Name) VALUES ('X')""}"
// Expected: "Gateway is in read-only mode"

// Clean up
Kill ^FDW.Gateway("readOnly")
```

### 1.8 Schema Metadata
```objectscript
// Test: GET /schema/Sample
Set req = ##class(%Net.HttpRequest).%New()
Set req.Server = "localhost"
Set req.Port = 52773
Set req.Location = "/iris-fdw/v1/schema/Sample"

Set sc = req.Get("schema")
Set resp = {}.%FromJSON(req.HttpResponse.Data)
// Expected: resp.schema = "Sample", resp.tables is an array
// Each table has name and columns array
```

## Phase 2: Wasm FDW Verification (requires Supabase project)

### 2.1 Setup on Supabase
```sql
-- 1. Enable Wrappers extension
create extension if not exists wrappers with schema extensions;

-- 2. Create Wasm FDW
create foreign data wrapper wasm_wrapper
  handler wasm_fdw_handler
  validator wasm_fdw_validator;

-- 3. Create IRIS server
create server iris_server
  foreign data wrapper wasm_wrapper
  options (
    fdw_package_url 'https://github.com/<USER>/iris-supabase-fdw/releases/download/v0.1.0/iris_fdw.wasm',
    fdw_package_name 'iris:iris-fdw',
    fdw_package_version '0.1.0',
    fdw_package_checksum '<SHA256>',
    api_url 'https://your-iris-host/iris-fdw/v1',
    username 'fdw_user',
    password 'your-password',
    debug 'true'
  );

-- 4. Create schema and foreign table
create schema if not exists iris;
create foreign table iris.person (
  id bigint,
  name text,
  city text
)
server iris_server
options (
  object 'Sample.Person',
  rowid_column 'id'
);
```

### 2.2 Basic SELECT
```sql
-- Test: Simple query
select * from iris.person limit 5;
-- Expected: Returns rows from IRIS Sample.Person table

-- Test: Column projection
select id, name from iris.person limit 3;
-- Expected: Only id and name columns
```

### 2.3 Filter Pushdown
```sql
-- Test: WHERE clause pushed to IRIS
select id, name from iris.person where city = 'New York';
-- Check logs for "iris_fdw: SQL = SELECT ... WHERE "city" = 'New York'"
-- Expected: Only New York residents returned
```

### 2.4 INSERT
```sql
-- Test: Insert a row into IRIS
insert into iris.person (name, city) values ('Test User', 'Test City');
-- Check IRIS: SELECT * FROM Sample.Person WHERE Name = 'Test User'
```

### 2.5 UPDATE
```sql
-- Test: Update a row
update iris.person set city = 'Updated City' where id = 1;
-- Check IRIS: verify the update took effect
```

### 2.6 DELETE
```sql
-- Test: Delete a row
delete from iris.person where id = 9999;
-- Expected: row deleted from IRIS
```

## Phase 3: Integration Tests (end-to-end)

### 3.1 Cross-Database JOIN
```sql
-- Test: Join IRIS table with local Supabase table
create table local.notes (person_id bigint, note text);
insert into local.notes values (1, 'Test note');

select p.name, n.note
from iris.person p
join local.notes n on n.person_id = p.id;
-- Expected: joined results
```

### 3.2 Pagination
```sql
-- Test: Large result set with LIMIT
select * from iris.person limit 10;
-- Expected: only 10 rows, no more

-- Test: OFFSET
select * from iris.person limit 5 offset 5;
-- Expected: rows 6-10
```

### 3.3 LIKE Pushdown
```sql
-- Test: LIKE condition
select * from iris.person where name like 'Smith%';
-- Check logs for LIKE in generated SQL
```

### 3.4 Error Handling
```sql
-- Test: Invalid table
select * from iris.nonexistent_table;
-- Expected: clear error message from IRIS

-- Test: Invalid column
select nonexistent_col from iris.person;
-- Expected: error message
```

## Running Tests Locally

### Gateway tests (requires IRIS instance)
```bash
# Start IRIS with Docker
docker run -d --name iris-test -p 52773:52773 \
  intersystemsdc/iris-community:latest

# Wait for IRIS to start, then import and test
docker exec iris-test iris session IRIS < import-gateway.txt

# Run verification via curl
curl -u _SYSTEM:SYS http://localhost:52773/iris-fdw/v1/ping
curl -u _SYSTEM:SYS -X POST http://localhost:52773/iris-fdw/v1/query \
  -H "Content-Type: application/json" \
  -d '{"query":"SELECT 1 AS test","parameters":[],"offset":0,"max_rows":10}'
```

### Wasm FDW tests (requires Supabase project + IRIS)
```bash
# 1. Build the Wasm package
cd iris-fdw-wasm
cargo component build --release

# 2. Upload .wasm to a GitHub release or local HTTP server

# 3. Run SQL tests on Supabase (see Phase 2 above)
```