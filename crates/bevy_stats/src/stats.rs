//! The [`Stats`] container component, reductions, and per-stat configuration.

use crate::expr::CExpr;
use crate::modifier::{IntoModifierValue, Modifier, ModifierSet, ModifierValue};
use crate::tags::TagSet;
use alloc::sync::Arc;
use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::resource::Resource;
use bevy_platform::collections::HashMap;
use core::fmt;

/// How a stat combines its modifier values into one number.
#[derive(Clone, Default)]
pub enum Reduction {
    /// Adds the values. No modifiers ⇒ `0`. The default.
    #[default]
    Sum,
    /// Multiplies `(1 + v)` factors: each value `v` contributes a factor of
    /// `1 + v`, so `+0.5` means ×1.5 and `0.5` with `0.3` gives ×1.95.
    /// No modifiers ⇒ `1`.
    Product,
    /// A user-supplied reduction over the matching modifier values.
    ///
    /// The slice order follows insertion order (the replaceable base value,
    /// if set, comes first). An empty slice must still produce the identity
    /// you want for "no modifiers".
    Custom(Arc<dyn Fn(&[f32]) -> f32 + Send + Sync>),
}

impl Reduction {
    /// Builds a custom reduction from a closure.
    pub fn custom(f: impl Fn(&[f32]) -> f32 + Send + Sync + 'static) -> Reduction {
        Reduction::Custom(Arc::new(f))
    }

    /// Reduces a list of modifier values.
    pub fn reduce(&self, values: &[f32]) -> f32 {
        match self {
            // Not `Iterator::sum`: its `f32` identity is `-0.0`, which would
            // leak an ugly negative zero out of empty stats.
            Reduction::Sum => values.iter().fold(0.0, |acc, v| acc + v),
            Reduction::Product => values.iter().fold(1.0, |acc, v| acc * (1.0 + v)),
            Reduction::Custom(f) => f(values),
        }
    }
}

impl fmt::Debug for Reduction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Reduction::Sum => write!(f, "Sum"),
            Reduction::Product => write!(f, "Product"),
            Reduction::Custom(_) => write!(f, "Custom(..)"),
        }
    }
}

/// Per-stat configuration: which [`Reduction`] each stat name uses.
///
/// Lookup order: exact name, then the name's last dot-segment (so
/// `set_segment("more", Reduction::Product)` makes every `*.more` stat a
/// product), then the default ([`Reduction::Sum`]).
///
/// Configure through [`StatsPlugin`](crate::StatsPlugin) or
/// [`StatsAppExt`](crate::app_ext::StatsAppExt).
#[derive(Resource, Default, Debug)]
pub struct StatConfig {
    exact: HashMap<String, Reduction>,
    segment: HashMap<String, Reduction>,
    default: Reduction,
}

impl StatConfig {
    /// Sets the reduction for an exact stat name.
    pub fn set_exact(&mut self, stat: impl Into<String>, reduction: Reduction) {
        self.exact.insert(stat.into(), reduction);
    }

    /// Sets the reduction for every stat whose last dot-segment matches
    /// (e.g. `"more"` matches `Damage.more` and `Speed.more`).
    pub fn set_segment(&mut self, segment: impl Into<String>, reduction: Reduction) {
        self.segment.insert(segment.into(), reduction);
    }

    /// Sets the fallback reduction (initially [`Reduction::Sum`]).
    pub fn set_default(&mut self, reduction: Reduction) {
        self.default = reduction;
    }

    /// The reduction a stat of this name uses.
    pub fn reduction_for(&self, stat: &str) -> &Reduction {
        if let Some(r) = self.exact.get(stat) {
            return r;
        }
        let last = stat.rsplit('.').next().unwrap_or(stat);
        if let Some(r) = self.segment.get(last) {
            return r;
        }
        &self.default
    }
}

/// A pending entry queued on a [`Stats`] value before it is inserted onto an
/// entity; drained into the graph by the on-insert observer.
#[derive(Clone, Debug)]
pub(crate) enum PendingEntry {
    /// A replaceable base value (see [`StatsMutator::set`](crate::StatsMutator::set)).
    Base(String, f32),
    /// An ordinary modifier.
    Modifier(String, Modifier),
}

/// One modifier as stored in the graph: compiled, tagged, and addressable.
#[derive(Clone, Debug)]
pub(crate) struct StoredModifier {
    pub id: u64,
    pub tags: TagSet,
    pub value: StoredValue,
}

#[derive(Clone, Debug)]
pub(crate) enum StoredValue {
    Literal(f32),
    Expr(CExpr),
}

/// One named stat inside a [`Stats`] container.
#[derive(Debug, Default)]
pub(crate) struct StatNode {
    /// The replaceable, untagged "base" slot written by `set()` and instant
    /// effects. Participates in the reduction as the first value.
    pub base: Option<f32>,
    pub modifiers: Vec<StoredModifier>,
    /// Cached resolved values per query tag set. A new tag combination is
    /// computed and cached on first use.
    pub cache: HashMap<TagSet, f32>,
}

/// A dependent of a stat: `stat` on `entity` has a modifier expression that
/// reads the stat this edge points away from.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Dependent {
    /// The entity owning the dependent stat.
    pub entity: Entity,
    /// The name of the dependent stat.
    pub stat: String,
}

/// The stat container component.
///
/// Each stat is a named value produced by combining its modifiers with the
/// stat's [`Reduction`]. All mutation happens through
/// [`StatsMutator`](crate::StatsMutator), which keeps the dependency graph
/// consistent and recomputes exactly what a change affects.
///
/// # Seeding starting stats
///
/// `Stats::new().with(...)` queues entries that are installed when the
/// component is inserted (the [`StatsPlugin`](crate::StatsPlugin) must be
/// added):
///
/// ```
/// # use bevy_stats::Stats;
/// // Literal values become replaceable *base* values; expression strings
/// // become formula modifiers.
/// let stats = Stats::new()
///     .with("Vitality", 12.0)
///     .with("MaxHealth", "Vitality * 10 + 50");
/// ```
#[derive(Component, Default, Debug)]
pub struct Stats {
    pub(crate) pending: Vec<PendingEntry>,
    pub(crate) nodes: HashMap<String, StatNode>,
    /// Named, runtime-mutable cross-entity links used by `source@Stat`
    /// references in this entity's modifier expressions.
    pub(crate) links: HashMap<String, Entity>,
}

impl Stats {
    /// An empty container.
    pub fn new() -> Stats {
        Stats::default()
    }

    /// Queues a starting stat. Untagged literal values become the stat's
    /// replaceable *base* value; expression strings and tagged literals
    /// (`"Damage.added{fire}"`) become modifiers.
    ///
    /// # Panics
    /// Panics on syntax errors (this is a declarative seeding API; validate
    /// ahead of time with [`Expression::parse`](crate::Expression::parse) if
    /// the source is untrusted).
    #[must_use]
    pub fn with(mut self, path: &str, value: impl IntoModifierValue) -> Stats {
        let value = match value.into_modifier_value() {
            Ok(v) => v,
            Err(e) => panic!("invalid starting stat `{path}`: {e}"),
        };
        let (stat, tags) = match crate::expr::parse_stat_path(path) {
            Ok(parsed) => parsed,
            Err(e) => panic!("invalid starting stat `{path}`: {e}"),
        };
        match value {
            ModifierValue::Literal(v) if tags.is_empty() => {
                self.pending.push(PendingEntry::Base(stat, v));
            }
            value => {
                self.pending
                    .push(PendingEntry::Modifier(stat, Modifier { value, tags }));
            }
        }
        self
    }

    /// Queues an explicit starting modifier (never a base value).
    #[must_use]
    pub fn with_modifier(mut self, path: &str, modifier: Modifier) -> Stats {
        self.pending
            .push(PendingEntry::Modifier(path.to_string(), modifier));
        self
    }

    /// Queues every entry of a [`ModifierSet`] as starting modifiers.
    #[must_use]
    pub fn with_set(mut self, set: &ModifierSet) -> Stats {
        for (stat, modifier) in set.entries() {
            self.pending
                .push(PendingEntry::Modifier(stat.to_string(), modifier.clone()));
        }
        self
    }

    /// The last resolved value of an untagged query on `stat`, if one has
    /// been computed. Cheap cached read for read-only access; use
    /// [`StatsMutator::get`](crate::StatsMutator::get) or
    /// [`StatsReader`](crate::StatsReader) to compute values on demand.
    pub fn cached(&self, stat: &str) -> Option<f32> {
        self.nodes
            .get(stat)
            .and_then(|n| n.cache.get(&TagSet::NONE))
            .copied()
    }

    /// The entity a named cross-entity link currently points at.
    pub fn link(&self, name: &str) -> Option<Entity> {
        self.links.get(name).copied()
    }

    /// Iterates the names of all stats present in this container.
    pub fn stat_names(&self) -> impl Iterator<Item = &str> {
        self.nodes.keys().map(String::as_str)
    }

    /// Whether a stat node exists (has ever received a base, modifier, or
    /// cached read).
    pub fn has_stat(&self, stat: &str) -> bool {
        self.nodes.contains_key(stat)
    }

    /// The current replaceable base value of a stat, if set.
    pub fn base(&self, stat: &str) -> Option<f32> {
        self.nodes.get(stat).and_then(|n| n.base)
    }

    /// The number of modifiers currently on a stat (not counting the base).
    pub fn modifier_count(&self, stat: &str) -> usize {
        self.nodes.get(stat).map_or(0, |n| n.modifiers.len())
    }
}

impl From<ModifierSet> for Stats {
    fn from(set: ModifierSet) -> Self {
        Stats::new().with_set(&set)
    }
}
