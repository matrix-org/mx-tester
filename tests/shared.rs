use log::debug;
use mx_tester::Config;

use lazy_static::lazy_static;

lazy_static! {
    pub static ref DOCKER: bollard::Docker =
        bollard::Docker::connect_with_local_defaults().expect("Failed to connect to Docker daemon");
}

/// Utility trait, designed to simplify assigning a random port for a test.
pub trait AssignPort {
    /// Assign a random port for a test.
    fn assign_port(self) -> Self;
}

impl AssignPort for Config {
    /// Assign a random port for a test.
    fn assign_port(mut self) -> Self {
        use rand::Rng;
        let port = loop {
            let port = rand::thread_rng().gen_range(1025..10_000);
            if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
                debug!("This test will use port {}", port);
                break port as u64;
            }
            debug!("Port {} already occupied, looking for another", port);
        };
        self.homeserver.set_host_port(port);
        self
    }
}
