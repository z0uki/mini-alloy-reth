use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use alloy::{
    providers::{Provider, ProviderBuilder, RootProvider, WsConnect},
    pubsub::PubSubFrontend,
    rpc::types::Filter,
};
use futures::StreamExt;
use futures_ratelimit::ordered::FuturesOrderedIter;
use jemalloc_ctl::{Access, AsName};
use mini_alloy_reth::{layer::RethDbLayer, provider::RethDbProvider};
use reth_chainspec::MAINNET;
use reth_db::{mdbx::DatabaseArguments, open_db_read_only, DatabaseEnv};
use reth_node_ethereum::{EthEvmConfig, EthereumNode};
use reth_node_types::NodeTypesWithDBAdapter;
use reth_provider::{
    providers::StaticFileProvider, BlockNumReader, BlockReader, ProviderFactory, ReceiptProvider,
};

type RethProvider = RethDbProvider<RootProvider<PubSubFrontend>, PubSubFrontend>;

#[global_allocator]
static ALLOC: jemallocator::Jemalloc = jemallocator::Jemalloc;

const PROF_ACTIVE: &'static [u8] = b"prof.active\0";
const PROF_DUMP: &'static [u8] = b"prof.dump\0";
const PROFILE_OUTPUT: &'static [u8] = b"profile.out\0";

fn set_prof_active(active: bool) {
    let name = PROF_ACTIVE.name();
    name.write(active).expect("Should succeed to set prof");
}

fn dump_profile() {
    let name = PROF_DUMP.name();
    name.write(PROFILE_OUTPUT)
        .expect("Should succeed to dump profile")
}

fn main() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let handle = rt.handle();
    rt.block_on(async {
        tracing_subscriber::fmt().with_env_filter("info").init();
        let ws = WsConnect::new("ws://localhost:8545");
        let db_path = "/root/.local/share/reth/mainnet".into();

        let provider = Arc::new(
            ProviderBuilder::new()
                .layer(RethDbLayer::new(db_path, handle.clone()))
                .on_ws(ws)
                .await
                .unwrap(),
        );

        batch_get_logs_from_db(provider).await;
    })
}

async fn batch_get_logs_from_db(provider: Arc<RethProvider>) {
    let latest_block = provider.get_block_number().await.unwrap();

    for number in (latest_block - 10)..latest_block {
        let receipts = provider.get_block_receipts(number.into()).await.unwrap();

        println!(
            "block: {} distance: {} receipts: {:?}",
            number,
            latest_block - number,
            receipts.map(|x| x.len())
        );
    }

    tokio::time::sleep(std::time::Duration::from_secs(12)).await;

    let receipts = provider
        .get_block_receipts(latest_block.into())
        .await
        .unwrap();

    println!("block: {} receipts: {:?}", latest_block, receipts);

    // loop {
    //     let latest_block = provider.get_block_number().await.unwrap();
    //     println!("Syncing from block {}", latest_block);
    //     if latest_block == synced {
    //         tokio::time::sleep(std::time::Duration::from_secs(6)).await;
    //         continue;
    //     }
    //     let receipts = provider.get_block_receipts(21421398.into()).await.unwrap();

    //     println!("Got {:?}", receipts.map(|x| x.len()));

    //     synced = latest_block;
    // }
}
