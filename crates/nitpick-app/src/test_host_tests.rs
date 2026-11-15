//! The shared host contract on the executor of the Qt host: `gui_executor`
//! with a simulated event loop in place of the Qt event loop.

use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use nitpick_test_host::LocalFuture;
use nitpick_test_host::contract::{self, LocalExecutor};

use crate::gui_executor::{self, Post};
use crate::qrhi_driver::tests::ChannelPost;

struct GuiLoop;

impl LocalExecutor for GuiLoop {
    fn run(&mut self, host: LocalFuture<()>, script: LocalFuture<()>) {
        let (tx, jobs) = mpsc::channel();
        let post: Arc<dyn Post> = Arc::new(ChannelPost(Mutex::new(tx)));
        let host = gui_executor::spawn_local(Arc::clone(&post), host);
        let script = gui_executor::spawn_local(post, script);
        // One job per event loop pass. No job left: both tasks wait.
        while !(host.is_finished() && script.is_finished()) {
            let job = jobs
                .try_recv()
                .expect("the scenario stopped making progress");
            job();
        }
    }
}

#[test]
fn shared_host_contract_on_the_qt_executor() {
    contract::run_all(&mut GuiLoop);
}
