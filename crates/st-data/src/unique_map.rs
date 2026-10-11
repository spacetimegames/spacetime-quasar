//! A map that rejects duplicate keys when it is loaded.

use core::fmt;
use core::marker::PhantomData;
use std::collections::BTreeMap;

use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
use serde::{Serialize, Serializer};

/// Marks the custom error that `UniqueMap` raises so that the loader can
/// turn it into a `duplicate-key` diagnostic. `ron` carries custom serde
/// errors as plain messages, so a tag in the message is the only channel.
pub(crate) const DUPLICATE_KEY_TAG: &str = "\u{1}st-data:duplicate-key";

/// A `BTreeMap` that rejects a duplicate key while it is deserialised.
///
/// `ron` keeps the last value of a repeated key without an error, which
/// would let a pasted-twice entry silently replace the first. Schema types
/// use `UniqueMap` for every map; `HashMap`, `HashSet` and other containers
/// with unspecified iteration order are not allowed in schema types.
/// Iteration order is the key order, so nothing downstream depends on the
/// order in the file.
///
/// # Examples
///
/// ```
/// use st_data::UniqueMap;
///
/// let mut map = UniqueMap::new();
/// map.insert("b", 2);
/// map.insert("a", 1);
/// let keys: Vec<_> = map.keys().copied().collect();
/// assert_eq!(keys, ["a", "b"]);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct UniqueMap<K: Ord, V>(BTreeMap<K, V>);

impl<K: Ord, V> UniqueMap<K, V> {
    /// An empty map.
    #[must_use]
    pub fn new() -> Self {
        Self(BTreeMap::new())
    }

    /// Inserts a value, returning the previous value for the key, if any.
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        self.0.insert(key, value)
    }

    /// The underlying ordered map.
    #[must_use]
    pub fn as_map(&self) -> &BTreeMap<K, V> {
        &self.0
    }

    /// Unwraps the underlying ordered map.
    #[must_use]
    pub fn into_map(self) -> BTreeMap<K, V> {
        self.0
    }
}

impl<K: Ord, V> Default for UniqueMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Ord, V> core::ops::Deref for UniqueMap<K, V> {
    type Target = BTreeMap<K, V>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<K: Ord, V> FromIterator<(K, V)> for UniqueMap<K, V> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl<K: Ord + Serialize, V: Serialize> Serialize for UniqueMap<K, V> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de, K, V> Deserialize<'de> for UniqueMap<K, V>
where
    K: Ord + Deserialize<'de>,
    V: Deserialize<'de>,
{
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct MapVisitor<K, V>(PhantomData<(K, V)>);

        impl<'de, K, V> Visitor<'de> for MapVisitor<K, V>
        where
            K: Ord + Deserialize<'de>,
            V: Deserialize<'de>,
        {
            type Value = UniqueMap<K, V>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a map")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
                let mut map = BTreeMap::new();
                while let Some(key) = access.next_key::<K>()? {
                    if map.contains_key(&key) {
                        return Err(serde::de::Error::custom(DUPLICATE_KEY_TAG));
                    }
                    let value = access.next_value::<V>()?;
                    map.insert(key, value);
                }
                Ok(UniqueMap(map))
            }
        }

        deserializer.deserialize_map(MapVisitor(PhantomData))
    }
}
