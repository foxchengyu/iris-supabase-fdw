# iris-supabase-fdw

> [中文版本](README_CN.md)

Query **InterSystems IRIS** tables from **Supabase** as if they were local Postgres tables.

```
┌─── Supabase (Postgres) ───┐         ┌─── InterSystems IRIS ───┐
│ wrappers → Wasm runtime    │  HTTP   │ iris-fdw-gateway         │
│   └─ iris_fdw.wasm (Rust)  ├────────►│   └─ Dynamic SQL ─► DB  │
└────────────────────────────┘  JSON   └─────────────────────────┘
```

## Components

| Component | Language | Description |
|---|---|---|
| `iris-fdw-wasm/` | Rust → WebAssembly | FDW package, runs inside Supabase Wrappers |
| `iris-fdw-gateway/` | ObjectScript | REST gateway that executes SQL on IRIS |

## Quick Start

### 1. Install the IRIS Gateway

On your IRIS instance:

```objectscript
zpm "install iris-fdw-gateway"
```

Or manually: import `IrisFdw.Gateway.Handler.cls` and set up the web application at `/iris-fdw/v1`.

Create a dedicated user:
```objectscript
Set user = ##class(Security.Users).Create("fdw_user", "%All", "your-password")
```

Grant table access:
```sql
GRANT SELECT, INSERT, UPDATE, DELETE ON Demo.Patient TO fdw_user;
```

### 2. Install the Wasm FDW on Supabase

```sql
-- Enable Wrappers
create extension if not exists wrappers with schema extensions;
create foreign data wrapper wasm_wrapper
  handler wasm_fdw_handler
  validator wasm_fdw_validator;

-- Create IRIS server
create server iris_server
  foreign data wrapper wasm_wrapper
  options (
    fdw_package_url 'https://github.com/foxchengyu/iris-supabase-fdw/releases/download/v0.1.0/iris_fdw.wasm',
    fdw_package_name 'iris:iris-fdw',
    fdw_package_version '0.1.0',
    fdw_package_checksum '10b1c9624e8cf90c1921d92b0e87b8f7600328595829076e5d09f55b40b9d0d0',
    api_url 'https://your-iris-host/iris-fdw/v1',
    username 'fdw_user',
    password_id '<vault-secret-id>',
    debug 'true'
  );

-- Import IRIS schema as foreign tables
create schema if not exists iris;
import foreign schema "Demo" from server iris_server into iris;
```

### 3. Query IRIS from Supabase

```sql
-- Query IRIS tables
select id, name, city from iris.patient where city = 'Beijing';

-- Join IRIS tables with Postgres tables
select p.name, l.status
from iris.patient p
join local.visits l on l.patient_id = p.id;

-- Insert into IRIS
insert into iris.patient (name, city) values ('新患者', 'Shanghai');

-- Update IRIS
update iris.patient set city = 'Guangzhou' where id = 1;

-- Delete from IRIS
delete from iris.patient where id = 99;
```

## Configuration Options

### Server options (CREATE SERVER)

| Option | Required | Default | Description |
|---|---|---|---|
| `fdw_package_url` | yes | - | URL to the .wasm file |
| `fdw_package_name` | yes | - | Package identifier |
| `fdw_package_version` | yes | - | Semantic version |
| `fdw_package_checksum` | yes | - | SHA256 of .wasm |
| `api_url` | yes | - | IRIS gateway base URL |
| `username` | no | `_SYSTEM` | IRIS login username |
| `password` | no | `SYS` | IRIS login password (plain) |
| `password_id` | no | - | Supabase Vault secret ID (preferred) |
| `batch_size` | no | `100` | Rows per batch in paged queries |
| `debug` | no | `false` | Enable SQL debug logging |

### Table options (CREATE FOREIGN TABLE)

| Option | Required | Description |
|---|---|---|
| `object` | yes | IRIS table name (e.g., `Demo.Patient`) |

## Supported Types

| IRIS | Postgres |
|---|---|
| INTEGER | integer |
| BIGINT | bigint |
| SMALLINT / TINYINT | smallint |
| DOUBLE / FLOAT / REAL | double precision |
| VARCHAR(n) / CHAR(n) | text |
| DATE | date |
| TIMESTAMP | timestamp |
| TIME | text |
| BOOLEAN / BIT | boolean |
| NUMERIC / DECIMAL | numeric |

## Security

- IRIS SQL executes with the authenticated user's privileges
- Password stored in Supabase Vault (not plaintext in SQL)
- `/query` endpoint only allows SELECT
- `/exec` endpoint only allows INSERT/UPDATE/DELETE
- Read-only mode: `Set ^FDW.Gateway("readOnly") = 1`
- Use HTTPS between Supabase and IRIS

## Build from Source

### Wasm FDW

```bash
cd iris-fdw-wasm
rustup target add wasm32-unknown-unknown
cargo install cargo-component --locked --version 0.21.1
cargo component build --release --target wasm32-unknown-unknown
# Output: target/wasm32-unknown-unknown/release/iris_fdw.wasm
```

Or just download the prebuilt binary from [Releases](https://github.com/foxchengyu/iris-supabase-fdw/releases).

### IRIS Gateway

```bash
cd iris-fdw-gateway
# Import src/IrisFdw.Gateway.Handler.cls into IRIS
# Set up web application at /iris-fdw/v1
```

## Limitations

- `NUMERIC` values travel as 64-bit floats (Wasm FDW interface limitation)
- Paging is stateless (offset-based); results may shift if data changes during multi-page scan
- No TCP/SuperServer support (Wasm sandbox restriction)
- IRIS Cloud SQL is not supported (no web application hosting)

## License

MIT