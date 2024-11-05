use std::{marker::PhantomData, path::PathBuf, sync::Arc};

use crate::layer::RethDbLayer;
use alloy::{
    providers::{Provider, ProviderLayer, RootProvider},
    rpc::types::{Filter, Log},
    transports::{Transport, TransportErrorKind, TransportResult},
};
use async_trait::async_trait;
use reth_beacon_consensus::EthBeaconConsensus;
use reth_blockchain_tree::noop::NoopBlockchainTree;
use reth_chain_state::test_utils::TestCanonStateSubscriptions;
use reth_chainspec::{ChainSpecBuilder, MAINNET};
use reth_db::{open_db_read_only, DatabaseEnv};
use reth_network_api::noop::NoopNetwork;
use reth_node_ethereum::{EthEvmConfig, EthExecutorProvider, EthereumNode};
use reth_node_types::NodeTypesWithDBAdapter;
use reth_provider::{
    providers::{BlockchainProvider, StaticFileProvider},
    ChainSpecProvider, ProviderFactory,
};
use reth_rpc::{EthApi, EthFilter};
use reth_rpc_builder::{RpcModuleBuilder, TransportRpcModuleConfig};
use reth_rpc_eth_api::filter::EthFilterApiServer;
use reth_rpc_server_types::RethRpcModule;
use reth_tasks::TokioTaskExecutor;
use reth_transaction_pool::noop::NoopTransactionPool;

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
pub struct RethDbProvider<P, T> {
    inner: P,
    filter: Arc<RethFilter>,
    db_path: PathBuf,
    _pd: PhantomData<T>,
}

impl<P, T> RethDbProvider<P, T> {
    /// Create a new `RethDbProvider` instance.
    pub fn new(inner: P, db_path: PathBuf) -> Self {
        let db =
            Arc::new(open_db_read_only(db_path.join("db").as_path(), Default::default()).unwrap());

        let chain = MAINNET.clone();
        let provider_factory = ProviderFactory::<NodeTypesWithDBAdapter<_, Arc<DatabaseEnv>>>::new(
            db,
            chain,
            StaticFileProvider::read_only(db_path.join("static_files"), true).unwrap(),
        );

        let provider = BlockchainProvider::new(
            provider_factory.clone(),
            Arc::new(NoopBlockchainTree::default()),
        )
        .unwrap();
        let spec = Arc::new(ChainSpecBuilder::mainnet().build());
        let rpc_builder = RpcModuleBuilder::default()
            .with_provider(provider.clone())
            // Rest is just noops that do nothing
            .with_noop_pool()
            .with_noop_network()
            .with_executor(TokioTaskExecutor::default())
            .with_evm_config(EthEvmConfig::new(spec.clone()))
            .with_events(TestCanonStateSubscriptions::default())
            .with_block_executor(EthExecutorProvider::ethereum(provider.chain_spec()))
            .with_consensus(EthBeaconConsensus::new(spec));

        let registry =
            rpc_builder.into_registry(Default::default(), Box::new(EthApi::with_spawner));

        // // Pick which namespaces to expose.
        // let config = TransportRpcModuleConfig::default().with_http([RethRpcModule::Eth]);
        // let mut server = rpc_builder.build(config, Box::new(EthApi::with_spawner));

        // let state_cache = EthStateCache::spawn_with(
        //     provider.clone(),
        //     EthStateCacheConfig::default(),
        //     task_executor.clone(),
        //     evm_config.clone(),
        // );

        // let filter = EthFilter::new(
        //     provider.clone(),
        //     NoopTransactionPool::default(),
        //     state_cache.clone(),
        //     EthFilterConfig::default(),
        //     Box::new(task_executor.clone()),
        //     EthTxBuilder,
        // );

        Self {
            inner,
            db_path,
            filter: Arc::new(registry.eth_handlers().filter.clone()),
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
