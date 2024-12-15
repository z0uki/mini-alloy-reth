#![allow(dead_code)]
use std::path::PathBuf;

/// We use the tower-like layering functionality that has been baked into the
/// alloy-provider to intercept the requests and redirect to the
/// `RethDbProvider`.
pub struct RethDbLayer {
    db_path: PathBuf,
    handle: tokio::runtime::Handle,
}

/// Initialize the `RethDBLayer` with the path to the reth datadir.
impl RethDbLayer {
    pub const fn new(db_path: PathBuf, handle: tokio::runtime::Handle) -> Self {
        Self { db_path, handle }
    }

    pub const fn db_path(&self) -> &PathBuf {
        &self.db_path
    }

    pub const fn handle(&self) -> &tokio::runtime::Handle {
        &self.handle
    }
}
