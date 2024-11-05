use std::{marker::PhantomData, path::PathBuf, sync::Arc};

use alloy::{
    providers::{Provider, ProviderLayer, RootProvider},
    rpc::types::{Filter, Log},
    transports::{Transport, TransportErrorKind, TransportResult},
};
use async_trait::async_trait;
use reth_blockchain_tree::noop::NoopBlockchainTree;
use reth_chainspec::MAINNET;
use reth_db::{open_db_read_only, DatabaseEnv};
use reth_network_api::noop::NoopNetwork;
use reth_node_ethereum::{EthEvmConfig, EthereumNode};
use reth_node_types::NodeTypesWithDBAdapter;
use reth_provider::{
    providers::{BlockchainProvider, StaticFileProvider},
    ProviderFactory,
};
use reth_rpc::{eth::EthTxBuilder, EthApi, EthFilter};
use reth_rpc_eth_api::filter::EthFilterApiServer;
use reth_rpc_eth_types::{EthFilterConfig, EthStateCache, EthStateCacheConfig};
use reth_tasks::TokioTaskExecutor;
use reth_transaction_pool::noop::NoopTransactionPool;

use crate::layer::RethDbLayer;

type RethProvider = BlockchainProvider<NodeTypesWithDBAdapter<EthereumNode, Arc<DatabaseEnv>>>;
type RethApi = EthApi<RethProvider, RethTxPool, NoopNetwork, EthEvmConfig>;
type RethFilter = EthFilter<RethProvider, RethTxPool, RethApi>;
type RethTxPool = NoopTransactionPool;

/// Implement the `ProviderLayer` trait for the `RethDBLayer` struct.
impl<P, T> ProviderLayer<P, T> for RethDbLayer
where
    P: Provider<T>,
    T: Transport + Clone,
{
    type Provider = RethDbProvider<P, T>;

    fn layer(&self, inner: P) -> Self::Provider {
        RethDbProvider::new(inner, self.db_path().clone())
    }
}

/// A provider that overrides the vanilla `Provider` trait to get results from
/// the reth-db.
///
/// It holds the `reth_provider::ProviderFactory` that enables read-only access
/// to the database tables and static files.
#[derive(Clone)]
pub struct RethDbProvider<P, T> {
    inner: P,
    filter: RethFilter,
    db_path: PathBuf,
    _pd: PhantomData<T>,
}

impl<P, T> RethDbProvider<P, T> {
    /// Create a new `RethDbProvider` instance.
    pub fn new(inner: P, db_path: PathBuf) -> Self {
        let db =
            Arc::new(open_db_read_only(db_path.join("db").as_path(), Default::default()).unwrap());
        let task_executor = TokioTaskExecutor::default();

        let chain = MAINNET.clone();
        let evm_config = EthEvmConfig::new(chain.clone());
        let provider_factory = ProviderFactory::<NodeTypesWithDBAdapter<_, Arc<DatabaseEnv>>>::new(
            Arc::clone(&db),
            Arc::clone(&chain),
            StaticFileProvider::read_only(db_path.join("static_files"), false).unwrap(),
        );

        let provider = BlockchainProvider::new(
            provider_factory.clone(),
            Arc::new(NoopBlockchainTree::default()),
        )
        .unwrap();

        let state_cache = EthStateCache::spawn_with(
            provider.clone(),
            EthStateCacheConfig::default(),
            task_executor.clone(),
            evm_config.clone(),
        );

        let filter = EthFilter::new(
            provider.clone(),
            NoopTransactionPool::default(),
            state_cache.clone(),
            EthFilterConfig::default(),
            Box::new(task_executor.clone()),
            EthTxBuilder,
        );

        Self {
            inner,
            db_path,
            filter,
            _pd: PhantomData,
        }
    }

    /// Get the DB Path
    pub fn db_path(&self) -> PathBuf {
        self.db_path.clone()
    }
}

/// Implement the `Provider` trait for the `RethDbProvider` struct.
///
/// This is where we override specific RPC methods to fetch from the reth-db.
#[async_trait]
impl<P, T> Provider<T> for RethDbProvider<P, T>
where
    P: Provider<T>,
    T: Transport + Clone,
{
    fn root(&self) -> &RootProvider<T> {
        self.inner.root()
    }

    async fn get_logs(&self, filter: &Filter) -> TransportResult<Vec<Log>> {
        let logs = self
            .filter
            .logs(filter.to_owned())
            .await
            .map_err(TransportErrorKind::custom)?;

        Ok(logs)
    }
}
