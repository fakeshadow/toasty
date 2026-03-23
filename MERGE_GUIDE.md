# Merge Guide for the `engine` Branch

This document records what must be preserved when merging future branches into
the `engine` branch. Review it before resolving conflicts in the files listed
below.

---

## Key invariants

### 1. `Db` — no connection pool, plain `Clone`

`crates/toasty/src/db.rs`

- `Db` is a plain struct that derives `Clone`. It holds one field: `engine: Engine`.
- **No pool module.** The deleted `crates/toasty/src/db/pool.rs` must stay
  deleted. Do not reintroduce a `pool` module or a `Pool` type.
- Connections are acquired lazily per-operation via `Db::connection()`, which
  calls `self.engine.driver.connect()` directly.
- `Db` exposes no pool-related public API (no `pool()`, `pool_size()`,
  `max_connections()`, etc.).
- `impl Executor for &Db` **must remain**. It provides the same behaviour as
  `impl Executor for Db` and is required so callers can use a shared reference
  as an executor.

  ```rust
  #[async_trait]
  impl Executor for &Db {
      async fn transaction(&mut self) -> Result<Transaction<'_>> {
          Transaction::begin(self).await
      }
      async fn exec_untyped(&mut self, stmt: stmt::Statement) -> Result<Value> {
          self.exec_stmt(stmt, false).await
      }
      fn schema(&mut self) -> &Arc<Schema> {
          Db::schema(self)
      }
  }
  ```

### 2. `Engine` — owns schema and driver

`crates/toasty/src/engine.rs`

- `Engine` derives `Debug` and `Clone`. It holds:
  - `schema: Arc<Schema>`
  - `driver: Arc<dyn Driver>`
- It does **not** hold a pool or any connection state.
- `Engine::exec` takes a `&mut dyn Connection` passed in from the caller; it
  does not own or acquire connections itself.

### 3. `Transaction` and `TransactionBuilder` — borrow `&Db`

`crates/toasty/src/transaction.rs`

- `Transaction<'db>` borrows `&'db Db` (not an owned `Db` and not an `Arc`).
- Fields that must remain:
  - `db: &'db Db`
  - `finalized: bool` — set to `true` before awaiting commit/rollback so that
    a future-cancellation mid-flight does not queue a second rollback.
  - `savepoint: Option<usize>` — depth counter for nested transactions
    (implemented via SQL savepoints).
- The `Drop` impl spawns a `tokio::spawn` task that clones `self.db` and sends
  a rollback operation when `finalized` is `false`. This is the only correct
  way to perform async work in `Drop`.

  ```rust
  impl Drop for Transaction<'_> {
      fn drop(&mut self) {
          if !self.finalized {
              let op = match self.savepoint {
                  Some(_) => operation::Transaction::RollbackToSavepoint(self.savepoint()),
                  None => operation::Transaction::Rollback,
              };
              let db = self.db.clone();
              tokio::spawn(async move {
                  let _ = db.exec_operation(op.into()).await;
              });
          }
      }
  }
  ```

- `TransactionBuilder<'db>` borrows `&'db Db` and must not be changed to own
  it or use `Arc<Db>`.

### 4. No pool-related tests or doc-tests

- Any test (unit, integration, or doc-test) that exercises a connection pool
  concept must be removed or not merged.
- This includes doc-tests in `db.rs` or `builder.rs` that demonstrate pool
  configuration (e.g., `pool_size`, `max_connections`, `idle_timeout`).
- The existing doc-tests in `db.rs` (showing `Db::builder()` → `build()` →
  `push_schema()`) are fine to keep as they do not involve a pool.

---

## Conflict resolution checklist

When merging another branch into `engine`, go through these steps:

1. **`crates/toasty/src/db.rs`**
   - [ ] `Db` still derives `Clone` with a single `engine: Engine` field.
   - [ ] `impl Executor for &Db` block is present and unchanged.
   - [ ] No pool field, pool method, or pool import has been added.
   - [ ] `Db::connection()` still calls `self.engine.driver.connect()` directly.

2. **`crates/toasty/src/db/`**
   - [ ] `pool.rs` does not exist.
   - [ ] `mod pool;` is not present in `db.rs`.

3. **`crates/toasty/src/engine.rs`**
   - [ ] `Engine` has exactly two fields: `schema: Arc<Schema>` and
     `driver: Arc<dyn Driver>`.
   - [ ] `Engine::exec` signature still takes `connection: &mut dyn Connection`.

4. **`crates/toasty/src/transaction.rs`**
   - [ ] `Transaction` still borrows `&'db Db`, not an owned copy.
   - [ ] `finalized` and `savepoint` fields are intact.
   - [ ] `Drop` impl spawns a `tokio::spawn` rollback task.
   - [ ] `commit` and `rollback` still set `finalized = true` before awaiting.

5. **Tests**
   - [ ] No test references a pool type or pool configuration.
   - [ ] No doc-test demonstrates pool usage.

---

## Why these constraints exist

| Constraint | Reason |
|---|---|
| No connection pool | The `engine` branch redesigns connection management; drivers own their own pooling strategy. A `Db`-level pool would conflict with driver-internal pooling and complicate the transaction ownership model. |
| `impl Executor for &Db` | Callers that hold a shared reference (e.g. inside a struct) should not need to clone or mutably borrow `Db` just to execute a query. |
| `Transaction` borrows `&Db` | Ownership prevents two live transactions on the same `Db` handle at the compile level, enforcing single-transaction-at-a-time semantics without runtime locks. |
| `Drop` uses `tokio::spawn` | `Drop` is synchronous; the only safe way to perform async cleanup (sending a rollback command) is to hand the work off to the runtime. Cloning `Db` is cheap (`Arc` clone) so this has negligible overhead. |
