//! The shared host contract on the executor of the command-line host: a
//! Tokio current-thread runtime that awaits the host future directly.

use std::time::Duration;

use super::LocalFuture;
use crate::contract::{self, LocalExecutor};

struct CommandLine(tokio::runtime::Runtime);

impl LocalExecutor for CommandLine {
    fn run(&mut self, host: LocalFuture<()>, script: LocalFuture<()>) {
        self.0.block_on(async {
            tokio::time::timeout(
                Duration::from_secs(10),
                futures_util::future::join(host, script),
            )
            .await
            .expect("the scenario stopped making progress");
        });
    }
}

#[test]
fn shared_host_contract_on_the_command_line_executor() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    contract::run_all(&mut CommandLine(runtime));
}
