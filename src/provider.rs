use std::{marker::PhantomData, path::PathBuf, sync::Arc, time::Duration};

use alloy::{
    eips::{eip1559::ETHEREUM_BLOCK_GAS_LIMIT, BlockId},
    providers::{Provider, ProviderCall, ProviderLayer, RootProvider},
    rpc::types::{simulate::MAX_SIMULATE_BLOCKS, Filter, Log, TransactionReceipt},
    transports::{Transport, TransportErrorKind, TransportResult},
};
use async_trait::async_trait;
use reth_beacon_consensus::EthBeaconConsensus;
use reth_blockchain_tree::{
    BlockchainTree, BlockchainTreeConfig, ShareableBlockchainTree, TreeExternals,
};
use reth_chain_state::test_utils::TestCanonStateSubscriptions;
use reth_chainspec::MAINNET;
use reth_db::{
    mdbx::{DatabaseArguments, MaxReadTransactionDuration},
    open_db_read_only, DatabaseEnv,
};
use reth_network_api::noop::NoopNetwork;
use reth_node_ethereum::{EthEvmConfig, EthExecutorProvider, EthereumNode};
use reth_node_types::NodeTypesWithDBAdapter;
use reth_provider::{
    providers::{BlockchainProvider, StaticFileProvider},
    ProviderFactory,
};
use reth_rpc::{eth::EthTxBuilder, EthApi, EthFilter};
use reth_rpc_eth_api::{helpers::EthBlocks, EthFilterApiServer};
use reth_rpc_eth_types::{
    EthApiBuilderCtx, EthConfig, EthFilterConfig, EthStateCache, EthStateCacheConfig,
    FeeHistoryCache, FeeHistoryCacheConfig, GasPriceOracle, GasPriceOracleConfig,
};
use reth_rpc_server_types::constants::{DEFAULT_ETH_PROOF_WINDOW, DEFAULT_PROOF_PERMITS};
use reth_tasks::{pool::BlockingTaskPool, TaskManager, TokioTaskExecutor};
use reth_transaction_pool::{
    blobstore::NoopBlobStore, validate::EthTransactionValidatorBuilder, CoinbaseTipOrdering,
    EthPooledTransaction, EthTransactionValidator, Pool, TransactionValidationTaskExecutor,
};

use crate::layer::RethDbLayer;

type RethProvider = BlockchainProvider<NodeTypesWithDBAdapter<EthereumNode, Arc<DatabaseEnv>>>;
type RethApi = EthApi<RethProvider, RethTxPool, NoopNetwork, EthEvmConfig>;
type RethFilter = EthFilter<RethProvider, RethTxPool, RethApi>;
// type RethTrace = TraceApi<RethProvider, RethApi>;
// type RethDebug = DebugApi<RethProvider, RethApi,
// BasicBlockExecutorProvider<EthExecutionStrategyFactory>>;
type RethTxPool = Pool<
    TransactionValidationTaskExecutor<EthTransactionValidator<RethProvider, EthPooledTransaction>>,
    CoinbaseTipOrdering<EthPooledTransaction>,
    NoopBlobStore,
>;

// type RethProvider = BlockchainProvider<NodeTypesWithDBAdapter<EthereumNode,
// Arc<DatabaseEnv>>>; type RethApi = EthApi<RethProvider, RethTxPool,
// NoopNetwork, EthEvmConfig>; type RethFilter = EthFilter<RethProvider,
// RethTxPool, RethApi>; type RethTxPool = NoopTransactionPool;

/// Implement the `ProviderLayer` trait for the `RethDBLayer` struct.
impl<P, T> ProviderLayer<P, T> for RethDbLayer
where
    P: Provider<T>,
    T: Transport + Clone,
{
    type Provider = RethDbProvider<P, T>;

    fn layer(&self, inner: P) -> Self::Provider {
        RethDbProvider::new(inner, self.db_path().clone(), self.handle())
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
    // provider_factory: DbAccessor,
    api: RethApi,
    filter: RethFilter,
    db_path: PathBuf,
    _pd: PhantomData<T>,
}

impl<P, T> RethDbProvider<P, T> {
    /// Create a new `RethDbProvider` instance.
    pub fn new(inner: P, db_path: PathBuf, handle: &tokio::runtime::Handle) -> Self {
        let task_executor = TokioTaskExecutor::default();

        let args = DatabaseArguments::default();

        let db = Arc::new(open_db_read_only(db_path.join("db").as_path(), args).unwrap());

        let chain = MAINNET.clone();
        let evm_config = EthEvmConfig::new(chain.clone());
        let provider_factory = ProviderFactory::<NodeTypesWithDBAdapter<_, Arc<DatabaseEnv>>>::new(
            Arc::clone(&db),
            Arc::clone(&chain),
            StaticFileProvider::read_only(db_path.join("static_files"), true).unwrap(),
        );

        // let provider = provider_factory
        //     .provider()
        //     .map_err(TransportErrorKind::custom)
        //     .unwrap();

        let tree_externals = TreeExternals::new(
            provider_factory.clone(),
            Arc::new(EthBeaconConsensus::new(Arc::clone(&chain))),
            EthExecutorProvider::ethereum(chain.clone()),
        );

        let tree_config = BlockchainTreeConfig::default();

        let blockchain_tree =
            ShareableBlockchainTree::new(BlockchainTree::new(tree_externals, tree_config).unwrap());

        let provider =
            BlockchainProvider::new(provider_factory.clone(), Arc::new(blockchain_tree)).unwrap();

        let state_cache = EthStateCache::spawn_with(
            provider.clone(),
            EthStateCacheConfig::default(),
            task_executor.clone(),
            evm_config.clone(),
        );

        let transaction_validator = EthTransactionValidatorBuilder::new(chain.clone())
            .build_with_tasks(
                provider.clone(),
                task_executor.clone(),
                NoopBlobStore::default(),
            );

        let tx_pool = reth_transaction_pool::Pool::eth_pool(
            transaction_validator,
            NoopBlobStore::default(),
            Default::default(),
        );

        let blocking = BlockingTaskPool::build().unwrap();
        let eth_state_config = EthStateCacheConfig::default();
        let fee_history = FeeHistoryCache::new(
            EthStateCache::spawn_with(
                provider.clone(),
                eth_state_config,
                task_executor.clone(),
                evm_config.clone(),
            ),
            FeeHistoryCacheConfig::default(),
        );

        let ctx = EthApiBuilderCtx {
            provider: provider.clone(),
            pool: tx_pool.clone(),
            network: NoopNetwork::default(),
            evm_config,
            config: EthConfig::default(),
            executor: task_executor.clone(),
            events: TestCanonStateSubscriptions::default(),
            cache: state_cache.clone(),
        };

        let api = EthApi::with_spawner(&ctx);

        // let blocking_task_guard = BlockingTaskGuard::new(10000);
        // let provider_executor = EthExecutorProvider::ethereum(chain.clone());

        // let trace = TraceApi::new(provider.clone(), api.clone(),
        // blocking_task_guard.clone()); let debug =
        // DebugApi::new(provider.clone(), api.clone(), blocking_task_guard,
        // provider_executor);
        let filter = EthFilter::new(
            provider.clone(),
            tx_pool.clone(),
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
            api,
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
        self.filter
            .logs(filter.to_owned())
            .await
            .map_err(TransportErrorKind::custom)
    }

    fn get_block_receipts(
        &self,
        block: BlockId,
    ) -> ProviderCall<T, (BlockId,), Option<Vec<TransactionReceipt>>> {
        let api = self.api.clone();
        ProviderCall::BoxedFuture(Box::pin(async move {
            api.block_receipts(block)
                .await
                .map_err(TransportErrorKind::custom)
        }))
    }
}
