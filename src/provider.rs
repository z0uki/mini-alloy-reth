use std::{marker::PhantomData, path::PathBuf, sync::Arc, time::Duration};

use alloy::{
    eips::{eip1559::ETHEREUM_BLOCK_GAS_LIMIT, BlockId, BlockNumberOrTag},
    primitives::{Address, U64},
    providers::{Provider, ProviderCall, ProviderLayer, RootProvider, RpcWithBlock},
    rpc::types::{simulate::MAX_SIMULATE_BLOCKS, TransactionReceipt},
    transports::{Transport, TransportErrorKind},
};
use async_trait::async_trait;
use reth_beacon_consensus::EthBeaconConsensus;
use reth_blockchain_tree::{
    BlockchainTree, BlockchainTreeConfig, ShareableBlockchainTree, TreeExternals,
};
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
    BlockNumReader, DatabaseProviderFactory, ProviderError, ProviderFactory, StateProvider,
    TryIntoHistoricalStateProvider,
};
use reth_rpc::EthApi;
use reth_rpc_eth_api::helpers::EthBlocks;
use reth_rpc_eth_types::{
    EthStateCache, EthStateCacheConfig, FeeHistoryCache, FeeHistoryCacheConfig, GasPriceOracle,
    GasPriceOracleConfig,
};
use reth_rpc_server_types::constants::{DEFAULT_ETH_PROOF_WINDOW, DEFAULT_PROOF_PERMITS};
use reth_tasks::{pool::BlockingTaskPool, TokioTaskExecutor};
use reth_transaction_pool::{
    blobstore::NoopBlobStore, validate::EthTransactionValidatorBuilder, CoinbaseTipOrdering,
    EthPooledTransaction, EthTransactionValidator, Pool, TransactionValidationTaskExecutor,
};

use crate::layer::RethDbLayer;

type RethProvider = BlockchainProvider<NodeTypesWithDBAdapter<EthereumNode, Arc<DatabaseEnv>>>;
type RethApi = EthApi<RethProvider, RethTxPool, NoopNetwork, EthEvmConfig>;
type RethTxPool = Pool<
    TransactionValidationTaskExecutor<EthTransactionValidator<RethProvider, EthPooledTransaction>>,
    CoinbaseTipOrdering<EthPooledTransaction>,
    NoopBlobStore,
>;

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
    api: RethApi,
    provider_factory: DbAccessor,
    db_path: PathBuf,
    _pd: PhantomData<T>,
}

impl<P, T> RethDbProvider<P, T> {
    /// Create a new `RethDbProvider` instance.
    pub fn new(inner: P, db_path: PathBuf) -> Self {
        let args = DatabaseArguments::default()
            .with_max_read_transaction_duration(Some(MaxReadTransactionDuration::Set(
                Duration::from_secs(10),
            )))
            .with_exclusive(Some(false));

        let db = Arc::new(open_db_read_only(db_path.join("db").as_path(), args).unwrap());
        let task_executor = TokioTaskExecutor::default();

        let chain = MAINNET.clone();
        let evm_config = EthEvmConfig::new(chain.clone());
        let provider_factory = ProviderFactory::<NodeTypesWithDBAdapter<_, Arc<DatabaseEnv>>>::new(
            Arc::clone(&db),
            Arc::clone(&chain),
            StaticFileProvider::read_only(db_path.join("static_files"), true).unwrap(),
        );

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

        let api = EthApi::new(
            provider.clone(),
            tx_pool.clone(),
            NoopNetwork::default(),
            state_cache.clone(),
            GasPriceOracle::new(
                provider.clone(),
                GasPriceOracleConfig::default(),
                state_cache.clone(),
            ),
            ETHEREUM_BLOCK_GAS_LIMIT,
            MAX_SIMULATE_BLOCKS,
            DEFAULT_ETH_PROOF_WINDOW,
            blocking,
            fee_history,
            evm_config.clone(),
            DEFAULT_PROOF_PERMITS,
        );

        let db_accessor: DbAccessor<
            ProviderFactory<NodeTypesWithDBAdapter<EthereumNode, Arc<DatabaseEnv>>>,
        > = DbAccessor::new(provider_factory);

        Self {
            inner,
            db_path,
            provider_factory: db_accessor,
            _pd: PhantomData,
            api,
        }
    }

    /// Get the DB Path
    pub fn db_path(&self) -> PathBuf {
        self.db_path.clone()
    }

    const fn factory(&self) -> &DbAccessor {
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

    fn get_transaction_count(&self, address: Address) -> RpcWithBlock<T, Address, U64, u64> {
        let this = self.factory().clone();
        RpcWithBlock::new_provider(move |block_id| {
            let provider = this
                .provider_at(block_id)
                .map_err(TransportErrorKind::custom)
                .unwrap();

            let maybe_acc = provider
                .basic_account(address)
                .map_err(TransportErrorKind::custom)
                .unwrap();

            let nonce = maybe_acc.map(|acc| acc.nonce).unwrap_or_default();

            ProviderCall::ready(Ok(nonce))
        })
    }

    fn get_block_receipts(
        &self,
        block: BlockId,
    ) -> ProviderCall<T, (BlockId,), Option<Vec<TransactionReceipt>>> {
        let api = self.api.clone();
        let root = self.root().clone();
        ProviderCall::BoxedFuture(Box::pin(async move {
            match api
                .block_receipts(block)
                .await
                .map_err(TransportErrorKind::custom)
            {
                Ok(result) => {
                    if let Some(receipts) = result {
                        Ok(Some(receipts))
                    } else {
                        root.get_block_receipts(block).await
                    }
                }
                Err(e) => return Err(e),
            }
        }))
    }
}

#[derive(Debug, Clone)]
struct DbAccessor<DB = ProviderFactory<NodeTypesWithDBAdapter<EthereumNode, Arc<DatabaseEnv>>>>
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

    fn provider(&self) -> Result<DB::Provider, ProviderError> {
        self.inner.database_provider_ro()
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
