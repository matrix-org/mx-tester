use crate::Config;
use log::warn;
use std::sync::Arc;

/// Cleanup any Docker images at the end of a block,
/// even in case of panic.
///
/// This reduces the chances that Docker will assume
/// that the images must be restarted on your next computer
/// startup and decide to bring them up as `root`.
///
/// As a side-effect, all tests MUST be prefixed with
/// `#[tokio::test(flavor = "multi_thread")]`
pub struct Cleanup {
    /// If `true`, cleanup is still needed.
    is_armed: bool,

    /// The container name used during `build`.
    setup_container_name: Arc<str>,

    /// The container name used during `up` and `run`.
    run_container_name: Arc<str>,

    /// The network to which this container is attached.
    network_name: Arc<str>,

    /// If `true`, during cleanup, also take down the network.
    /// `false` by default.
    cleanup_network: bool,
}
impl Cleanup {
    pub fn new(config: &Config) -> Self {
        Cleanup {
            is_armed: true,
            setup_container_name: config.setup_container_name().into(),
            run_container_name: config.run_container_name().into(),
            network_name: config.network().into(),
            cleanup_network: false,
        }
    }

    /// Enable or disable network cleanup.
    ///
    /// `false` by default.
    ///
    /// Note that `disarm()` prevents *all* cleanup, regardless of `cleanup_network()`.
    pub fn cleanup_network(&mut self, value: bool) {
        self.cleanup_network = value;
    }

    /// Disarm this guard.
    ///
    /// Once disarmed, it will not cause cleanup anymore when it leaves scope.
    pub fn disarm(mut self) {
        self.is_armed = false;
    }
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        if !self.is_armed {
            return;
        }
        let docker = bollard::Docker::connect_with_local_defaults()
            .expect("Failed to connect to Docker daemon");
        let setup_container_name = self.setup_container_name.clone();
        let run_container_name = self.run_container_name.clone();
        let network_name = self.network_name.clone();
        let cleanup_network = self.cleanup_network;
        tokio::task::block_in_place(move || {
            tokio::runtime::Handle::current().block_on(async move {
                warn!("Auto-cleanup...");
                let _ = docker.stop_container(&setup_container_name, None).await;
                let _ = docker.remove_container(&setup_container_name, None).await;
                let _ = docker.stop_container(&run_container_name, None).await;
                let _ = docker.remove_container(&run_container_name, None).await;
                if cleanup_network {
                    let _ = docker.remove_network(&network_name).await;
                }
                warn!("Auto-cleanup... DONE");
            });
        });
    }
}

/// A utility trait used to call `foo.disarm()` on a container
/// holding an instance of `Cleanup`.
///
/// The main utility at the time of this writing is to be able
/// to disarm a `Option<Cleanup>`.
pub trait Disarm {
    fn disarm(self);
}
impl<T> Disarm for T
where
    T: IntoIterator<Item = Cleanup>,
{
    fn disarm(self) {
        for item in self {
            // In case of panic during a call to `disarm()`,
            // the remaining items will be auto-cleaned up.
            item.disarm();
        }
    }
}
