use std::sync::Arc;

use alloy::{
    providers::{Provider, ProviderBuilder, RootProvider, WsConnect},
    pubsub::PubSubFrontend,
    rpc::types::Filter,
};
use mini_alloy_reth::{layer::RethDbLayer, provider::RethDbProvider};

type RethProvider = RethDbProvider<RootProvider<PubSubFrontend>, PubSubFrontend>;

#[tokio::main]
async fn main() {
    let ws = WsConnect::new("ws://localhost:8545");
    let db_path = "/root/.local/share/reth/mainnet".into();

    let provider = Arc::new(
        ProviderBuilder::new()
            .layer(RethDbLayer::new(db_path))
            .on_ws(ws)
            .await
            .unwrap(),
    );

    batch_get_logs_from_db(provider).await;
}

async fn batch_get_logs_from_db(provider: Arc<RethProvider>) {
    let semaphore = Arc::new(tokio::sync::Semaphore::new(50));
    let mut tasks = Vec::new();

    let latest_block = provider.get_block_number().await.unwrap();

    for start in (0..latest_block).step_by(20) {
        let end = start + 20;
        let provider = provider.clone();
        let semaphore = semaphore.clone();

        let task = tokio::spawn(async move {
            let _permit = semaphore.clone().acquire_owned().await.unwrap();
            let filter = Filter::new().from_block(start).to_block(end);

            let logs = provider.get_logs(&filter).await.unwrap();
            println!("Got {} logs from block {} to {}", logs.len(), start, end);
        });

        tasks.push(task);
    }

    for task in tasks {
        task.await.unwrap();
    }
}
