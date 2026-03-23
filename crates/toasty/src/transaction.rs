use std::sync::Arc;

use crate::{db::Connection, Db, Executor, Result};

use toasty_core::{
    async_trait,
    driver::operation::{self, IsolationLevel},
    stmt::Value,
    Schema,
};
use tokio::sync::Mutex;

/// Builder for configuring a transaction before starting it.
pub struct TransactionBuilder<'db> {
    db: &'db Db,
    isolation: Option<IsolationLevel>,
    read_only: bool,
}

impl<'db> TransactionBuilder<'db> {
    pub(crate) fn new(db: &'db Db) -> Self {
        TransactionBuilder {
            db,
            isolation: None,
            read_only: false,
        }
    }

    /// Set the isolation level for this transaction.
    pub fn isolation(mut self, level: IsolationLevel) -> Self {
        self.isolation = Some(level);
        self
    }

    /// Set whether this transaction is read-only.
    pub fn read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    /// Begin the transaction with the configured options.
    pub async fn begin(self) -> Result<Transaction<'db>> {
        Transaction::begin_with(self.db, self.isolation, self.read_only).await
    }
}

/// An active database transaction.
///
/// Borrows `&mut Db` for its lifetime, preventing concurrent use of the
/// same Db handle while a transaction is open.
///
/// If dropped without calling [`commit`](Self::commit) or
/// [`rollback`](Self::rollback), the transaction is automatically rolled back.
pub struct Transaction<'db> {
    conn: Arc<Mutex<Box<dyn Connection>>>,
    db: &'db Db,
    /// Whether commit or rollback has been called.
    finalized: bool,

    /// If this is a nested transaction (implemented through savepoints),
    /// this holds the savepoint stack depth to be used as an identifier.
    savepoint: Option<usize>,
}

impl<'db> Transaction<'db> {
    pub(crate) async fn begin(db: &'db Db) -> Result<Transaction<'db>> {
        Self::begin_with(db, None, false).await
    }

    pub(crate) async fn begin_with(
        db: &'db Db,
        isolation: Option<IsolationLevel>,
        read_only: bool,
    ) -> Result<Transaction<'db>> {
        let conn = db.connection().await?;

        // We're creating the Transaction struct before actually starting the transaction. If the
        // future is cancelled while waiting on the response of the start command, the transaction
        // is still rolled back.
        let mut tx = Transaction {
            conn: Arc::new(Mutex::new(conn)),
            db,
            finalized: false,
            savepoint: None,
        };

        tx.execute(operation::Transaction::Start {
            isolation,
            read_only,
        })
        .await?;

        Ok(tx)
    }

    /// Commit the transaction.
    pub async fn commit(mut self) -> Result<()> {
        // Because driver operations are done in a background task, all the operations aren't
        // cancelled and will continue even if this future is dropped. Setting the finalized flag
        // to true early here makes sure that if the future is dropped we don't queue a rollback
        // command.
        self.finalized = true;
        let op = match self.savepoint {
            Some(_) => operation::Transaction::ReleaseSavepoint(self.savepoint()),
            None => operation::Transaction::Commit,
        };
        self.execute(op).await
    }

    /// Roll back the transaction.
    pub async fn rollback(mut self) -> Result<()> {
        // See `commit` why we're setting the finalized flag to true early.
        self.finalized = true;
        let op = match self.savepoint {
            Some(_) => operation::Transaction::RollbackToSavepoint(self.savepoint()),
            None => operation::Transaction::Rollback,
        };
        self.execute(op).await
    }

    fn savepoint(&self) -> String {
        format!("tx_{}", self.savepoint.unwrap())
    }

    async fn execute(&mut self, op: operation::Transaction) -> Result<()> {
        self.conn
            .lock()
            .await
            .exec(self.db.schema(), op.into())
            .await?;
        Ok(())
    }
}

impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        if !self.finalized {
            let op = match self.savepoint {
                Some(_) => operation::Transaction::RollbackToSavepoint(self.savepoint()),
                None => operation::Transaction::Rollback,
            };
            let db = self.db.clone();
            let conn = self.conn.clone();
            tokio::spawn(async move {
                let _ = conn.lock().await.exec(db.schema(), op.into()).await;
            });
        }
    }
}

#[async_trait]
impl<'a> Executor for Transaction<'a> {
    async fn transaction(&mut self) -> Result<Transaction<'_>> {
        let mut tx = Transaction {
            conn: self.conn.clone(),
            db: self.db,
            finalized: false,
            savepoint: Some(match self.savepoint {
                Some(savepoint) => savepoint + 1,
                None => 1,
            }),
        };

        tx.execute(operation::Transaction::Savepoint(tx.savepoint()))
            .await?;
        Ok(tx)
    }

    async fn exec_untyped(&mut self, stmt: toasty_core::stmt::Statement) -> Result<Value> {
        let mut conn = self.conn.lock().await;
        self.db.exec_stmt(&mut **conn, stmt, true).await
    }

    fn schema(&mut self) -> &Arc<Schema> {
        self.db.schema()
    }
}
