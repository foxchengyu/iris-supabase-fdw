# IRIS FDW Gateway

This is the IRIS-side gateway for the `iris_fdw` Supabase Wasm Foreign Data Wrapper.

## Quick Install

```objectscript
; Install via IPM (InterSystems Package Manager)
zpm "install iris-fdw-gateway"
```

This sets up a web application at `/iris-fdw/v1` on your IRIS instance.

## Manual Setup

If you don't have IPM, you can manually set up the web application:

### 1. Import the class

Import `src/IrisFdw.Gateway.Handler.cls` into your IRIS namespace.

### 2. Create the web application

In the Management Portal (http://localhost:52773/csp/sys/UtilHome.csp):

1. Go to **System Administration → Security → Applications → Web Applications**
2. Click **Create New Web Application**
3. Fill in:
   - **Name**: `/iris-fdw/v1`
   - **Namespace**: Your target namespace (e.g., `USER`)
   - **Dispatch Class**: `IrisFdw.Gateway.Handler`
   - **Enabled**: checked
   - **Allowed Authentication Methods**: Password
4. Click **Save**

### 3. Create a dedicated user (recommended)

```
USER>zn "%SYS"
%SYS>Set user = ##class(Security.Users).Create("fdw_user", "%All", "fdw_password")
%SYS>Set role = ##class(Security.Roles).Create("fdw_role", "FDW access role")
%SYS>Do ##class(Security.Roles).AddUser(role, user)
```

Grant only the SQL tables you want exposed:
```sql
GRANT SELECT, INSERT, UPDATE, DELETE ON Demo.Patient TO fdw_user;
GRANT SELECT ON Demo.Appointment TO fdw_user;
```

### 4. Enable read-only mode (optional)

```objectscript
Set ^FDW.Gateway("readOnly") = 1
```

## API Endpoints

### `GET /ping`

Health check. Returns server info.

### `POST /query`

Execute a SELECT query.

**Request:**
```json
{
  "query": "SELECT id, name, city FROM Demo.Patient WHERE city = ?",
  "parameters": ["Beijing"],
  "offset": 0,
  "max_rows": 100
}
```

**Response:**
```json
{
  "rows": [
    {"id": 1, "name": "张三", "city": "Beijing"},
    {"id": 3, "name": "李四", "city": "Beijing"}
  ],
  "row_count": 2,
  "has_more": false
}
```

### `POST /exec`

Execute an INSERT, UPDATE, or DELETE statement.

**Request:**
```json
{
  "query": "INSERT INTO Demo.Patient (name, city) VALUES (?, ?)",
  "parameters": ["王五", "Shanghai"]
}
```

**Response:**
```json
{
  "rows_affected": 1,
  "row_id": 5
}
```

### `GET /schema/:name`

Get metadata for all tables in a schema.

**Example:** `GET /schema/Demo`

**Response:**
```json
{
  "schema": "Demo",
  "tables": [
    {
      "name": "Patient",
      "columns": [
        {"name": "id", "type": "INTEGER", "nullable": false, "is_primary_key": false},
        {"name": "name", "type": "VARCHAR(50)", "nullable": false, "is_primary_key": false},
        {"name": "city", "type": "VARCHAR(50)", "nullable": true, "is_primary_key": false}
      ]
    }
  ]
}
```

## Security Notes

- The gateway uses **IRIS's own privilege model**. SQL statements execute with the authenticated user's permissions.
- `/query` only allows SELECT statements.
- `/exec` only allows INSERT/UPDATE/DELETE. SELECT is rejected on this endpoint.
- To prevent writes entirely, set `^FDW.Gateway("readOnly") = 1`.
- Use HTTPS in production. IRIS supports TLS via the web server or SuperServer SSL configuration.
- For Supabase integration, store the password in **Supabase Vault** (use `password_id` in server options).