//! Compact tag sets and the named tag registry.
//!
//! Tags let modifiers participate selectively in queries: a modifier applies
//! to a query if and only if the modifier's tags are a **subset** of the
//! query's tags. An untagged modifier is a subset of everything, so it applies
//! globally; a `fire | sword` modifier only applies to queries that ask for at
//! least both `fire` and `sword`.
//!
//! Insert modifiers with *broad* tags, query with *specific* tags.

use crate::error::StatError;
use bevy_ecs::resource::Resource;
use bevy_platform::collections::HashMap;
use core::fmt;
use core::ops::{BitOr, BitOrAssign};

/// A compact set of up to 64 tags, represented as a bitmask.
///
/// Obtain named tag sets from the [`TagRegistry`]. Combine them with `|`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct TagSet(u64);

impl TagSet {
    /// The empty tag set. As a modifier tag set it matches every query; as a
    /// query it only admits untagged modifiers.
    pub const NONE: TagSet = TagSet(0);

    /// Creates a tag set from a raw bitmask.
    pub const fn from_bits(bits: u64) -> Self {
        TagSet(bits)
    }

    /// The raw bitmask.
    pub const fn bits(self) -> u64 {
        self.0
    }

    /// Returns `true` if no tags are set.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Returns `true` if every tag in `other` is also in `self`.
    pub const fn contains(self, other: TagSet) -> bool {
        self.0 & other.0 == other.0
    }

    /// The subset rule: a modifier tagged `self` participates in a query
    /// tagged `query` iff `self ⊆ query`.
    ///
    /// - An untagged modifier applies to every query.
    /// - A `FIRE` modifier applies to a `FIRE | SWORD` query.
    /// - A `FIRE | SWORD` modifier does **not** apply to a `FIRE`-only query.
    pub const fn applies_to(self, query: TagSet) -> bool {
        query.contains(self)
    }

    /// The number of tags in the set.
    pub const fn len(self) -> u32 {
        self.0.count_ones()
    }
}

impl BitOr for TagSet {
    type Output = TagSet;
    fn bitor(self, rhs: Self) -> Self {
        TagSet(self.0 | rhs.0)
    }
}

impl BitOrAssign for TagSet {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl fmt::Debug for TagSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TagSet({:#b})", self.0)
    }
}

/// The single place tag names are declared, as a [`Resource`].
///
/// Register plain tags with [`register`](TagRegistry::register) and
/// hierarchical *group* tags — a name that stands for the union of its
/// members — with [`register_group`](TagRegistry::register_group). Both are
/// usually done through [`StatsPlugin`](crate::StatsPlugin) configuration or
/// [`StatsAppExt`](crate::app_ext::StatsAppExt).
///
/// Tag names registered here are usable in expression tag filters
/// (`"Damage.added{fire}"`), in modifier paths, and in tagged queries.
#[derive(Resource, Default, Debug)]
pub struct TagRegistry {
    entries: HashMap<String, TagSet>,
    next_bit: u32,
}

impl TagRegistry {
    /// Registers a leaf tag, allocating one of the 64 bits. Registering the
    /// same name twice returns the existing set. Fails with
    /// [`StatError::TagCapacity`] once 64 leaf tags exist.
    pub fn register(&mut self, name: impl Into<String>) -> Result<TagSet, StatError> {
        let name = name.into();
        if let Some(existing) = self.entries.get(&name) {
            return Ok(*existing);
        }
        if self.next_bit >= 64 {
            return Err(StatError::TagCapacity { tag: name });
        }
        let set = TagSet(1 << self.next_bit);
        self.next_bit += 1;
        self.entries.insert(name, set);
        Ok(set)
    }

    /// Registers a *group* tag: a name that stands for the union of its
    /// members. Members that are not yet registered are registered as leaf
    /// tags. Groups may include other groups.
    ///
    /// A query for a group covers every member (querying `elemental` covers
    /// `fire`, `cold` and `lightning`), because the group's mask is a superset
    /// of each member's mask.
    pub fn register_group<'a>(
        &mut self,
        name: impl Into<String>,
        members: impl IntoIterator<Item = &'a str>,
    ) -> Result<TagSet, StatError> {
        let mut set = TagSet::NONE;
        for member in members {
            set |= self.register(member)?;
        }
        self.entries.insert(name.into(), set);
        Ok(set)
    }

    /// Looks up a registered tag or group by name.
    pub fn get(&self, name: &str) -> Option<TagSet> {
        self.entries.get(name).copied()
    }

    /// Looks up a registered tag or group by name, failing with
    /// [`StatError::UnknownTag`] if it was never registered.
    pub fn resolve(&self, name: &str) -> Result<TagSet, StatError> {
        self.get(name)
            .ok_or_else(|| StatError::UnknownTag(name.to_string()))
    }

    /// Resolves a list of tag names into their union.
    pub fn resolve_all<'a>(
        &self,
        names: impl IntoIterator<Item = &'a str>,
    ) -> Result<TagSet, StatError> {
        let mut set = TagSet::NONE;
        for name in names {
            set |= self.resolve(name)?;
        }
        Ok(set)
    }

    /// Parses a tag list such as `"fire, sword"` or `"fire | sword"` into the
    /// union of the named tags. An empty string yields [`TagSet::NONE`].
    pub fn parse(&self, list: &str) -> Result<TagSet, StatError> {
        let mut set = TagSet::NONE;
        for name in list
            .split([',', '|'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            set |= self.resolve(name)?;
        }
        Ok(set)
    }
}
