use clap::Parser;
use serde::{Deserialize, Serialize};

use crate::commands::dev::messages::{
    MSG_NO_DEPS_HELP, MSG_TESTS_EXTERNAL_NODE_HELP, MSG_TEST_PATTERN_HELP, MSG_TEST_TIMEOUT_HELP,
};

#[derive(Debug, Serialize, Deserialize, Parser)]
pub struct IntegrationArgs {
    #[clap(short, long, help = MSG_TESTS_EXTERNAL_NODE_HELP)]
    pub external_node: bool,
    #[clap(short, long, help = MSG_NO_DEPS_HELP)]
    pub no_deps: bool,
    #[clap(short, long, help = MSG_TEST_PATTERN_HELP, allow_hyphen_values(true))]
    pub test_pattern: Option<String>,
    #[clap(short, long, help = MSG_TEST_TIMEOUT_HELP)]
    pub timeout: Option<u64>,
}
