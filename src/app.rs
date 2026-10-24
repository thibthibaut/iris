use crate::{config::Config, db::Database};

pub struct AppContext {
    pub config: Config,
    pub db: Database,
    pub scan_limit: Option<usize>,
    pub pending_limit: Option<usize>,
}

impl AppContext {
    pub fn new(config: Config, db: Database, scan_limit: Option<usize>, pending_limit: Option<usize>) -> Self {
        Self { config, db, scan_limit, pending_limit }
    }
}
