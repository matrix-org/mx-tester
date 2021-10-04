use serde::Deserialize;
/// An optional configuration to setup a postgres container that is networked with synapse.
#[derive(Debug, Deserialize)]
pub struct PostgresConfig {
    /// Any ports to expose in the format of pppp:pppp (host:guest) like docker
    ports: Vec<String>,

    /// Any volumes to mount, in the format of host:guest.
    volumes: Vec<String>,
}
