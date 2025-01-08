use anyhow::Context;
use xshell::{cmd, Shell};
use zkstack_cli_common::{
    cmd::Cmd,
    config::global_config,
    logger,
    server::{Server, ServerMode},
};
use zkstack_cli_config::{
    traits::FileConfigWithDefaultName, ChainConfig, ContractsConfig, EcosystemConfig,
    WalletsConfig, GENERAL_FILE, GENESIS_FILE, SECRETS_FILE,
};
use zksync_basic_types::SLChainId;

use crate::{
    commands::args::{RunServerArgs, ServerArgs, ServerCommand, WaitArgs},
    messages::{
        msg_waiting_for_server_success, MSG_BUILDING_SERVER, MSG_CHAIN_NOT_INITIALIZED,
        MSG_FAILED_TO_BUILD_SERVER_ERR, MSG_FAILED_TO_RUN_SERVER_ERR, MSG_STARTING_SERVER,
        MSG_WAITING_FOR_SERVER,
    },
};

pub async fn run(shell: &Shell, args: ServerArgs) -> anyhow::Result<()> {
    let ecosystem_config = EcosystemConfig::from_file(shell)?;
    let chain_config = ecosystem_config
        .load_current_chain()
        .context(MSG_CHAIN_NOT_INITIALIZED)?;

    match ServerCommand::from(args) {
        ServerCommand::Run(args) => run_server(args, &chain_config, shell).await,
        ServerCommand::Build => build_server(&chain_config, shell),
        ServerCommand::Wait(args) => wait_for_server(args, &chain_config).await,
    }
}

fn build_server(chain_config: &ChainConfig, shell: &Shell) -> anyhow::Result<()> {
    let _dir_guard = shell.push_dir(chain_config.link_to_code.join("core"));

    logger::info(MSG_BUILDING_SERVER);

    let mut cmd = Cmd::new(cmd!(shell, "cargo build --release --bin zksync_server"));
    cmd = cmd.with_force_run();
    cmd.run().context(MSG_FAILED_TO_BUILD_SERVER_ERR)
}

async fn run_server(
    args: RunServerArgs,
    chain_config: &ChainConfig,
    shell: &Shell,
) -> anyhow::Result<()> {
    logger::info(MSG_STARTING_SERVER);
    let server = Server::new(
        args.components.clone(),
        chain_config.link_to_code.clone(),
        args.uring,
    );

    let mode = if args.genesis {
        ServerMode::Genesis
    } else {
        ServerMode::Normal
    };

    let gateway_config = chain_config.get_gateway_chain_config().await.ok();
    let mut gateway_contracts = None;
    if let Some(gateway_config) = &gateway_config {
        let gateway_chain_id: SLChainId = gateway_config.get("gateway_chain_id")?;
        gateway_contracts = if gateway_chain_id != SLChainId(0) {
            Some(chain_config.path_to_gateway_chain_config())
        } else {
            None
        };
    }

    server
        .run(
            shell,
            mode,
            chain_config.configs.join(GENESIS_FILE),
            WalletsConfig::get_path_with_base_path(&chain_config.configs),
            chain_config.configs.join(GENERAL_FILE),
            chain_config.configs.join(SECRETS_FILE),
            ContractsConfig::get_path_with_base_path(&chain_config.configs),
            gateway_contracts,
            vec![],
        )
        .context(MSG_FAILED_TO_RUN_SERVER_ERR)
}

async fn wait_for_server(args: WaitArgs, chain_config: &ChainConfig) -> anyhow::Result<()> {
    let verbose = global_config().verbose;

    let health_check_port = chain_config
        .get_general_config()
        .await?
        .get("api.healthcheck.port")?;
    logger::info(MSG_WAITING_FOR_SERVER);
    args.poll_health_check(health_check_port, verbose).await?;
    logger::info(msg_waiting_for_server_success(health_check_port));
    Ok(())
}
