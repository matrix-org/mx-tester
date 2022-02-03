use std::sync::Arc;
use log::debug;
use crate::Config;

/// Cleanup any Docker images at the end of the test,
/// even in case of panic.
///
/// To use it, you need to prefix tests with
/// `#[tokio::test(flavor = "multi_thread")]`
pub struct Cleanup {
    is_armed: bool,
    setup_container_name: Arc<str>,
    run_container_name: Arc<str>,
}
impl Cleanup {
    pub fn new(config: &Config) -> Self {
        Cleanup {
            is_armed: true,
            setup_container_name: config.setup_container_name().into(),
            run_container_name: config.run_container_name().into(),
        }
    }
    pub fn disarm(mut self) {
        self.is_armed = false;
    }
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        if !self.is_armed {
            return;
        }
        let docker = bollard::Docker::connect_with_local_defaults().expect("Failed to connect to Docker daemon");
        let setup_container_name = self.setup_container_name.clone();
        let run_container_name = self.run_container_name.clone();
        tokio::task::block_in_place(move || {
            tokio::runtime::Handle::current().block_on(async move {
                debug!("Test cleanup...");
                let _ = docker.stop_container(&setup_container_name, None).await;
                let _ = docker.remove_container(&setup_container_name, None).await;
                let _ = docker.stop_container(&run_container_name, None).await;
                let _ = docker.remove_container(&run_container_name, None).await;
                debug!("Test cleanup... DONE");
            });
        });
    }
}

pub trait Disarm {
    fn disarm(self);
}

impl Disarm for Option<Cleanup> {
    fn disarm(self) {
        if let Some(cleanup) = self {
            cleanup.disarm();
        }
    }
}
