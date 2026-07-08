//! Observers that keep the graph consistent across spawns, despawns, and
//! equipment attach/detach.

use crate::access::StatsMutator;
use crate::modifier::{AppliedCollection, AttachedTo, ModifierCollection};
use crate::stats::Stats;
use bevy_ecs::lifecycle::{Discard, Insert};
use bevy_ecs::observer::On;
use bevy_ecs::system::{Commands, Query};

/// Installs the pending starting stats queued by [`Stats::new().with(...)`](Stats::with)
/// when the component lands on an entity.
pub(crate) fn on_stats_inserted(event: On<Insert, Stats>, mut stats: StatsMutator) {
    stats.drain_pending(event.entity);
}

/// Cleans up every dependency edge and recomputes downstream when a `Stats`
/// component is removed, replaced, or its entity despawned.
pub(crate) fn on_stats_discarded(event: On<Discard, Stats>, mut stats: StatsMutator) {
    stats.teardown(event.entity);
}

/// Applies an item's [`ModifierCollection`] to the entity it was just
/// [`AttachedTo`].
pub(crate) fn on_attached(
    event: On<Insert, AttachedTo>,
    items: Query<(&AttachedTo, &ModifierCollection)>,
    mut stats: StatsMutator,
    mut commands: Commands,
) {
    let item = event.entity;
    let Ok((attached, collection)) = items.get(item) else {
        return; // no collection: the relationship is used for something else
    };
    match stats.apply(attached.0, &collection.0) {
        Ok(applied) => {
            commands.entity(item).insert(AppliedCollection(applied));
        }
        Err(e) => {
            log::error!("failed to attach modifier collection from {item}: {e}");
        }
    }
}

/// Detaches an item's modifiers when its [`AttachedTo`] relationship is
/// removed, replaced (re-pointed), or the item despawns.
pub(crate) fn on_detached(
    event: On<Discard, AttachedTo>,
    receipts: Query<&AppliedCollection>,
    mut stats: StatsMutator,
    mut commands: Commands,
) {
    let item = event.entity;
    let Ok(receipt) = receipts.get(item) else {
        return;
    };
    stats.remove_applied_ref(&receipt.0);
    commands.entity(item).try_remove::<AppliedCollection>();
}
