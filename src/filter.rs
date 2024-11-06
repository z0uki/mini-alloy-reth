use std::{iter::StepBy, ops::RangeInclusive};

use alloy::{
    eips::BlockNumHash,
    rpc::types::{Filter, FilterBlockOption, FilteredParams, Log},
};
use eyre::{OptionExt, Result};
use reth_chainspec::ChainInfo;
use reth_provider::{
    BlockHashReader, BlockIdReader, BlockNumReader, HeaderProvider, ReceiptProvider,
};
use reth_rpc_eth_types::logs_utils::{self, append_matching_block_logs, ProviderOrBlock};

use crate::provider::RethDbProvider;

impl<P, T> RethDbProvider<P, T> {
    pub async fn internal_logs(&self, filter: Filter) -> Result<Vec<Log>> {
        self.logs_for_filter(filter).await
    }

    pub async fn logs_for_filter(&self, filter: Filter) -> Result<Vec<Log>> {
        match filter.block_option {
            FilterBlockOption::AtBlockHash(block_hash) => {
                unimplemented!()
            }
            FilterBlockOption::Range {
                from_block,
                to_block,
            } => {
                // compute the range
                let info = self.provider.chain_info()?;

                // we start at the most recent block if unset in filter
                let start_block = info.best_number;
                let from = from_block
                    .map(|num| self.provider.convert_block_number(num))
                    .transpose()?
                    .flatten();
                let to = to_block
                    .map(|num| self.provider.convert_block_number(num))
                    .transpose()?
                    .flatten();
                let (from_block_number, to_block_number) =
                    logs_utils::get_filter_block_range(from, to, start_block, info);
                self.get_logs_in_block_range(&filter, from_block_number, to_block_number, info)
                    .await
            }
        }
    }

    pub async fn get_logs_in_block_range(
        &self,
        filter: &Filter,
        from_block: u64,
        to_block: u64,
        chain_info: ChainInfo,
    ) -> Result<Vec<Log>> {
        if to_block < from_block {
            return Err(eyre::eyre!(
                "to_block must be greater than or equal to from_block"
            ));
        }

        let mut all_logs = Vec::new();
        let filter_params = FilteredParams::new(Some(filter.clone()));

        // // derive bloom filters from filter input, so we can check headers for matching logs
        let address_filter = FilteredParams::address_filter(&filter.address);
        let topics_filter = FilteredParams::topics_filter(&filter.topics);

        // // loop over the range of new blocks and check logs if the filter matches the log's bloom
        // // filter
        for (from, to) in BlockRangeInclusiveIter::new(from_block..=to_block, 1000) {
            let headers = self.provider.headers_range(from..=to)?;

            // for (idx, header) in headers.iter().enumerate() {
            //     // only if filter matches
            //     if FilteredParams::matches_address(header.logs_bloom, &address_filter)
            //         && FilteredParams::matches_topics(header.logs_bloom, &topics_filter)
            //     {
            //         let block_hash = match headers.get(idx + 1) {
            //             Some(parent) => parent.parent_hash,
            //             None => self
            //                 .provider
            //                 .block_hash(header.number)?
            //                 .ok_or_eyre("header not found")?,
            //         };

            //         let num_hash = BlockNumHash::new(header.number, block_hash);

            //         // if let Some(receipts) = self
            //         //     .provider
            //         //     .receipts_by_block(num_hash.hash.into())
            //         //     .map_err(|_| eyre::eyre!("failed to get receipts for block"))?
            //         // {
            //         //     append_matching_block_logs(
            //         //         &mut all_logs,
            //         //         ProviderOrBlock::Provider(&self.provider),
            //         //         &filter_params,
            //         //         num_hash,
            //         //         &receipts,
            //         //         false,
            //         //         header.timestamp,
            //         //     )?;
            //         // }
            //     }
            // }
        }

        Ok(all_logs)
    }
}

/// An iterator that yields _inclusive_ block ranges of a given step size
#[derive(Debug)]
struct BlockRangeInclusiveIter {
    iter: StepBy<RangeInclusive<u64>>,
    step: u64,
    end: u64,
}

impl BlockRangeInclusiveIter {
    fn new(range: RangeInclusive<u64>, step: u64) -> Self {
        Self {
            end: *range.end(),
            iter: range.step_by(step as usize + 1),
            step,
        }
    }
}

impl Iterator for BlockRangeInclusiveIter {
    type Item = (u64, u64);

    fn next(&mut self) -> Option<Self::Item> {
        let start = self.iter.next()?;
        let end = (start + self.step).min(self.end);
        if start > end {
            return None;
        }
        Some((start, end))
    }
}
