use std::{marker::PhantomData, path::PathBuf, sync::Arc};

use crate::layer::RethDbLayer;
use alloy::{
    eips::{BlockId, BlockNumberOrTag},
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
    BlockIdReader, BlockNumReader, BlockReader, ChainSpecProvider, DatabaseProviderFactory,
    ProviderError, ProviderFactory, ReceiptProvider, StateProvider, TryIntoHistoricalStateProvider,
};
use reth_rpc::{EthApi, EthFilter};
use reth_rpc_builder::{EthHandlers, RpcModuleBuilder};
use reth_rpc_eth_api::filter::EthFilterApiServer;
use reth_tasks::TokioTaskExecutor;
use reth_transaction_pool::noop::NoopTransactionPool;

type RethProvider = BlockchainProvider<NodeTypesWithDBAdapter<EthereumNode, Arc<DatabaseEnv>>>;
type RethApi = EthApi<RethProvider, RethTxPool, NoopNetwork, EthEvmConfig>;
type RethFilter = EthFilter<RethProvider, RethTxPool, RethApi>;
type RethTxPool = NoopTransactionPool;
type RethHandler =
    EthHandlers<RethProvider, RethTxPool, NoopNetwork, TestCanonStateSubscriptions, RethApi>;

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
    pub(crate) provider_factory: DbAccessor,
    db_path: PathBuf,
    _pd: PhantomData<T>,
}

impl<P, T> RethDbProvider<P, T> {
    /// Create a new `RethDbProvider` instance.
    pub fn new(inner: P, db_path: PathBuf) -> Self {
        let db =
            Arc::new(open_db_read_only(db_path.join("db").as_path(), Default::default()).unwrap());

        let chain = MAINNET.clone();
        let provider_factory =
            ProviderFactory::<NodeTypesWithDBAdapter<EthereumNode, Arc<DatabaseEnv>>>::new(
                db,
                chain,
                StaticFileProvider::read_only(db_path.join("static_files"), true).unwrap(),
            );

        // let provider = BlockchainProvider::new(
        //     provider_factory.clone(),
        //     Arc::new(NoopBlockchainTree::default()),
        // )
        // .unwrap();
        // let spec = Arc::new(ChainSpecBuilder::mainnet().build());
        // let rpc_builder = RpcModuleBuilder::default()
        //     .with_provider(provider.clone())
        //     // Rest is just noops that do nothing
        //     .with_noop_pool()
        //     .with_noop_network()
        //     .with_executor(TokioTaskExecutor::default())
        //     .with_evm_config(EthEvmConfig::new(spec.clone()))
        //     .with_events(TestCanonStateSubscriptions::default())
        //     .with_consensus(EthBeaconConsensus::new(spec))
        //     .with_block_executor(EthExecutorProvider::ethereum(provider.chain_spec()));

        // let registry =
        //     rpc_builder.into_registry(Default::default(), Box::new(EthApi::with_spawner));

        let db_accessor: DbAccessor<
            ProviderFactory<NodeTypesWithDBAdapter<EthereumNode, Arc<DatabaseEnv>>>,
        > = DbAccessor::new(provider_factory);

        Self {
            inner,
            db_path,
            provider_factory: db_accessor,
            _pd: PhantomData,
        }
    }

    /// Get the DB Path
    pub fn db_path(&self) -> PathBuf {
        self.db_path.clone()
    }

    pub const fn factory(&self) -> &DbAccessor {
        &self.provider_factory
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
        Ok(self
            .internal_logs(filter.to_owned())
            .await
            .map_err(|e| TransportErrorKind::custom_str(&e.to_string()))?)
    }
}

#[derive(Debug, Clone)]
pub struct DbAccessor<DB = ProviderFactory<NodeTypesWithDBAdapter<EthereumNode, Arc<DatabaseEnv>>>>
where
    DB: DatabaseProviderFactory<Provider: TryIntoHistoricalStateProvider + BlockNumReader>,
{
    inner: DB,
}

impl<DB> DbAccessor<DB>
where
    DB: DatabaseProviderFactory<Provider: TryIntoHistoricalStateProvider + BlockNumReader>,
{
    const fn new(inner: DB) -> Self {
        Self { inner }
    }

    pub fn provider(&self) -> Result<Arc<DB::Provider>, ProviderError> {
        self.inner.database_provider_ro().map(Arc::new)
    }

    fn provider_at(&self, block_id: BlockId) -> Result<Box<dyn StateProvider>, ProviderError> {
        let provider = self.inner.database_provider_ro()?;

        let block_number = match block_id {
            BlockId::Hash(hash) => {
                if let Some(num) = provider.block_number(hash.into())? {
                    num
                } else {
                    return Err(ProviderError::BlockHashNotFound(hash.into()));
                }
            }
            BlockId::Number(BlockNumberOrTag::Number(num)) => num,
            _ => provider.best_block_number()?,
        };

        provider.try_into_history_at_block(block_number)
    }
}
