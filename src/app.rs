use crate::{config::Config, db::Database};

pub struct AppContext {
    pub config: Config,
    pub db: Database,
    pub limit: Option<usize>,
}

impl AppContext {
    pub fn new(config: Config, db: Database, limit: Option<usize>) -> Self {
        Self { config, db, limit }
    }
}
