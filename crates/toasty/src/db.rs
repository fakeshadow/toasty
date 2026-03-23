mod builder;
mod connect;

pub use builder::Builder;
pub use connect::*;

use crate::{engine::Engine, Executor, Result, Transaction, TransactionBuilder};

use toasty_core::{
    async_trait,
    driver::Driver,
    stmt::{self, Value},
    Schema,
};

use std::sync::Arc;

/// A database handle. Each instance owns (or will lazily acquire) a dedicated
/// connection from the pool.
#[derive(Clone)]
pub struct Db {
    pub(crate) engine: Engine,
}

impl Db {
    /// Create a new [`Builder`] for configuring and opening a database.
    ///
    /// # Examples
    ///
    /// ```
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// # #[derive(Debug, toasty::Model)]
    /// # struct User {
    /// #     #[key]
    /// #     id: i64,
    /// #     name: String,
    /// # }
    /// let driver = toasty_driver_sqlite::Sqlite::in_memory();
    /// let db = toasty::Db::builder()
    ///     .register::<User>()
    ///     .build(driver)
    ///     .await
    ///     .unwrap();
    /// # });
    /// ```
    pub fn builder() -> Builder {
        Builder::default()
    }

    /// Lazily acquire a connection from the pool.
    pub(crate) async fn connection(&self) -> Result<Box<dyn Connection>> {
        self.engine.driver.connect().await
    }

    pub(crate) async fn exec_stmt(
        &self,
        conn: &mut dyn Connection,
        stmt: stmt::Statement,
        in_transaction: bool,
    ) -> Result<Value> {
        let returns_list = match &stmt {
            stmt::Statement::Query(q) => !q.single,
            stmt::Statement::Insert(i) => !i.source.single,
            stmt::Statement::Update(i) => match &i.target {
                stmt::UpdateTarget::Query(q) => !q.single,
                stmt::UpdateTarget::Model(_) => false,
                _ => true,
            },
            stmt::Statement::Delete(d) => !d.selection().single,
        };

        let mut stream = self.engine.exec(conn, stmt, in_transaction).await?;

        if returns_list {
            let values = stream.collect().await?;
            Ok(Value::List(values))
        } else {
            match stream.next().await {
                Some(value) => value,
                None => Ok(Value::Null),
            }
        }
    }

    /// Create a [`TransactionBuilder`] for configuring transaction options
    /// (isolation level, read-only) before starting it.
    ///
    /// # Examples
    ///
    /// ```
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// # #[derive(Debug, toasty::Model)]
    /// # struct User {
    /// #     #[key]
    /// #     id: i64,
    /// #     name: String,
    /// # }
    /// # let driver = toasty_driver_sqlite::Sqlite::in_memory();
    /// # let mut db = toasty::Db::builder().register::<User>().build(driver).await.unwrap();
    /// let tx = db.transaction_builder()
    ///     .read_only(true)
    ///     .begin()
    ///     .await
    ///     .unwrap();
    /// tx.commit().await.unwrap();
    /// # });
    /// ```
    pub fn transaction_builder(&mut self) -> TransactionBuilder<'_> {
        TransactionBuilder::new(self)
    }

    /// Creates tables and indices defined in the schema on the database.
    pub async fn push_schema(&mut self) -> Result<()> {
        self.connection().await?.push_schema(self.schema()).await
    }

    /// Drops the entire database and recreates an empty one without applying migrations.
    pub async fn reset_db(&self) -> Result<()> {
        self.driver().reset_db().await
    }

    /// Returns a reference to the underlying database driver.
    pub fn driver(&self) -> &dyn Driver {
        &*self.engine.driver
    }

    /// Returns the compiled schema used by this database handle.
    pub fn schema(&self) -> &Arc<Schema> {
        &self.engine.schema
    }

    /// Returns the capability flags reported by the driver.
    ///
    /// The query engine uses these to decide which operation types to generate
    /// (e.g., SQL vs. key-value).
    pub fn capability(&self) -> &Capability {
        self.engine.capability()
    }
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Db").field("engine", &self.engine).finish()
    }
}

#[async_trait]
impl Executor for Db {
    async fn transaction(&mut self) -> Result<Transaction<'_>> {
        Transaction::begin(self).await
    }

    #[inline]
    async fn exec_untyped(&mut self, stmt: stmt::Statement) -> Result<Value> {
        (&*self).exec_untyped(stmt).await
    }

    fn schema(&mut self) -> &Arc<Schema> {
        Db::schema(self)
    }
}

#[async_trait]
impl Executor for &Db {
    async fn transaction(&mut self) -> Result<Transaction<'_>> {
        Transaction::begin(self).await
    }

    async fn exec_untyped(&mut self, stmt: stmt::Statement) -> Result<Value> {
        let mut conn = self.connection().await?;
        self.exec_stmt(&mut *conn, stmt, false).await
    }

    fn schema(&mut self) -> &Arc<Schema> {
        Db::schema(self)
    }
}
