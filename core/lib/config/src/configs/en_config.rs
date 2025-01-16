use std::num::{NonZeroU64, NonZeroUsize};

use serde::Deserialize;
use zksync_basic_types::{
    commitment::L1BatchCommitmentMode, url::SensitiveUrl, L1ChainId, L2ChainId, SLChainId,
};

use crate::DAClientConfig;

/// Temporary config for initializing external node, will be completely replaced by consensus config later
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ENConfig {
    // Genesis
    pub l2_chain_id: L2ChainId,
    pub l1_chain_id: L1ChainId,
    pub l1_batch_commit_data_generator_mode: L1BatchCommitmentMode,

    // Main node configuration
    pub main_node_url: SensitiveUrl,
    pub main_node_rate_limit_rps: Option<NonZeroUsize>,

    pub bridge_addresses_refresh_interval_sec: Option<NonZeroU64>,

    pub gateway_chain_id: Option<SLChainId>,
    pub da_client: Option<DAClientConfig>, // Reused from general config
}
