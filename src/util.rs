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