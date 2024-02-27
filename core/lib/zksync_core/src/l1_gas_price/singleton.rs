use std::sync::Arc;

use anyhow::Context as _;
use tokio::{
    sync::{watch, OnceCell},
    task::JoinHandle,
};
use zksync_config::GasAdjusterConfig;
use zksync_eth_client::clients::QueryClient;

use crate::{l1_gas_price::GasAdjuster, native_token_fetcher::ConversionRateFetcher};

/// Special struct for creating a singleton of `GasAdjuster`.
/// This is needed only for running the server.
#[derive(Debug)]
pub struct GasAdjusterSingleton {
    web3_url: String,
    gas_adjuster_config: GasAdjusterConfig,
    singleton: OnceCell<Result<Arc<GasAdjuster<QueryClient>>, Error>>,
    erc20_fetcher_dyn: Option<Arc<dyn ConversionRateFetcher>>,
}

#[derive(thiserror::Error, Debug, Clone)]
#[error(transparent)]
pub struct Error(Arc<anyhow::Error>);

impl From<anyhow::Error> for Error {
    fn from(err: anyhow::Error) -> Self {
        Self(Arc::new(err))
    }
}

impl GasAdjusterSingleton {
    pub fn new(
        web3_url: String,
        gas_adjuster_config: GasAdjusterConfig,
        erc20_fetcher_dyn: Option<Arc<dyn ConversionRateFetcher>>,
    ) -> Self {
        Self {
            web3_url,
            gas_adjuster_config,
            singleton: OnceCell::new(),
            erc20_fetcher_dyn,
        }
    }

    pub async fn get_or_init(&mut self) -> Result<Arc<GasAdjuster<QueryClient>>, Error> {
        let adjuster = self
            .singleton
            .get_or_init(|| async {
                let query_client =
                    QueryClient::new(&self.web3_url).context("QueryClient::new()")?;
                let adjuster = GasAdjuster::new(
                    query_client.clone(),
                    self.gas_adjuster_config,
                    self.erc20_fetcher_dyn.clone(),
                )
                .await
                .context("GasAdjuster::new()")?;
                Ok(Arc::new(adjuster))
            })
            .await;
        adjuster.clone()
    }

    pub fn run_if_initialized(
        self,
        stop_signal: watch::Receiver<bool>,
    ) -> Option<JoinHandle<anyhow::Result<()>>> {
        let gas_adjuster = self.singleton.get()?.clone();
        Some(tokio::spawn(
            async move { gas_adjuster?.run(stop_signal).await },
        ))
    }
}
