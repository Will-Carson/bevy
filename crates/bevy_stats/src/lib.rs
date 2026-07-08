#![doc = include_str!("../README.md")]
//!
//! # Architecture notes
//!
//! Stats are string-keyed and hierarchical by a dot convention (`Damage`,
//! `Damage.added`, `Damage.increased`). Each stat combines a replaceable
//! *base* value and any number of *modifiers* with a [`Reduction`]. Modifier
//! expressions reference other stats — on the same entity, or across
//! entities through named links — and those references form a dependency
//! graph. Writes recompute exactly the affected subgraph, once per stat, in
//! dependency order; reads are cache hits.
//!
//! Nothing game-specific is formalized in the type system: "equipment" is a
//! [`ModifierCollection`], "a buff" is an applied [`ModifierSet`], "an attack"
//! is an [`InstantEffect`]. Build your game's typed vocabulary on top of the
//! string-keyed core (see the `extension` example).

extern crate alloc;

pub mod access;
pub mod app_ext;
pub mod effect;
pub mod error;
pub mod expr;
pub mod modifier;
pub mod require;
pub mod stats;
pub mod sync;
pub mod tags;

mod lifecycle;

pub use access::{ModifierIdGen, StatChanged, StatGraph, StatsMutator, StatsReader};
pub use app_ext::StatsAppExt;
pub use effect::{InstantEffect, InstantOp, InstantOutcome, Roles};
pub use error::{ParseError, ParseErrorKind, StatError};
pub use expr::Expression;
pub use modifier::{
    AppliedModifiers, AttachedItems, AttachedTo, IntoModifierValue, Modifier, ModifierCollection,
    ModifierHandle, ModifierSet, ModifierValue,
};
pub use require::Requirement;
pub use stats::{Reduction, StatConfig, Stats};
pub use sync::{StatValue, StatsBound, StatsSyncSet};
pub use tags::{TagRegistry, TagSet};

#[cfg(feature = "derive")]
pub use bevy_stats_derive::StatSync;

use bevy_app::{App, Plugin, PostUpdate};
use bevy_ecs::schedule::IntoScheduleConfigs;

/// The commonly used types: `use bevy_stats::prelude::*;`.
pub mod prelude {
    pub use crate::app_ext::StatsAppExt;
    pub use crate::modifiers;
    #[cfg(feature = "derive")]
    pub use crate::StatSync;
    pub use crate::{
        AttachedTo, Expression, InstantEffect, Modifier, ModifierCollection, ModifierSet,
        Reduction, Requirement, Roles, StatChanged, StatError, StatValue, Stats, StatsBound,
        StatsMutator, StatsPlugin, StatsReader, StatsSyncSet, TagRegistry, TagSet,
    };
}

/// Adds the stat system to an [`App`]: resources, lifecycle observers, and
/// the component-sync schedule. Configure tags and reductions inline, or
/// later through [`StatsAppExt`]:
///
/// ```
/// # use bevy_app::App;
/// # use bevy_stats::{Reduction, StatsPlugin};
/// App::new().add_plugins(
///     StatsPlugin::new()
///         .with_tags(["fire", "cold", "lightning", "sword", "axe"])
///         .with_tag_group("elemental", ["fire", "cold", "lightning"])
///         .with_segment_reduction("more", Reduction::Product),
/// );
/// ```
#[derive(Default)]
pub struct StatsPlugin {
    #[expect(clippy::type_complexity, reason = "erased one-shot setup closures")]
    setup: Vec<Box<dyn Fn(&mut App) + Send + Sync>>,
}

impl StatsPlugin {
    /// A plugin with no tags or special reductions configured.
    pub fn new() -> StatsPlugin {
        StatsPlugin::default()
    }

    /// Registers leaf tag names.
    #[must_use]
    pub fn with_tags<const N: usize>(mut self, tags: [&'static str; N]) -> StatsPlugin {
        self.setup.push(Box::new(move |app| {
            app.register_stat_tags(tags);
        }));
        self
    }

    /// Registers a group tag standing for the union of its members.
    #[must_use]
    pub fn with_tag_group<const N: usize>(
        mut self,
        group: &'static str,
        members: [&'static str; N],
    ) -> StatsPlugin {
        self.setup.push(Box::new(move |app| {
            app.register_stat_tag_group(group, members);
        }));
        self
    }

    /// Sets the [`Reduction`] for an exact stat name.
    #[must_use]
    pub fn with_reduction(mut self, stat: &'static str, reduction: Reduction) -> StatsPlugin {
        self.setup.push(Box::new(move |app| {
            app.register_stat_reduction(stat, reduction.clone());
        }));
        self
    }

    /// Sets the [`Reduction`] for every stat whose last dot-segment matches.
    #[must_use]
    pub fn with_segment_reduction(
        mut self,
        segment: &'static str,
        reduction: Reduction,
    ) -> StatsPlugin {
        self.setup.push(Box::new(move |app| {
            app.register_segment_reduction(segment, reduction.clone());
        }));
        self
    }
}

impl Plugin for StatsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TagRegistry>()
            .init_resource::<StatConfig>()
            .init_resource::<StatGraph>()
            .init_resource::<access::DirtyStats>()
            .init_resource::<ModifierIdGen>();
        app.add_observer(lifecycle::on_stats_inserted)
            .add_observer(lifecycle::on_stats_discarded)
            .add_observer(lifecycle::on_attached)
            .add_observer(lifecycle::on_detached);
        app.configure_sets(
            PostUpdate,
            (StatsSyncSet::WriteToStats, StatsSyncSet::ReadFromStats).chain(),
        );
        app.add_systems(
            PostUpdate,
            sync::clear_dirty_stats.after(StatsSyncSet::ReadFromStats),
        );
        for setup in &self.setup {
            setup(app);
        }
    }
}
