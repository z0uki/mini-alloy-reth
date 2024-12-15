use std::sync::Arc;

use alloy::{
    providers::{Provider, ProviderBuilder, RootProvider, WsConnect},
    pubsub::PubSubFrontend,
    rpc::types::Filter,
};
use futures::StreamExt;
use futures_ratelimit::ordered::FuturesOrderedIter;
use jemalloc_ctl::{Access, AsName};
use mini_alloy_reth::{layer::RethDbLayer, provider::RethDbProvider};

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
    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let handle = rt.handle();
    rt.block_on(async {
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
    let mut synced = 0;
    loop {
        let latest_block = provider.get_block_number().await.unwrap();
        println!("Syncing from block {}", synced);
        if latest_block == synced {
            tokio::time::sleep(std::time::Duration::from_secs(6)).await;
            continue;
        }

        let filter = Filter::new()
            .from_block(latest_block)
            .to_block(latest_block);
        let logs = provider.get_logs(&filter).await.unwrap();
        println!("Got {:?}", logs.len());

        // let receipts = provider
        //     .get_block_receipts((latest_block - 1).into())
        //     .await
        //     .unwrap();

        // println!("Got {:?}", receipts.map(|x| x.len()));

        synced = latest_block;
    }
}
