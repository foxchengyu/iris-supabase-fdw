# iris-supabase-fdw

> [English version](README.md)

在 **Supabase** 中像查询本地 Postgres 表一样查询 **InterSystems IRIS** 数据。

```
┌─── Supabase (Postgres) ───┐         ┌─── InterSystems IRIS ───┐
│ wrappers → Wasm 运行时     │  HTTP   │ iris-fdw-gateway         │
│   └─ iris_fdw.wasm (Rust)  ├────────►│   └─ Dynamic SQL ─► DB  │
└────────────────────────────┘  JSON   └─────────────────────────┘
```

## 组件

| 组件 | 语言 | 说明 |
|---|---|---|
| `iris-fdw-wasm/` | Rust → WebAssembly | FDW 包，在 Supabase Wrappers 内运行 |
| `iris-fdw-gateway/` | ObjectScript | REST 网关，在 IRIS 上执行 SQL |

## 快速开始

### 1. 安装 IRIS 网关

在 IRIS 实例上执行：

```objectscript
zpm "install iris-fdw-gateway"
```

或手动导入：将 `IrisFdw.Gateway.Handler.cls` 导入 IRIS，然后在 `/iris-fdw/v1` 配置 Web Application。

创建专用用户：
```objectscript
Set user = ##class(Security.Users).Create("fdw_user", "%All", "your-password")
```

授予表访问权限：
```sql
GRANT SELECT, INSERT, UPDATE, DELETE ON Demo.Patient TO fdw_user;
```

### 2. 在 Supabase 上安装 Wasm FDW

```sql
-- 启用 Wrappers
create extension if not exists wrappers with schema extensions;
create foreign data wrapper wasm_wrapper
  handler wasm_fdw_handler
  validator wasm_fdw_validator;

-- 创建 IRIS 服务器
create server iris_server
  foreign data wrapper wasm_wrapper
  options (
    fdw_package_url 'https://github.com/<user>/iris-supabase-fdw/releases/download/v0.1.0/iris_fdw.wasm',
    fdw_package_name 'iris:iris-fdw',
    fdw_package_version '0.1.0',
    fdw_package_checksum '<checksum-from-release>',
    api_url 'https://your-iris-host/iris-fdw/v1',
    username 'fdw_user',
    password_id '<vault-secret-id>',
    debug 'true'
  );

-- 将 IRIS Schema 导入为外部表
create schema if not exists iris;
import foreign schema "Demo" from server iris_server into iris;
```

### 3. 在 Supabase 中查询 IRIS

```sql
-- 查询 IRIS 表
select id, name, city from iris.patient where city = 'Beijing';

-- IRIS 表与 Postgres 表关联查询
select p.name, l.status
from iris.patient p
join local.visits l on l.patient_id = p.id;

-- 向 IRIS 插入数据
insert into iris.patient (name, city) values ('新患者', 'Shanghai');

-- 更新 IRIS 数据
update iris.patient set city = 'Guangzhou' where id = 1;

-- 删除 IRIS 数据
delete from iris.patient where id = 99;
```

## 配置选项

### 服务器选项 (CREATE SERVER)

| 选项 | 必填 | 默认值 | 说明 |
|---|---|---|---|
| `fdw_package_url` | 是 | - | .wasm 文件的 URL |
| `fdw_package_name` | 是 | - | 包标识符 |
| `fdw_package_version` | 是 | - | 语义化版本号 |
| `fdw_package_checksum` | 是 | - | .wasm 文件的 SHA256 |
| `api_url` | 是 | - | IRIS 网关基础 URL |
| `username` | 否 | `_SYSTEM` | IRIS 登录用户名 |
| `password` | 否 | `SYS` | IRIS 登录密码（明文） |
| `password_id` | 否 | - | Supabase Vault 密钥 ID（推荐） |
| `batch_size` | 否 | `100` | 分页查询每批行数 |
| `debug` | 否 | `false` | 开启 SQL 调试日志 |

### 表选项 (CREATE FOREIGN TABLE)

| 选项 | 必填 | 说明 |
|---|---|---|
| `object` | 是 | IRIS 表名（如 `Demo.Patient`） |

## 支持的数据类型

| IRIS | Postgres |
|---|---|
| INTEGER | bigint |
| BIGINT | bigint |
| DOUBLE | double precision |
| VARCHAR(n) | text |
| DATE | date |
| TIMESTAMP | timestamp |
| BOOLEAN | boolean |
| NUMERIC | double precision |

## 安全性

- IRIS SQL 以已认证用户的权限执行
- 密码存储在 Supabase Vault 中（而非 SQL 明文）
- `/query` 端点仅允许 SELECT 语句
- `/exec` 端点仅允许 INSERT/UPDATE/DELETE
- 只读模式：`Set ^FDW.Gateway("readOnly") = 1`
- Supabase 与 IRIS 之间使用 HTTPS 通信

## 从源码构建

### Wasm FDW

```bash
cd iris-fdw-wasm
rustup target add wasm32-unknown-unknown
cargo install cargo-component --locked --version 0.13.2
cargo component build --release
# 输出：target/wasm32-unknown-unknown/release/iris_fdw.wasm
```

### IRIS 网关

```bash
cd iris-fdw-gateway
# 将 src/IrisFdw.Gateway.Handler.cls 导入 IRIS
# 在 /iris-fdw/v1 配置 Web Application
```

## 限制

- `NUMERIC` 类型以 64 位浮点数传输（Wasm FDW 接口限制）
- 分页为无状态（基于 offset），多页扫描期间数据变化可能导致结果偏移
- 不支持 TCP/SuperServer 连接（Wasm 沙箱限制）
- 不支持 IRIS Cloud SQL（无法托管 Web 应用）

## 许可证

MIT