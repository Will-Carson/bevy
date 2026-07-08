//! Low-boilerplate [`App`] registration helpers.

use crate::stats::{Reduction, StatConfig};
use crate::sync::{
    StatsBound, StatsSyncSet, on_bound_inserted, read_from_stats_system, write_to_stats_system,
};
use crate::tags::TagRegistry;
use bevy_app::{App, PostUpdate};
use bevy_ecs::resource::Resource;
use bevy_ecs::schedule::IntoScheduleConfigs;
use bevy_platform::collections::HashSet;
use core::any::TypeId;

/// Guards against double-registering the same synced component type.
#[derive(Resource, Default)]
struct SyncRegistry(HashSet<TypeId>);

/// Extension methods on [`App`] for declaring tags, reductions, and synced
/// components. Everything here can also be configured up front on
/// [`StatsPlugin`](crate::StatsPlugin); use whichever reads better.
pub trait StatsAppExt {
    /// Registers a [`StatsBound`] component for two-way sync (see
    /// [`crate::sync`]). Idempotent per type.
    fn register_stats_component<T: StatsBound>(&mut self) -> &mut Self;

    /// Registers leaf tag names.
    fn register_stat_tags<'a>(&mut self, tags: impl IntoIterator<Item = &'a str>) -> &mut Self;

    /// Registers a group tag standing for the union of its members
    /// (members are auto-registered as leaves).
    fn register_stat_tag_group<'a>(
        &mut self,
        group: &str,
        members: impl IntoIterator<Item = &'a str>,
    ) -> &mut Self;

    /// Sets the [`Reduction`] for an exact stat name.
    fn register_stat_reduction(&mut self, stat: &str, reduction: Reduction) -> &mut Self;

    /// Sets the [`Reduction`] for every stat whose last dot-segment matches
    /// (e.g. `"more"` covers `Damage.more`, `Speed.more`, …).
    fn register_segment_reduction(&mut self, segment: &str, reduction: Reduction) -> &mut Self;
}

impl StatsAppExt for App {
    fn register_stats_component<T: StatsBound>(&mut self) -> &mut Self {
        let world = self.world_mut();
        world.init_resource::<SyncRegistry>();
        if !world
            .resource_mut::<SyncRegistry>()
            .0
            .insert(TypeId::of::<T>())
        {
            return self;
        }
        self.add_observer(on_bound_inserted::<T>);
        self.add_systems(
            PostUpdate,
            (
                write_to_stats_system::<T>.in_set(StatsSyncSet::WriteToStats),
                read_from_stats_system::<T>.in_set(StatsSyncSet::ReadFromStats),
            ),
        );
        self
    }

    fn register_stat_tags<'a>(&mut self, tags: impl IntoIterator<Item = &'a str>) -> &mut Self {
        self.init_resource::<TagRegistry>();
        let mut registry = self.world_mut().resource_mut::<TagRegistry>();
        for tag in tags {
            if let Err(e) = registry.register(tag) {
                log::error!("failed to register tag: {e}");
            }
        }
        self
    }

    fn register_stat_tag_group<'a>(
        &mut self,
        group: &str,
        members: impl IntoIterator<Item = &'a str>,
    ) -> &mut Self {
        self.init_resource::<TagRegistry>();
        let mut registry = self.world_mut().resource_mut::<TagRegistry>();
        if let Err(e) = registry.register_group(group, members) {
            log::error!("failed to register tag group: {e}");
        }
        self
    }

    fn register_stat_reduction(&mut self, stat: &str, reduction: Reduction) -> &mut Self {
        self.init_resource::<StatConfig>();
        self.world_mut()
            .resource_mut::<StatConfig>()
            .set_exact(stat, reduction);
        self
    }

    fn register_segment_reduction(&mut self, segment: &str, reduction: Reduction) -> &mut Self {
        self.init_resource::<StatConfig>();
        self.world_mut()
            .resource_mut::<StatConfig>()
            .set_segment(segment, reduction);
        self
    }
}
