use crate::{engine::Engine, Db, Register, Result};

use toasty_core::{
    driver::Driver,
    schema::{self, app},
};

#[derive(Default)]
pub struct Builder {
    /// Model definitions from macro
    ///
    /// TODO: move this into `core::schema::Builder` after old schema file
    /// implementatin is removed.
    models: Vec<app::Model>,

    /// Schema builder
    core: schema::Builder,
}

impl Builder {
    pub fn register<T: Register>(&mut self) -> &mut Self {
        self.models.push(T::schema());
        self
    }

    /// Set the table name prefix for all tables
    pub fn table_name_prefix(&mut self, prefix: &str) -> &mut Self {
        self.core.table_name_prefix(prefix);
        self
    }

    pub fn build_app_schema(&self) -> Result<app::Schema> {
        app::Schema::from_macro(&self.models)
    }

    pub async fn build(&mut self, driver: impl Driver) -> Result<Db> {
        // Validate capability consistency
        driver.capability().validate()?;

        let schema = self
            .core
            .build(self.build_app_schema()?, driver.capability())?;

        let engine = Engine::new(schema, Box::new(driver));

        Ok(Db { engine })
    }
}
