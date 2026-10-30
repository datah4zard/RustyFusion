# RustyFusion – Copilot Instructions

## Project Overview

RustyFusion is an open-source server emulator for Cartoon Network's MMO **FusionFall**, written in Rust. It is inspired by [OpenFusion](https://github.com/OpenFusionProject) and aims for eventual feature-completion with a cleaner, safer codebase. It speaks the original FusionFall network protocol and uses the OpenFusion PostgreSQL database schema.

The server is split into **two separate binaries**:
- **Login server** (`src/bin/login/`) – handles authentication, character selection, and shard routing.
- **Shard server** (`src/bin/shard/`) – handles the live game world (entities, combat, travel, items, etc.).

---

## Build, Lint, and Format

```bash
cargo build          # build (required before running)
cargo clippy         # lint (must pass before committing)
cargo fmt            # format (must pass before committing)
cargo run --bin login   # run login server
cargo run --bin shard   # run shard server
```

Unit tests exist in several modules (`util`, `config`, `chunk`, `net/mod`, `net/crypto`, `entity/mod`, `tabledata`). Run them with `cargo test`. Do not remove existing tests.

The minimum supported Rust version (MSRV) is **1.88.0**, edition **2021**, as declared in `Cargo.toml`.

CI (`rust.yml`) enforces both `cargo build` and `rustfmt` on push/PR to `main`. Always run `cargo fmt` and `cargo clippy` before committing.

---

## Repository Layout

```
src/
  lib.rs              # library root; declares all public modules
  defines.rs          # numeric constants (PROTOCOL_VERSION, ranges, IDs, etc.)
  enums.rs            # game enums (ItemType, CombatStyle, etc.) via ffenum! macro
  error.rs            # FFError, FFResult, logging utilities
  config.rs           # config framework (generated from config_schema.toml)
  tabledata.rs        # game data loaded from JSON tabledata submodule
  helpers.rs          # shared shard helper functions (broadcast, group, etc.)
  util.rs             # general utilities (clamp, rand, timers, Bitfield, etc.)
  chunk.rs            # world chunking, InstanceID, EntityMap
  ai.rs               # NPC AI script name selection
  geo.rs              # geo-IP-based shard routing
  monitor.rs          # OpenFusion monitor protocol
  tui.rs              # ratatui-based terminal UI
  timer.rs            # interval timer utilities
  path.rs             # NPC/entity pathing
  item.rs             # item types and logic
  nano.rs             # nano types and logic
  mission.rs          # mission/task types
  skills.rs           # skill/buff system
  trade.rs            # trade context
  net/                # networking layer
    mod.rs            # PacketCallback, DisconnectCallback, ClientMap types
    ffclient.rs       # FFClient handle (cheap to clone, send-safe)
    ffconnection.rs   # raw TCP connection handling
    ffserver.rs       # FFServer (async TCP listener + event loop)
    packet.rs         # Packet, FFPacket trait, PacketID enum (auto-generated)
    crypto.rs         # encryption logic
  entity/             # game entity types
    mod.rs            # Entity trait, Combatant trait, EntityID enum
    player.rs         # Player struct
    npc.rs            # NPC struct
    slider.rs         # Slider entity
    egg.rs            # E.G.G. entity
  state/              # server state
    mod.rs            # shared state utilities
    shard.rs          # ShardServerState
    login.rs          # LoginServerState
  database/           # database layer
    mod.rs            # DbImpl trait, db_get(), db_init(), macro-generated API
    postgresql.rs     # PostgreSQL implementation
  scripting/          # Luau scripting for NPC AI
    mod.rs            # scripting_init(), Lua environment setup, luau_* macros
    entity.rs         # entity Lua bindings
    npc.rs            # NPC-specific Lua bindings
  bin/
    login/            # login server binary
      main.rs
      login.rs        # login packet handlers
      shard.rs        # shard↔login connection handlers
    shard/            # shard server binary
      main.rs
      pc.rs           # player connection handlers
      nano.rs         # nano handlers
      item.rs         # item/vendor/trade handlers
      combat.rs       # combat handlers
      mission.rs      # mission handlers
      npc.rs          # NPC interaction handlers
      chat.rs         # chat handlers
      buddy.rs        # buddy system handlers
      group.rs        # group handlers
      gm.rs           # GM/admin command handlers
      transport.rs    # travel/transport handlers
      trade.rs        # trade handlers
config_schema.toml    # source of truth for config struct (auto-generates config code)
build.rs              # code generation (config + Luau type stubs)
tabledata/            # git submodule with JSON game data (xdt.json, NPCs, paths, etc.)
scripts/              # Luau scripts for NPC AI behaviors
sql/                  # SQL schema files
docker-compose.yml    # PostgreSQL dev container
```

---

## Key Types and Patterns

### Error Handling

All fallible functions return `FFResult<T>` = `Result<T, FFError>`.

```rust
// Build an error (non-disconnecting)
FFError::build(Severity::Warning, "message".to_string())

// Build a disconnecting error (closes the client connection)
FFError::build_dc(Severity::Warning, "message".to_string())

// Convert an enum parse error
FFError::from_enum_err(val)

// Chain errors
err.with_parent(inner_err)
```

Severity levels: `Debug`, `Info`, `Warning`, `Fatal`.

Logging utilities:
```rust
log(Severity::Info, "message");     // log a plain message
log_error(err);                     // log an FFError
log_if_failed(result);              // log and swallow errors
panic_if_failed(result);            // log then panic on error
```

### Packet Handlers

Shard packet handlers that need async DB access use the async signature:
```rust
pub async fn handler_name(
    pkt: Packet,
    clients: &ClientMap<'_>,
    state_lock: Arc<Mutex<ShardServerState>>,
    time: SystemTime,
) -> FFResult<()>
```

Sync handlers (most shard handlers) use:
```rust
pub fn handler_name(
    pkt: Packet,
    clients: &ClientMap,
    state: &mut ShardServerState,
) -> FFResult<()>
```

Login server handlers use `LoginServerState` instead of `ShardServerState`.

**Reading a packet:**
```rust
let pkt: &sP_CL2FE_REQ_SOME_ACTION = pkt.get()?;
```

**Sending a packet:**
```rust
client.send_packet(P_FE2CL_REP_SOME_ACTION_SUCC, &resp_struct);
```

**Broadcasting to nearby entities:**
```rust
state.entity_map.for_each_around(EntityID::Player(pc_id), |client| {
    client.send_packet(P_FE2CL_SOME_BCAST, &bcast_struct);
});
```

### Packet Naming Convention

Packets follow the OpenFusion naming convention:
- `P_CL2LS_` – client → login server
- `P_LS2CL_` – login server → client
- `P_CL2FE_` – client → shard (FE = Frontend)
- `P_FE2CL_` – shard → client
- `P_FE2LS_` – shard → login server
- `P_LS2FE_` – login server → shard
- `REQ_` = request, `REP_` = reply, `SUCC` = success, `FAIL` = failure

Packet structs (e.g. `sP_CL2FE_REQ_NANO_EQUIP`) are auto-generated and live in `src/net/packet.rs`.

### Macros

```rust
unused!()        // expands to Default::default(); used for padding/unused packet fields
placeholder!(v)  // returns v; logs "PLACEHOLDER" in debug builds for unimplemented features
```

### Config

Config is generated at build time from `config_schema.toml` via `build.rs`. Access it with:
```rust
let config = config_get();
let val = config.general.some_setting.get();
```

Never edit the generated config code directly; edit `config_schema.toml`.

### Tabledata

Game data (items, NPCs, missions, nano stats, etc.) is loaded from the `tabledata/` submodule (JSON files). Access via:
```rust
let tdata = tdata_get();
let stats = tdata.get_npc_stats(npc_type);
```

The submodule must be checked out (`git clone --recurse-submodules` or `git submodule update --init`).

### Database

The database trait is defined via macro in `src/database/mod.rs`. Only PostgreSQL is currently supported. Initialization:
```rust
db_init(Severity::Fatal).await?;
let db = db_get();
db.some_operation(args).await?;
```

### Entity System

All game entities implement the `Entity` trait. Combat-capable entities additionally implement `Combatant`. Entity IDs are:
```rust
EntityID::Player(i32)
EntityID::NPC(i32)
EntityID::Slider(i32)
EntityID::Egg(i32)
```

Entities live in `ShardServerState.entity_map` (an `EntityMap`). Players are retrieved by pc_id:
```rust
state.get_player(pc_id)?      // &Player
state.get_player_mut(pc_id)?  // &mut Player
```

### FFClient

`FFClient` is a cheap-to-clone, thread-safe handle to a connected client. It holds a `Arc<RwLock<ClientMetadata>>` and a sender channel. Key methods:
```rust
client.send_packet(pkt_id, &pkt);
client.get_player_id()?
client.get_account_id()?
client.disconnect();
```

### Scripting (Luau)

NPC AI is driven by Luau scripts in `scripts/`. The `luau_type!`, `luau_function!`, `luau_class!`, and `luau_method!` macros in `src/scripting/mod.rs` both register Lua bindings and generate Luau type stubs (`scripts/globals.d.luau`) via `build.rs`.

### Chunking / Instancing

The world is divided into chunks. Each entity has a `ChunkCoords` with an `InstanceID` (channel, map, optional instance number). Entities are tracked in the `EntityMap`. Visibility is chunk-based.

---

## Code Style

- **Formatting**: enforced by `cargo fmt` (rustfmt defaults).
- **Blank lines**: add blank lines between multi-line blocks; do **not** add blank lines between adjacent single-line statements (e.g., `send_packet()` followed by `Ok(())`).
- **Comments**: only add comments matching the existing style or explaining non-obvious logic.
- **`unused!()`**: always use this macro for default/padding packet fields rather than `Default::default()` directly.
- **`placeholder!()`**: use for stubs of unimplemented but planned features.
- **Error propagation**: prefer `?` operator; use `log_if_failed()` when you want to swallow and log an error silently.

### Imports

Handler files in `src/bin/shard/` and `src/bin/login/` use glob imports for tightly-coupled namespaces:
```rust
use rusty_fusion::{
    enums::*,
    error::*,
    net::packet::{PacketID::*, *},
    unused, util,
};
```
Use `PacketID::*` glob only when many packet IDs are needed in the same file. Use `crate::error::*` in library handler files for the same reason. Do not glob-import otherwise.

### Naming

- Rust code uses standard Rust naming: `snake_case` for variables/functions/modules, `PascalCase` for types/enums/traits.
- Packet struct fields use the original FusionFall Hungarian notation (`i` = int, `s` = string, `e` = enum, `sz` = zero-terminated string, `b` = bool, `ui` = unsigned int, etc.). Do not rename these.
- Protocol-level structs (`sP_CL2FE_REQ_…`, `LoginData`) that carry non-snake-case field names are annotated with `#[allow(non_snake_case)]`.
- Game enums defined with `ffenum!` in `enums.rs` use `PascalCase` variants.

### Casting and Numeric Types

Packet fields use types dictated by the protocol (`i32`, `i16`, `i8`, `u32`, etc.). Cast to the appropriate Rust type as needed when using them in logic (e.g., `pkt.iSlotNum as usize`). Prefer infallible `as` casts for well-bounded protocol fields, and `try_into()?` for enum fields or values that may be out-of-range.

### Scoped Error Chains

When a handler needs to perform several fallible steps before sending a response (especially a FAIL reply on error), use an immediately-invoked closure to create a local `FFResult` scope:
```rust
pub fn some_handler(...) -> FFResult<()> {
    let result: FFResult<ResponseType> = (|| {
        let x = fallible_step_1()?;
        let y = fallible_step_2(x)?;
        Ok(build_response(y))
    })();

    match result {
        Ok(resp) => client.send_packet(P_FE2CL_REP_SUCC, &resp),
        Err(e) => {
            log_error(e);
            client.send_packet(P_FE2CL_REP_FAIL, &fail_pkt);
        }
    }
    Ok(())
}
```
This pattern keeps the success path clean and lets the failure handling construct the appropriate FAIL packet.

---

## Module Organization

### Library vs. Binary

`src/lib.rs` is the library crate root — it declares all public modules and is shared by both binaries. Binaries (`src/bin/login/` and `src/bin/shard/`) import from the library with `use rusty_fusion::…`. Logic shared across both binaries belongs in the library; binary-specific handler logic lives in the binary.

### Re-export Pattern

Sub-modules within a module directory follow the re-export pattern: the sub-module is declared private (`mod ffclient;`) and its public items are re-exported with `pub use ffclient::*;`. This lets callers import from the parent module without knowing the internal file structure:
```rust
// in net/mod.rs
mod ffclient;
pub use ffclient::*;
```

### Global Singletons

Long-lived, read-only globals (config, tabledata, database handle) are held in `OnceLock` or `LazyLock` statics and accessed via `config_get()`, `tdata_get()`, `db_get()`. These must be initialized once at startup before use. Never store mutable game state in globals; that belongs in `ShardServerState` or `LoginServerState`.

### Handler File Organization

Each shard binary handler file (`pc.rs`, `item.rs`, `combat.rs`, etc.) groups all packet handlers for a single game feature domain. Handler functions are `pub` and named after the packet they handle (lowercased, without the `p_cl2fe_req_` prefix, e.g. `item_move`, `pc_attack_npcs`).

When a handler file has internal helpers shared only within that file, they are placed in a private `mod helpers { ... }` block at the bottom of the file. These are not exposed publicly:
```rust
// at the bottom of gm.rs
mod helpers {
    use super::*;
    pub fn validate_perms(client: &FFClient, state: &ShardServerState, req_perms: i16) -> FFResult<i32> { … }
}
```

### State Modules

`state/shard.rs` (`ShardServerState`) holds all live shard game state: the entity map, active trades, groups, login data, etc. `state/login.rs` (`LoginServerState`) holds login server state. Both are passed to handlers by `&mut` reference (sync handlers) or via `Arc<Mutex<…>>` (async handlers).

---

## Safety — The Trust-but-Verify Model

RustyFusion treats **all client input as untrusted**. Validation is layered: the networking layer enforces structural validity before a packet ever reaches a handler, and handlers are responsible for semantic validation.

### Layer 1 — Connection-level filtering (`ffconnection.rs`)

Before any handler is called, `can_send_packet()` checks that the packet ID is valid for the current `ClientType`. Unauthenticated clients (`ClientType::Unknown`) may only send a hard-coded whitelist of three packets (`UNKNOWN_CT_ALLOWED_PACKETS`). Clients whose type doesn't match the packet direction bitmask have their packet silently dropped with a Warning log. This prevents unauthed clients from invoking handlers that expect an authenticated session.

### Layer 2 — Structural deserialization (`pkt.get()`)

`pkt.get::<sP_CL2FE_REQ_…>()` returns `FFResult<&T>`. It validates:
- That enough bytes came in for the struct size.
- That the data pointer is correctly aligned (checked in `bytes_to_struct()`; misaligned data returns an error rather than causing UB).

A failed `pkt.get()` propagates as `?` and terminates the handler early. The `FFError` that results has `should_dc = false` by default; use `FFError::build_dc(…)` to force a disconnect.

### Layer 3 — Semantic validation inside handlers

Every handler validates the meaningful content of packet fields before acting on them:

- **Enum fields**: always converted with `.try_into()?` (e.g., `pkt.eFrom.try_into()?`). An unrecognized discriminant is an immediate error + disconnect (`FFError::from_enum_err` sets `should_dc = true`).
- **Slot indices / array bounds**: all slot numbers from the client are validated against inventory sizes via `player.set_item(location, slot_num, …)?` and similar methods that return `FFResult` on out-of-range access.
- **Target counts**: client-reported counts (e.g., `iNPCCnt`) are checked against a server-defined maximum before iterating.
- **Entity lookups**: `state.get_player(pc_id)?`, `state.get_npc(npc_id)?`, etc. all return `FFResult` — a missing entity is an error, not a panic.
- **Currency / resource checks**: before deducting taros, nano potions, weapon boosts, etc., the handler verifies the player has enough.
- **Permission checks**: all GM handlers call `helpers::validate_perms(client, state, CN_ACCOUNT_LEVEL__…)` as their first action. The player's `perms` field (sourced from the database `AccountLevel` column) must be at or above the required level; otherwise the call fails with a Warning.

### Layer 4 — Login handshake / session guard

Players can only enter a shard if the login server has deposited a `LoginData` entry keyed by their serial key. The shard removes this entry atomically on `pc_enter`, preventing replay. A `pending_entering_uids` set guards against concurrent double-enters during the async DB load phase.

### Unsafe code

The only `unsafe` in the networking code is the pointer cast in `bytes_to_struct`. This is sound because:
1. Alignment is checked at runtime before the cast.
2. All packet structs are `#[repr(C)]` / `#[repr(packed(4))]` and composed of primitive integer types, which are valid for any bit pattern.
3. The receive buffer (`AlignedBuf`) is declared `#[repr(C, align(4))]`, guaranteeing 4-byte alignment at the call site.

Unit tests in `net/mod.rs` verify that misaligned slices are rejected and that aligned round-trips are correct.

---

## Development Setup

1. **Clone recursively** to get the tabledata submodule:
   ```bash
   git clone --recurse-submodules https://github.com/yungcomputerchair/RustyFusion
   ```
2. **Database**: start a PostgreSQL instance. For local dev, use:
   ```bash
   docker compose up -d
   ```
   Or configure `config.toml` with your own PostgreSQL connection details.
3. **Config**: copy/create `config.toml` from the schema in `config_schema.toml`. The config file path can be overridden via command-line argument.
4. **Run**:
   ```bash
   cargo run --bin login
   cargo run --bin shard
   ```

---

## CI / GitHub Actions

The workflow at `.github/workflows/rust.yml` runs on push/PR to `main` (when relevant files change):
- **Build and test**: `cargo build --verbose` + `cargo test`
- **Formatting**: `rustfmt` check via `actions-rust-lang/rustfmt`
- **MSRV**: read from `Cargo.toml` via `actions-rust-lang/msrv`

Build artifacts (`target/debug/login`, `target/debug/shard`) are uploaded on push to `main`.

---

## Common Pitfalls

- The `tabledata/` directory is a **git submodule**. If it is empty, the server will fail to start. Run `git submodule update --init`.
- Config is code-generated; adding a new config option requires editing `config_schema.toml`, not the Rust source.
- Packet structs are code-generated from the protocol; do not manually add or edit them in `packet.rs`.
- The `scripting/mod.rs` macros (`luau_type!`, etc.) are parsed by `build.rs` to produce Luau type stubs. Their format must be preserved exactly.
- `FFClient::send_packet` takes `&self` — no mutable borrow needed to send packets.
- Packet struct fields use the original FusionFall naming convention (Hungarian notation: `i` = int, `s` = string, `e` = enum, etc.).
