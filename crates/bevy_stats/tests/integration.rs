//! Integration tests covering the correctness bars of the stat system.

use alloc::sync::Arc;
extern crate alloc;
use std::sync::atomic::{AtomicUsize, Ordering};

use bevy_app::{App, Update};
use bevy_ecs::entity::Entity;
use bevy_ecs::observer::On;
use bevy_ecs::prelude::{Query, Ref, ResMut, Resource};
use bevy_ecs::query::Changed;
use bevy_ecs::system::RunSystemOnce;
use bevy_ecs::world::World;
use bevy_stats::prelude::*;
use bevy_stats::{InstantOp, StatError};

fn app() -> App {
    let mut app = App::new();
    app.add_plugins(
        StatsPlugin::new()
            .with_tags(["fire", "cold", "lightning", "physical", "sword", "axe", "attack"])
            .with_tag_group("elemental", ["fire", "cold", "lightning"])
            .with_segment_reduction("increased", Reduction::Sum)
            .with_segment_reduction("more", Reduction::Product),
    );
    app
}

fn get(world: &mut World, entity: Entity, stat: &'static str) -> f32 {
    world
        .run_system_once(move |mut stats: StatsMutator| stats.get(entity, stat))
        .unwrap()
}

fn get_tagged(world: &mut World, entity: Entity, stat: &'static str, tags: &'static str) -> f32 {
    world
        .run_system_once(move |mut stats: StatsMutator| {
            stats.get_with_tags(entity, stat, tags).unwrap()
        })
        .unwrap()
}

fn set(world: &mut World, entity: Entity, stat: &'static str, value: f32) {
    world
        .run_system_once(move |mut stats: StatsMutator| stats.set(entity, stat, value).unwrap())
        .unwrap();
}

// ---------------------------------------------------------------------------
// Formulas & propagation
// ---------------------------------------------------------------------------

#[test]
fn formula_recomputes_on_source_change() {
    let mut app = app();
    let world = app.world_mut();
    let hero = world
        .spawn(
            Stats::new()
                .with("Vitality", 12.0)
                .with("MaxHealth", "Vitality * 10 + 50"),
        )
        .id();
    assert_eq!(get(world, hero, "MaxHealth"), 170.0);
    set(world, hero, "Vitality", 13.0);
    assert_eq!(get(world, hero, "MaxHealth"), 180.0);
    // The dependency is live, not a one-shot: change again.
    set(world, hero, "Vitality", 20.0);
    assert_eq!(get(world, hero, "MaxHealth"), 250.0);
}

#[test]
fn recompute_touches_exactly_dependents_once() {
    let mut app = app();
    let d_count = Arc::new(AtomicUsize::new(0));
    let u_count = Arc::new(AtomicUsize::new(0));
    let d_counter = d_count.clone();
    let u_counter = u_count.clone();
    app.register_stat_reduction(
        "D",
        Reduction::custom(move |values| {
            d_counter.fetch_add(1, Ordering::SeqCst);
            values.iter().sum()
        }),
    );
    app.register_stat_reduction(
        "Unrelated",
        Reduction::custom(move |values| {
            u_counter.fetch_add(1, Ordering::SeqCst);
            values.iter().sum()
        }),
    );
    let world = app.world_mut();
    // Diamond: A feeds B and C; D combines B and C. Plus an unrelated stat.
    let e = world
        .spawn(
            Stats::new()
                .with("A", 1.0)
                .with("B", "A * 2")
                .with("C", "A + 1")
                .with("D", "B + C")
                .with("Unrelated", 7.0),
        )
        .id();
    assert_eq!(get(world, e, "D"), 4.0);
    assert_eq!(get(world, e, "Unrelated"), 7.0);
    let d_before = d_count.load(Ordering::SeqCst);
    let u_before = u_count.load(Ordering::SeqCst);

    set(world, e, "A", 2.0);

    // D was re-reduced exactly once for the change, despite two paths A->D.
    assert_eq!(d_count.load(Ordering::SeqCst), d_before + 1);
    // The unrelated stat was not re-evaluated at all.
    assert_eq!(u_count.load(Ordering::SeqCst), u_before);
    assert_eq!(get(world, e, "D"), 7.0);
    // Reading D again hits the cache: no further reduction calls.
    assert_eq!(d_count.load(Ordering::SeqCst), d_before + 1);
}

#[test]
fn no_op_write_causes_no_recompute() {
    let mut app = app();
    let count = Arc::new(AtomicUsize::new(0));
    let counter = count.clone();
    app.register_stat_reduction(
        "Derived",
        Reduction::custom(move |values| {
            counter.fetch_add(1, Ordering::SeqCst);
            values.iter().sum()
        }),
    );
    let world = app.world_mut();
    let e = world
        .spawn(Stats::new().with("Base", 5.0).with("Derived", "Base * 2"))
        .id();
    assert_eq!(get(world, e, "Derived"), 10.0);
    let before = count.load(Ordering::SeqCst);
    set(world, e, "Base", 5.0); // same value
    assert_eq!(count.load(Ordering::SeqCst), before);
}

// ---------------------------------------------------------------------------
// Reductions
// ---------------------------------------------------------------------------

#[test]
fn reduction_identities_and_product_rule() {
    let mut app = app();
    let world = app.world_mut();
    let e = world.spawn(Stats::new()).id();
    // Empty sum is 0; empty product is 1 (segment `more` is Product).
    assert_eq!(get(world, e, "Damage.added"), 0.0);
    assert_eq!(get(world, e, "Damage.more"), 1.0);

    world
        .run_system_once(move |mut stats: StatsMutator| {
            stats.add_modifier(e, "Damage.more", 0.5).unwrap();
            stats.add_modifier(e, "Damage.more", 0.3).unwrap();
        })
        .unwrap();
    // (1 + 0.5) * (1 + 0.3) = 1.95
    let v = get(world, e, "Damage.more");
    assert!((v - 1.95).abs() < 1e-6, "expected 1.95, got {v}");
}

#[test]
fn custom_reduction() {
    let mut app = app();
    app.register_stat_reduction(
        "Highest",
        Reduction::custom(|values| values.iter().copied().fold(0.0, f32::max)),
    );
    let world = app.world_mut();
    let e = world.spawn(Stats::new()).id();
    world
        .run_system_once(move |mut stats: StatsMutator| {
            stats.add_modifier(e, "Highest", 3.0).unwrap();
            stats.add_modifier(e, "Highest", 9.0).unwrap();
            stats.add_modifier(e, "Highest", 5.0).unwrap();
        })
        .unwrap();
    assert_eq!(get(world, e, "Highest"), 9.0);
}

#[test]
fn base_replacement_leaves_other_contributions_intact() {
    let mut app = app();
    let world = app.world_mut();
    let e = world
        .spawn(Stats::new().with("Might", 4.0).with("Power", "Might * 3"))
        .id();
    world
        .run_system_once(move |mut stats: StatsMutator| {
            stats.set(e, "Power", 100.0).unwrap(); // changing base
            stats.add_modifier(e, "Power", 10.0).unwrap(); // flat modifier
        })
        .unwrap();
    // base 100 + formula 12 + flat 10
    assert_eq!(get(world, e, "Power"), 122.0);
    set(world, e, "Power", 80.0); // replace ONLY the base
    assert_eq!(get(world, e, "Power"), 102.0);
    set(world, e, "Might", 10.0); // formula contribution still live
    assert_eq!(get(world, e, "Power"), 120.0);
}

// ---------------------------------------------------------------------------
// Tags
// ---------------------------------------------------------------------------

#[test]
fn tag_subset_rule_is_exact() {
    let mut app = app();
    let world = app.world_mut();
    let e = world.spawn(Stats::new()).id();
    world
        .run_system_once(move |mut stats: StatsMutator| {
            stats.add_modifier(e, "Damage", 1.0).unwrap(); // untagged
            stats.add_modifier(e, "Damage{fire}", 10.0).unwrap();
            stats.add_modifier(e, "Damage{fire, sword}", 100.0).unwrap();
        })
        .unwrap();
    // Untagged applies to every query.
    assert_eq!(get(world, e, "Damage"), 1.0);
    // FIRE modifier applies to a FIRE query; FIRE|SWORD does not.
    assert_eq!(get_tagged(world, e, "Damage", "fire"), 11.0);
    // FIRE and FIRE|SWORD both apply to a FIRE|SWORD query.
    assert_eq!(get_tagged(world, e, "Damage", "fire, sword"), 111.0);
    // SWORD-only sees just the untagged modifier.
    assert_eq!(get_tagged(world, e, "Damage", "sword"), 1.0);
}

#[test]
fn group_tags_cover_members() {
    let mut app = app();
    let world = app.world_mut();
    let e = world.spawn(Stats::new()).id();
    world
        .run_system_once(move |mut stats: StatsMutator| {
            stats.add_modifier(e, "Damage{fire}", 5.0).unwrap();
            stats.add_modifier(e, "Damage{cold}", 7.0).unwrap();
            stats.add_modifier(e, "Damage{physical}", 100.0).unwrap();
        })
        .unwrap();
    // Querying the group covers every member, but not outsiders.
    assert_eq!(get_tagged(world, e, "Damage", "elemental"), 12.0);
}

#[test]
fn tagged_queries_recompute_like_untagged_ones() {
    let mut app = app();
    let world = app.world_mut();
    let e = world
        .spawn(Stats::new().with("Level", 1.0))
        .id();
    world
        .run_system_once(move |mut stats: StatsMutator| {
            stats.add_modifier(e, "Damage{fire}", "Level * 3").unwrap();
        })
        .unwrap();
    assert_eq!(get_tagged(world, e, "Damage", "fire"), 3.0);
    set(world, e, "Level", 4.0);
    // The cached fire-query value was refreshed by the propagation.
    assert_eq!(get_tagged(world, e, "Damage", "fire"), 12.0);
}

#[test]
fn expression_tag_filters_by_name() {
    let mut app = app();
    let world = app.world_mut();
    let e = world.spawn(Stats::new()).id();
    world
        .run_system_once(move |mut stats: StatsMutator| {
            stats.add_modifier(e, "Damage.added{fire}", 12.0).unwrap();
            stats.add_modifier(e, "Damage.added{cold}", 5.0).unwrap();
            stats
                .add_modifier(e, "FireFraction", "Damage.added{fire} / Damage.added{elemental}")
                .unwrap();
        })
        .unwrap();
    let v = get(world, e, "FireFraction");
    assert!((v - 12.0 / 17.0).abs() < 1e-6);
}

#[test]
fn unknown_tag_is_a_typed_error() {
    let mut app = app();
    let world = app.world_mut();
    let e = world.spawn(Stats::new()).id();
    let err = world
        .run_system_once(move |mut stats: StatsMutator| {
            stats.add_modifier(e, "Damage{chaos}", 1.0).unwrap_err()
        })
        .unwrap();
    assert_eq!(err, StatError::UnknownTag("chaos".to_string()));
}

// ---------------------------------------------------------------------------
// Compound stats
// ---------------------------------------------------------------------------

#[test]
fn compound_stat_resolves_new_tag_combinations_on_first_use() {
    let mut app = app();
    let world = app.world_mut();
    let e = world.spawn(Stats::new()).id();
    world
        .run_system_once(move |mut stats: StatsMutator| {
            // damage = added * (1 + increased) * more; parts inherit the
            // query's tags because the references carry no explicit filter.
            stats
                .add_modifier(
                    e,
                    "Damage",
                    "Damage.added * (1 + Damage.increased) * Damage.more",
                )
                .unwrap();
            stats.add_modifier(e, "Damage.added", 10.0).unwrap();
            stats.add_modifier(e, "Damage.added{fire}", 5.0).unwrap();
            stats.add_modifier(e, "Damage.increased{fire}", 0.2).unwrap();
            stats.add_modifier(e, "Damage.more{sword}", 0.5).unwrap();
        })
        .unwrap();
    // Untagged: 10 * 1.0 * 1.0
    assert_eq!(get(world, e, "Damage"), 10.0);
    // fire: (10+5) * 1.2 * 1
    assert!((get_tagged(world, e, "Damage", "fire") - 18.0).abs() < 1e-4);
    // A combination never queried before resolves on first use:
    // fire|sword: (10+5) * 1.2 * 1.5
    assert!((get_tagged(world, e, "Damage", "fire, sword") - 27.0).abs() < 1e-4);
}

// ---------------------------------------------------------------------------
// Modifier bundles (equipment / enchant flows)
// ---------------------------------------------------------------------------

#[test]
fn modifier_sets_apply_and_detach_symmetrically() {
    let mut app = app();
    let world = app.world_mut();
    let hero = world.spawn(Stats::new().with("Damage.added", 3.0)).id();

    let sword = modifiers! {
        "Damage.added" => 12.0,
        "Damage.increased" => 0.15,
    };
    let enchant = modifiers! {
        "Damage.added{fire}" => 6.0,
    };

    let sword_receipt = world
        .run_system_once(move |mut stats: StatsMutator| stats.apply(hero, &sword).unwrap())
        .unwrap();
    let enchant_receipt = world
        .run_system_once(move |mut stats: StatsMutator| stats.apply(hero, &enchant).unwrap())
        .unwrap();

    assert_eq!(get(world, hero, "Damage.added"), 15.0);
    assert_eq!(get_tagged(world, hero, "Damage.added", "fire"), 21.0);

    // Remove the enchant: the sword and the character's own base remain.
    world
        .run_system_once(move |mut stats: StatsMutator| stats.remove(&enchant_receipt))
        .unwrap();
    assert_eq!(get_tagged(world, hero, "Damage.added", "fire"), 15.0);
    assert_eq!(get(world, hero, "Damage.added"), 15.0);

    // Remove the sword: back to the bare character.
    world
        .run_system_once(move |mut stats: StatsMutator| stats.remove(&sword_receipt))
        .unwrap();
    assert_eq!(get(world, hero, "Damage.added"), 3.0);
    assert_eq!(get(world, hero, "Damage.increased"), 0.0);
}

#[test]
fn attached_collection_follows_the_relationship() {
    let mut app = app();
    let world = app.world_mut();
    let alice = world.spawn(Stats::new().with("Armor", 5.0)).id();
    let bob = world.spawn(Stats::new().with("Armor", 5.0)).id();
    let cuirass = world
        .spawn((
            ModifierCollection(modifiers! { "Armor" => 20.0 }),
            AttachedTo(alice),
        ))
        .id();
    world.flush();
    assert_eq!(get(world, alice, "Armor"), 25.0);
    assert_eq!(get(world, bob, "Armor"), 5.0);

    // Hand it to Bob: one operation moves the whole bundle.
    world.entity_mut(cuirass).insert(AttachedTo(bob));
    world.flush();
    assert_eq!(get(world, alice, "Armor"), 5.0);
    assert_eq!(get(world, bob, "Armor"), 25.0);

    // Destroying the item detaches it.
    world.despawn(cuirass);
    world.flush();
    assert_eq!(get(world, bob, "Armor"), 5.0);
}

#[test]
fn removing_one_of_two_identical_modifiers_keeps_the_other() {
    let mut app = app();
    let world = app.world_mut();
    let e = world.spawn(Stats::new().with("Source", 2.0)).id();
    let (h1, _h2) = world
        .run_system_once(move |mut stats: StatsMutator| {
            let h1 = stats.add_modifier(e, "Derived", "Source * 10").unwrap();
            let h2 = stats.add_modifier(e, "Derived", "Source * 10").unwrap();
            (h1, h2)
        })
        .unwrap();
    assert_eq!(get(world, e, "Derived"), 40.0);
    world
        .run_system_once(move |mut stats: StatsMutator| {
            stats.remove_modifier(e, "Derived", h1).unwrap();
        })
        .unwrap();
    assert_eq!(get(world, e, "Derived"), 20.0);
    // The shared dependency edge survived (refcounted): changes still flow.
    set(world, e, "Source", 3.0);
    assert_eq!(get(world, e, "Derived"), 30.0);
}

// ---------------------------------------------------------------------------
// Cross-entity links
// ---------------------------------------------------------------------------

#[test]
fn link_repoint_rewires_and_recomputes() {
    let mut app = app();
    let world = app.world_mut();
    let hero = world.spawn(Stats::new().with("Strength", 10.0)).id();
    let ogre = world.spawn(Stats::new().with("Strength", 50.0)).id();
    let sword = world
        .spawn(Stats::new().with("Damage", "wielder@Strength * 2"))
        .id();
    world
        .run_system_once(move |mut stats: StatsMutator| {
            stats.set_link(sword, "wielder", hero).unwrap();
        })
        .unwrap();
    assert_eq!(get(world, sword, "Damage"), 20.0);
    // Source change flows across the entity boundary.
    set(world, hero, "Strength", 12.0);
    assert_eq!(get(world, sword, "Damage"), 24.0);

    // Re-point the link: a single operation rewires and recomputes.
    world
        .run_system_once(move |mut stats: StatsMutator| {
            stats.set_link(sword, "wielder", ogre).unwrap();
        })
        .unwrap();
    assert_eq!(get(world, sword, "Damage"), 100.0);
    // The old source no longer propagates here...
    set(world, hero, "Strength", 999.0);
    assert_eq!(get(world, sword, "Damage"), 100.0);
    // ...and the old edge is gone from the old source.
    let count = world
        .run_system_once(move |stats: StatsMutator| stats.dependent_count(hero, "Strength"))
        .unwrap();
    assert_eq!(count, 0);
    // The new source does propagate.
    set(world, ogre, "Strength", 60.0);
    assert_eq!(get(world, sword, "Damage"), 120.0);
}

#[test]
fn multi_hop_chain_recomputes_across_entities() {
    let mut app = app();
    let world = app.world_mut();
    // aura -> hero -> weapon: three hops, two links.
    let aura = world.spawn(Stats::new().with("Bonus", 5.0)).id();
    let hero = world
        .spawn(Stats::new().with("Strength.base", 10.0).with(
            "Strength",
            "Strength.base + aura@Bonus",
        ))
        .id();
    let weapon = world
        .spawn(Stats::new().with("Damage", "wielder@Strength * 2"))
        .id();
    world
        .run_system_once(move |mut stats: StatsMutator| {
            stats.set_link(hero, "aura", aura).unwrap();
            stats.set_link(weapon, "wielder", hero).unwrap();
        })
        .unwrap();
    assert_eq!(get(world, weapon, "Damage"), 30.0);
    // A change at the far end of the chain reaches the weapon.
    set(world, aura, "Bonus", 15.0);
    assert_eq!(get(world, weapon, "Damage"), 50.0);
}

#[test]
fn clearing_a_link_reads_as_missing_stats() {
    let mut app = app();
    let world = app.world_mut();
    let hero = world.spawn(Stats::new().with("Strength", 10.0)).id();
    let sword = world
        .spawn(Stats::new().with("Damage", "wielder@Strength * 2 + 1"))
        .id();
    world
        .run_system_once(move |mut stats: StatsMutator| {
            stats.set_link(sword, "wielder", hero).unwrap();
        })
        .unwrap();
    assert_eq!(get(world, sword, "Damage"), 21.0);
    world
        .run_system_once(move |mut stats: StatsMutator| {
            stats.clear_link(sword, "wielder").unwrap();
        })
        .unwrap();
    assert_eq!(get(world, sword, "Damage"), 1.0);
    // Changing the ex-source does not resurrect the connection.
    set(world, hero, "Strength", 100.0);
    assert_eq!(get(world, sword, "Damage"), 1.0);
}

// ---------------------------------------------------------------------------
// Despawn cleanup
// ---------------------------------------------------------------------------

#[test]
fn despawn_cleans_up_edges_and_recomputes_dependents() {
    let mut app = app();
    let world = app.world_mut();
    let hero = world.spawn(Stats::new().with("Strength", 10.0)).id();
    let weapon = world
        .spawn(Stats::new().with("Damage", "wielder@Strength * 2 + 3"))
        .id();
    world
        .run_system_once(move |mut stats: StatsMutator| {
            stats.set_link(weapon, "wielder", hero).unwrap();
        })
        .unwrap();
    assert_eq!(get(world, weapon, "Damage"), 23.0);

    world.despawn(hero);
    world.flush();
    // The weapon's cached value was recomputed with the source gone.
    assert_eq!(get(world, weapon, "Damage"), 3.0);
}

#[test]
fn despawning_a_dependent_releases_edges_on_its_sources() {
    let mut app = app();
    let world = app.world_mut();
    let hero = world.spawn(Stats::new().with("Strength", 10.0)).id();
    let weapon = world
        .spawn(Stats::new().with("Damage", "wielder@Strength * 2"))
        .id();
    world
        .run_system_once(move |mut stats: StatsMutator| {
            stats.set_link(weapon, "wielder", hero).unwrap();
        })
        .unwrap();
    assert_eq!(get(world, weapon, "Damage"), 20.0);
    let count = world
        .run_system_once(move |stats: StatsMutator| stats.dependent_count(hero, "Strength"))
        .unwrap();
    assert_eq!(count, 1);

    world.despawn(weapon);
    world.flush();
    // The graph no longer tracks the dead dependent.
    let count = world
        .run_system_once(move |stats: StatsMutator| stats.dependent_count(hero, "Strength"))
        .unwrap();
    assert_eq!(count, 0);
    // And writes to the hero proceed without incident.
    set(world, hero, "Strength", 11.0);
}

// ---------------------------------------------------------------------------
// Expression evaluation edge cases
// ---------------------------------------------------------------------------

#[test]
fn expression_edge_cases() {
    let mut app = app();
    let world = app.world_mut();
    let e = world.spawn(Stats::new().with("Zero", 0.0).with("Ten", 10.0)).id();
    let eval = |world: &mut World, src: &'static str| -> f32 {
        world
            .run_system_once(move |stats: StatsMutator| {
                stats.eval(e, &Expression::parse(src).unwrap()).unwrap()
            })
            .unwrap()
    };
    // Division by (near-)zero yields 0, never NaN/inf.
    assert_eq!(eval(world, "Ten / Zero"), 0.0);
    assert_eq!(eval(world, "Ten % Zero"), 0.0);
    assert_eq!(eval(world, "1 / 0.0000000001"), 0.0);
    // Power producing a non-finite result yields 0.
    assert_eq!(eval(world, "10 ^ 400"), 0.0);
    assert_eq!(eval(world, "(0 - 1) ^ 0.5"), 0.0);
    assert_eq!(eval(world, "2 ^ 10"), 1024.0);
    // Comparison/logical operators yield 1/0.
    assert_eq!(eval(world, "Ten > 5"), 1.0);
    assert_eq!(eval(world, "Ten < 5"), 0.0);
    assert_eq!(eval(world, "Ten >= 10 && Zero == 0"), 1.0);
    assert_eq!(eval(world, "Ten < 5 || Zero != 0"), 0.0);
    assert_eq!(eval(world, "!Zero"), 1.0);
    // Equality compares within a small epsilon.
    assert_eq!(eval(world, "0.1 + 0.2 == 0.3"), 1.0);
    assert_eq!(eval(world, "0.1 + 0.2 != 0.3"), 0.0);
    // Math helpers.
    assert_eq!(eval(world, "min(Ten, 3, 7)"), 3.0);
    assert_eq!(eval(world, "max(Ten, 30)"), 30.0);
    assert_eq!(eval(world, "abs(0 - Ten)"), 10.0);
    assert_eq!(eval(world, "clamp(Ten, 0, 7.5)"), 7.5);
    // Missing stats read as their reduction identity.
    assert_eq!(eval(world, "NeverDefined + 4"), 4.0);
}

#[test]
fn parse_errors_are_typed() {
    assert!(matches!(
        Expression::parse("Strength +"),
        Err(StatError::Parse(_))
    ));
    assert!(matches!(
        Expression::parse("frobnicate(1)"),
        Err(StatError::Parse(bevy_stats::ParseError {
            kind: bevy_stats::ParseErrorKind::UnknownFunction(_),
            ..
        }))
    ));
    assert!(matches!(
        Expression::parse("clamp(1, 2)"),
        Err(StatError::Parse(bevy_stats::ParseError {
            kind: bevy_stats::ParseErrorKind::WrongArity { .. },
            ..
        }))
    ));
    assert!(matches!(
        Expression::parse("1 ? 2"),
        Err(StatError::Parse(_))
    ));
}

#[test]
fn dependency_cycles_do_not_hang_or_crash() {
    let mut app = app();
    let world = app.world_mut();
    let e = world
        .spawn(Stats::new().with("Chicken", "Egg + 1").with("Egg", "Chicken + 1"))
        .id();
    // The cycle is broken deterministically (in-progress frame reads 0).
    let v = get(world, e, "Chicken");
    assert!(v.is_finite());
    set(world, e, "Chicken", 5.0);
    assert!(get(world, e, "Egg").is_finite());
}

// ---------------------------------------------------------------------------
// Requirements
// ---------------------------------------------------------------------------

#[test]
fn requirements_gate_on_current_stats() {
    let mut app = app();
    let world = app.world_mut();
    let e = world
        .spawn(Stats::new().with("Intelligence", 14.0).with("Strength", 30.0))
        .id();
    let req = Requirement::parse("Intelligence >= 15 && Strength >= 20").unwrap();
    let check = move |world: &mut World, req: Requirement| -> bool {
        world
            .run_system_once(move |stats: StatsMutator| stats.check(e, &req).unwrap())
            .unwrap()
    };
    assert!(!check(world, req.clone()));
    set(world, e, "Intelligence", 15.0);
    assert!(check(world, req));
}

// ---------------------------------------------------------------------------
// Instant effects
// ---------------------------------------------------------------------------

#[test]
fn instant_effects_use_roles_and_leave_no_modifiers() {
    let mut app = app();
    let world = app.world_mut();
    let attacker = world.spawn(Stats::new().with("Strength", 20.0)).id();
    let weapon = world.spawn(Stats::new().with("Damage", 15.0)).id();
    let target = world
        .spawn(Stats::new().with("Life", 100.0).with("Armor", 5.0))
        .id();

    let strike = InstantEffect::new().sub(
        "target",
        "Life",
        "attacker@Strength / 2 + weapon@Damage - target@Armor",
    );
    let roles = Roles::new()
        .with("attacker", attacker)
        .with("weapon", weapon)
        .with("target", target);

    // Preview computes without committing.
    let (preview, applied) = world
        .run_system_once(move |mut stats: StatsMutator| {
            let preview = stats.preview_effect(&strike, &roles).unwrap();
            let before = stats.get(target, "Life");
            assert_eq!(before, 100.0, "preview must not commit");
            let applied = stats.apply_effect(&strike, &roles).unwrap();
            (preview, applied)
        })
        .unwrap();
    assert_eq!(preview, applied);
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0].op, InstantOp::Sub);
    assert_eq!(applied[0].amount, 20.0);
    assert_eq!(applied[0].new_base, 80.0);
    assert_eq!(get(world, target, "Life"), 80.0);
    // One-shot: no lingering modifier, just the base moved.
    assert_eq!(world.get::<Stats>(target).unwrap().modifier_count("Life"), 0);
    assert_eq!(world.get::<Stats>(target).unwrap().base("Life"), Some(80.0));
}

#[test]
fn instant_effect_ops_share_a_pre_effect_snapshot() {
    let mut app = app();
    let world = app.world_mut();
    let a = world.spawn(Stats::new().with("Life", 50.0)).id();
    let b = world.spawn(Stats::new().with("Life", 70.0)).id();
    // Swap-flavored effect: both sides read pre-effect values.
    let effect = InstantEffect::new()
        .set("a", "Life", "b@Life")
        .set("b", "Life", "a@Life");
    let roles = Roles::new().with("a", a).with("b", b);
    world
        .run_system_once(move |mut stats: StatsMutator| {
            stats.apply_effect(&effect, &roles).unwrap();
        })
        .unwrap();
    assert_eq!(get(world, a, "Life"), 70.0);
    assert_eq!(get(world, b, "Life"), 50.0);
}

#[test]
fn missing_role_is_a_typed_error() {
    let mut app = app();
    let world = app.world_mut();
    let effect = InstantEffect::new().add("target", "Life", 5.0);
    let err = world
        .run_system_once(move |mut stats: StatsMutator| {
            stats.apply_effect(&effect, &Roles::new()).unwrap_err()
        })
        .unwrap();
    assert_eq!(err, StatError::UnknownRole("target".to_string()));
}

// ---------------------------------------------------------------------------
// StatChanged events
// ---------------------------------------------------------------------------

#[derive(Resource, Default)]
struct ChangeLog(Vec<(Entity, String)>);

#[test]
fn stat_changed_events_fire_only_on_real_changes() {
    let mut app = app();
    app.init_resource::<ChangeLog>();
    app.add_observer(|event: On<StatChanged>, mut log: ResMut<ChangeLog>| {
        log.0.push((event.entity, event.stat.clone()));
    });
    let world = app.world_mut();
    let e = world
        .spawn(Stats::new().with("Vitality", 12.0).with("MaxHealth", "Vitality * 10"))
        .id();
    get(world, e, "MaxHealth"); // warm the cache so changes are observable
    world.resource_mut::<ChangeLog>().0.clear();

    set(world, e, "Vitality", 13.0);
    world.flush();
    let log = std::mem::take(&mut world.resource_mut::<ChangeLog>().0);
    assert!(log.contains(&(e, "MaxHealth".to_string())));

    // A no-op write fires nothing.
    set(world, e, "Vitality", 13.0);
    world.flush();
    assert!(world.resource::<ChangeLog>().0.is_empty());
}

// ---------------------------------------------------------------------------
// Component sync
// ---------------------------------------------------------------------------

#[derive(bevy_ecs::prelude::Component, StatSync, Debug, PartialEq)]
struct Health {
    #[stat("Life.max")]
    max: f32,
    #[stat("Life.current", write)]
    current: f32,
}

#[derive(Resource, Default)]
struct SyncChurn(usize);

fn count_health_changes(query: Query<Ref<Health>, Changed<Health>>, mut churn: ResMut<SyncChurn>) {
    churn.0 += query.iter().count();
}

#[test]
fn component_sync_two_way_with_change_detection() {
    let mut app = app();
    app.init_resource::<SyncChurn>();
    app.register_stats_component::<Health>();
    app.add_systems(Update, count_health_changes);

    let world = app.world_mut();
    let hero = world
        .spawn((
            Stats::new().with("Life.max", "Vitality * 10").with("Vitality", 10.0),
            Health {
                max: 0.0,
                current: 40.0,
            },
        ))
        .id();
    world.flush();
    app.update();

    // Initialize-on-spawn: `write` fields seeded the graph, `read` fields
    // pulled resolved values.
    {
        let world = app.world_mut();
        assert_eq!(get(world, hero, "Life.current"), 40.0);
        let health = world.get::<Health>(hero).unwrap();
        assert_eq!(health.max, 100.0);
    }

    // Stats -> component.
    {
        let world = app.world_mut();
        set(world, hero, "Vitality", 15.0);
    }
    app.update();
    assert_eq!(app.world().get::<Health>(hero).unwrap().max, 150.0);

    // Component -> stats.
    app.world_mut().get_mut::<Health>(hero).unwrap().current = 25.0;
    app.update();
    assert_eq!(get(app.world_mut(), hero, "Life.current"), 25.0);

    // No churn once stable: further updates change nothing.
    app.world_mut().resource_mut::<SyncChurn>().0 = 0;
    app.update();
    app.update();
    assert_eq!(app.world().resource::<SyncChurn>().0, 0);
}

// Manual StatsBound impl: interpret a numeric stat as a bool.
#[derive(bevy_ecs::prelude::Component, Default)]
struct Burning(bool);

impl StatsBound for Burning {
    fn write_stats(&self, _: Entity, _: &mut StatsMutator) {}
    fn read_stats(&mut self, entity: Entity, stats: &mut StatsMutator) -> bool {
        let burning = stats.get(entity, "BurnStacks") > 0.0;
        let changed = self.0 != burning;
        self.0 = burning;
        changed
    }
}

#[test]
fn manual_sync_impl_maps_custom_types() {
    let mut app = app();
    app.register_stats_component::<Burning>();
    let world = app.world_mut();
    let e = world.spawn((Stats::new().with("BurnStacks", 0.0), Burning(false))).id();
    world.flush();
    app.update();
    assert!(!app.world().get::<Burning>(e).unwrap().0);

    set(app.world_mut(), e, "BurnStacks", 3.0);
    app.update();
    assert!(app.world().get::<Burning>(e).unwrap().0);
}

// ---------------------------------------------------------------------------
// Misc container behavior
// ---------------------------------------------------------------------------

#[test]
fn removing_stats_component_then_reinserting_starts_clean() {
    let mut app = app();
    let world = app.world_mut();
    let hero = world.spawn(Stats::new().with("Strength", 10.0)).id();
    let sword = world
        .spawn(Stats::new().with("Damage", "wielder@Strength * 2"))
        .id();
    world
        .run_system_once(move |mut stats: StatsMutator| {
            stats.set_link(sword, "wielder", hero).unwrap();
        })
        .unwrap();
    assert_eq!(get(world, sword, "Damage"), 20.0);

    // Replacing the component discards the old graph node cleanly.
    world.entity_mut(hero).insert(Stats::new().with("Strength", 40.0));
    world.flush();
    assert_eq!(get(world, hero, "Strength"), 40.0);
    // The weapon lost its edge when the old component was torn down; it
    // reads the new component's value on the next resolve.
    assert_eq!(get(world, sword, "Damage"), 80.0);
}
