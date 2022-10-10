use async_trait::async_trait;
use log::debug;
use rand::Rng;

/// A generic syntax for dict-like structures.
///
/// Works for HashMap but also for e.g. serde_json or serde_yaml maps.
///
/// ```rust
/// # #[macro_use] extern crate mx_tester;
/// # fn main() {
///
/// use std::collections::HashMap;
///
/// let empty: HashMap<u8, u8> = dict!(HashMap::new(), {});
/// assert_eq!(empty.len(), 0);
///
/// let map: HashMap<u8, u8> = dict!(HashMap::new(), {
///    0 => 255,
///    1 => 254,
///    2 => 253,
/// });
/// assert_eq!(map.len(), 3);
/// assert!(matches!(map.get(&0), Some(255)));
/// assert!(matches!(map.get(&1), Some(254)));
/// assert!(matches!(map.get(&2), Some(253)));
///
/// # }
/// ```
#[macro_export]
macro_rules! dict {
    // Empty
    ( $container: expr, {}) => {
        $container
    };
    // Without trailing `,`.
    ( $container: expr, { $( $k:expr => $v:expr ),+ } ) => {
        dict!($container, { $($k => $v,)* })
    };
    // With trailing `,`.
    ( $container: expr, { $( $k:expr => $v:expr ),+, } ) => {
        {
            let mut container = $container;
            $(
                container.insert($k.into(), $v.into());
            )*
            container
        }
    };
}

/// A generic syntax for seq-like structures.
///
/// Works for Vec but also for serde_json or serde_yaml arrays.
///
/// ```rust
/// # #[macro_use] extern crate mx_tester;
/// # fn main() {
///
/// use std::collections::HashMap;
///
/// let empty: Vec<u8> = seq!(Vec::new(), []);
/// assert_eq!(empty.len(), 0);
///
/// let vec: Vec<u8> = seq!(Vec::new(), [
///    255,
///    254,
///    253,
/// ]);
/// assert_eq!(vec.len(), 3);
/// assert!(matches!(vec.get(0), Some(255)));
/// assert!(matches!(vec.get(1), Some(254)));
/// assert!(matches!(vec.get(2), Some(253)));
///
/// # }
/// ```
#[macro_export]
macro_rules! seq {
    // Empty
    ( $container: expr, []) => {
        $container
    };
    // Without trailing `,`.
    ( $container: expr, [ $( $v:expr ),+ ] ) => {
        seq!($container, [$($v,)* ])
    };
    // With trailing `,`.
    ( $container: expr, [ $( $v:expr ),+, ] ) => {
        #[allow(clippy::vec_init_then_push)]
        {
            let mut container = $container;
            $(
                container.push($v.into());
            )*
            container
        }
    };
}

/// A lightweight syntax for YAML.
///
/// ```rust
/// # #[macro_use] extern crate mx_tester;
/// # fn main() {
///
/// use serde_yaml;
///
/// let empty_map = yaml!({});
/// assert!(empty_map.as_mapping().is_some());
/// assert!(empty_map.as_mapping().unwrap().is_empty());
///
/// let empty_seq = yaml!([]);
/// assert!(empty_seq.as_sequence().is_some());
/// assert!(empty_seq.as_sequence().unwrap().is_empty());
///
/// let five = yaml!(5);
/// assert!(matches!(five.as_u64(), Some(5)));
///
/// let ten = yaml!(10);
///
/// let simple_map = yaml!({
///     5 => 10 // No trailing comma
/// });
/// assert!(simple_map.as_mapping().is_some());
/// assert_eq!(simple_map.as_mapping().unwrap().len(), 1);
/// assert_eq!(simple_map.as_mapping().unwrap().get(&five).unwrap(), &ten);
///
/// let simple_map_2 = yaml!({
///     5 => 10, // Trailing comma
/// });
/// assert_eq!(simple_map_2, simple_map);
///
/// let nested_map = yaml!({
///     5 => 10,
///     10 => yaml!({ }),
/// });
/// let nested_map_2 = yaml!({
///     10 => yaml!({ }),
///     5 => 10
/// });
/// assert_eq!(nested_map, nested_map_2);
///
/// let seq = yaml!([ 5, 5, 10 ]);
/// assert!(seq.as_sequence().is_some());
/// assert_eq!(seq[0], five);
/// assert_eq!(seq[1], five);
/// assert_eq!(seq[2], ten);
/// assert!(seq[3].is_null());
///
/// # }
/// ```
#[macro_export]
macro_rules! yaml {
    // Map: empty
    ({}) => {
        serde_yaml::Value::Mapping(dict!(serde_yaml::Mapping::new(), {}))
    };
    // Map: without trailing `,`.
    ({ $( $k:expr => $v:expr ),+ } ) => {
        serde_yaml::Value::Mapping(dict!(serde_yaml::Mapping::new(), { $($k => $v,)* }))
    };
    // Map: with trailing `,`.
    ({ $( $k:expr => $v:expr ),+, } ) => {
        serde_yaml::Value::Mapping(dict!(serde_yaml::Mapping::new(), { $($k => $v,)* }))
    };
    // Sequence: empty
    ([]) => {
        serde_yaml::Value::Sequence(seq!(serde_yaml::Sequence::new(), []))
    };
    // Sequence: without trailing `,`.
    ( [ $( $v:expr ),+ ] ) => {
        serde_yaml::Value::Sequence(seq!(serde_yaml::Sequence::new(), [$($v,)* ]))
    };
    // Sequence: with trailing `,`.
    ( [ $( $v:expr ),+, ] ) => {
        serde_yaml::Value::Sequence(seq!(serde_yaml::Sequence::new(), [$($v,)* ]))
    };
    // Anything else: convert to YAML.
    ( $v:expr ) => {
        serde_yaml::Value::from($v)
    }
}

/// Utility extensions to manipulate yaml.
pub trait YamlExt {
    /// Convert a yaml subtree into a sequence.
    ///
    /// This works only if the yaml subtree is either null or already a sequence.
    fn to_seq_mut(&mut self) -> Option<&mut serde_yaml::Sequence>;
}
impl YamlExt for serde_yaml::Value {
    /// Convert a yaml subtree into a sequence.
    ///
    /// This works only if the yaml subtree is either null or already a sequence.
    fn to_seq_mut(&mut self) -> Option<&mut serde_yaml::Sequence> {
        if self.is_null() {
            *self = yaml!([]);
        }
        self.as_sequence_mut()
    }
}

/// Utility function: return `true`.
pub fn true_() -> bool {
    true
}

pub trait AsRumaError {
    fn as_ruma_error(&self) -> Option<&matrix_sdk::ruma::api::client::Error>;
}
impl AsRumaError for matrix_sdk::HttpError {
    fn as_ruma_error(&self) -> Option<&matrix_sdk::ruma::api::client::Error> {
        match *self {
            matrix_sdk::HttpError::Api(
                matrix_sdk::ruma::api::error::FromHttpResponseError::Server(
                    matrix_sdk::ruma::api::error::ServerError::Known(
                        matrix_sdk::RumaApiError::ClientApi(ref err),
                    ),
                ),
            ) => Some(err),
            _ => None,
        }
    }
}
impl AsRumaError for matrix_sdk::Error {
    fn as_ruma_error(&self) -> Option<&matrix_sdk::ruma::api::client::Error> {
        match *self {
            matrix_sdk::Error::Http(ref err) => err.as_ruma_error(),
            _ => None,
        }
    }
}

#[async_trait]
pub trait Retry {
    async fn auto_retry(&self, attempts: u64) -> Result<reqwest::Response, anyhow::Error>;
}

#[async_trait]
impl Retry for reqwest::RequestBuilder {
    async fn auto_retry(&self, max_attempts: u64) -> Result<reqwest::Response, anyhow::Error> {
        /// The duration of the retry will be picked randomly within this interval,
        /// plus an exponential backoff.
        const BASE_INTERVAL_MS: std::ops::Range<u64> = 300..1000;

        let mut attempt = 1;
        loop {
            match self
                .try_clone()
                .expect("Cannot auto-retry non-clonable requests")
                .send()
                .await
            {
                Ok(response) => {
                    debug!("auto_retry success");
                    break Ok(response);
                }
                Err(err) => {
                    debug!("auto_retry error {:?} => {:?}", err, err.status());
                    // FIXME: Is this the right way to decide when to retry?
                    let should_retry = attempt < max_attempts
                        && (err.is_connect() || err.is_timeout() || err.is_request());

                    if should_retry {
                        let duration =
                            (attempt * attempt) * rand::thread_rng().gen_range(BASE_INTERVAL_MS);
                        attempt += 1;
                        debug!("auto_retry: sleeping {}ms", duration);
                        tokio::time::sleep(std::time::Duration::from_millis(duration)).await;
                    } else {
                        debug!("auto_retry: giving up!");
                        return Err(err.into());
                    }
                }
            }
        }
    }
}
