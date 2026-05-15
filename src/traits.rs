use anyhow::Result;

use crate::app::AppContext;

pub trait BatchProcessor {
    fn name(&self) -> &'static str;
    fn run(&self, ctx: &AppContext) -> Result<()>;
}
