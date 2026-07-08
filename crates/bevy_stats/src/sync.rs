//! Two-way synchronization between ordinary typed components and the stat
//! graph.
//!
//! Implement [`StatsBound`] (usually via `#[derive(StatSync)]`) and register
//! the type with
//! [`register_stats_component`](crate::app_ext::StatsAppExt::register_stats_component).
//! From then on:
//!
//! - **On insert**, the component's *write* fields seed the stat graph and
//!   its *read* fields are initialized from it (a missing [`Stats`]
//!   component is added automatically).
//! - **Component → stats** ([`StatsSyncSet::WriteToStats`]): when the
//!   component changes, its authoritative fields are written into stat base
//!   values.
//! - **Stats → component** ([`StatsSyncSet::ReadFromStats`], ordered after):
//!   when the entity's stats change, fields are refreshed from resolved
//!   values.
//!
//! Both directions are guarded by change detection *and* value comparison,
//! so the loop neither oscillates nor churns when nothing moved.

use crate::access::{DirtyStats, StatsMutator};
use crate::stats::Stats;
use bevy_ecs::change_detection::DetectChangesMut;
use bevy_ecs::component::{Component, Mutable};
use bevy_ecs::entity::Entity;
use bevy_ecs::lifecycle::Insert;
use bevy_ecs::observer::On;
use bevy_ecs::query::Changed;
use bevy_ecs::schedule::SystemSet;
use bevy_ecs::system::{Commands, Query, SystemState};
use bevy_ecs::world::World;

/// System sets for component synchronization, run in `PostUpdate`.
/// [`WriteToStats`](StatsSyncSet::WriteToStats) is ordered before
/// [`ReadFromStats`](StatsSyncSet::ReadFromStats), so same-frame component
/// writes win over stale stat values and immediately round-trip.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StatsSyncSet {
    /// Changed components write their authoritative fields into stats.
    WriteToStats,
    /// Components on entities with changed stats refresh their fields.
    ReadFromStats,
}

/// A component whose fields mirror stats. Derive it with
/// `#[derive(StatSync)]` (feature `derive`) or implement it by hand for
/// custom mappings:
///
/// ```
/// # use bevy_ecs::component::Component;
/// # use bevy_ecs::entity::Entity;
/// # use bevy_stats::{StatsBound, StatsMutator};
/// #[derive(Component)]
/// struct Stunned(bool);
///
/// impl StatsBound for Stunned {
///     fn write_stats(&self, _: Entity, _: &mut StatsMutator) {}
///     fn read_stats(&mut self, entity: Entity, stats: &mut StatsMutator) -> bool {
///         // Interpret a numeric stat as a bool.
///         let stunned = stats.get(entity, "StunTime") > 0.0;
///         let changed = self.0 != stunned;
///         self.0 = stunned;
///         changed
///     }
/// }
/// ```
pub trait StatsBound: Component<Mutability = Mutable> {
    /// Writes the component's authoritative values into the stat graph
    /// (typically via [`StatsMutator::set`], which no-ops on equal values).
    fn write_stats(&self, entity: Entity, stats: &mut StatsMutator);

    /// Reads resolved stat values into the component. Must return `true`
    /// only if a field actually changed, so change detection stays accurate.
    fn read_stats(&mut self, entity: Entity, stats: &mut StatsMutator) -> bool;
}

/// A field type usable in `#[derive(StatSync)]` components: convertible to
/// and from the stat graph's `f32` values.
pub trait StatValue: PartialEq + Clone + Send + Sync + 'static {
    /// Converts a resolved stat value into this type.
    fn from_stat(value: f32) -> Self;
    /// Converts this value into a stat value.
    fn to_stat(&self) -> f32;
}

impl StatValue for f32 {
    fn from_stat(value: f32) -> Self {
        value
    }
    fn to_stat(&self) -> f32 {
        *self
    }
}

impl StatValue for f64 {
    fn from_stat(value: f32) -> Self {
        value as f64
    }
    fn to_stat(&self) -> f32 {
        *self as f32
    }
}

impl StatValue for bool {
    fn from_stat(value: f32) -> Self {
        value != 0.0
    }
    fn to_stat(&self) -> f32 {
        if *self { 1.0 } else { 0.0 }
    }
}

macro_rules! int_stat_value {
    ($($ty:ty),*) => {$(
        impl StatValue for $ty {
            fn from_stat(value: f32) -> Self {
                value.round() as $ty
            }
            fn to_stat(&self) -> f32 {
                *self as f32
            }
        }
    )*};
}

int_stat_value!(i8, i16, i32, i64, u8, u16, u32, u64, usize, isize);

/// Component → stats. Runs in [`StatsSyncSet::WriteToStats`] for each
/// registered type.
pub(crate) fn write_to_stats_system<T: StatsBound>(
    targets: Query<(Entity, &T), Changed<T>>,
    mut stats: StatsMutator,
) {
    for (entity, component) in &targets {
        component.write_stats(entity, &mut stats);
    }
}

/// Stats → component. Runs in [`StatsSyncSet::ReadFromStats`] for each
/// registered type, visiting only entities whose resolved values actually
/// changed (the [`DirtyStats`] set). Uses `bypass_change_detection` and only
/// flags the component when a field really moved, so the two systems cannot
/// ping-pong.
pub(crate) fn read_from_stats_system<T: StatsBound>(
    mut targets: Query<&mut T>,
    mut stats: StatsMutator,
) {
    for entity in stats.dirty_entities() {
        let Ok(mut component) = targets.get_mut(entity) else {
            continue;
        };
        let changed = component
            .bypass_change_detection()
            .read_stats(entity, &mut stats);
        if changed {
            component.set_changed();
        }
    }
}

/// Clears the [`DirtyStats`] set once every registered sync system has seen
/// it; runs at the end of `PostUpdate`.
pub(crate) fn clear_dirty_stats(mut dirty: bevy_ecs::system::ResMut<DirtyStats>) {
    dirty.0.clear();
}

/// Initialize-on-spawn: seed stats from the component's authoritative
/// fields, then reflect resolved values back, adding a [`Stats`] component
/// if the entity has none.
pub(crate) fn on_bound_inserted<T: StatsBound>(event: On<Insert, T>, mut commands: Commands) {
    let entity = event.entity;
    commands.queue(move |world: &mut World| {
        if world.get_entity(entity).is_err() {
            return;
        }
        if world.get::<Stats>(entity).is_none() {
            world.entity_mut(entity).insert(Stats::new());
        }
        let mut state: SystemState<(StatsMutator, Query<&mut T>)> = SystemState::new(world);
        let Ok((mut stats, mut components)) = state.get_mut(world) else {
            return;
        };
        if let Ok(mut component) = components.get_mut(entity) {
            let inner = component.bypass_change_detection();
            inner.write_stats(entity, &mut stats);
            let changed = inner.read_stats(entity, &mut stats);
            if changed {
                component.set_changed();
            }
        }
        state.apply(world);
    });
}
