//! An equipment / enchant flow: equip a sword, enchant it, remove the
//! enchant, hand the sword to someone else — the numbers track exactly what
//! came from where, and detaching is perfectly symmetric.
#![expect(clippy::print_stdout, reason = "example output")]

use bevy_app::App;
use bevy_ecs::system::RunSystemOnce;
use bevy_stats::prelude::*;

fn main() {
    let mut app = App::new();
    app.add_plugins(StatsPlugin::new().with_tags(["physical", "fire", "sword"]));
    let world = app.world_mut();

    // A bare-fisted hero with a little innate damage.
    let hero = world
        .spawn(Stats::new().with("Damage.added{physical}", 3.0))
        .id();

    // Equipment is nothing special: an entity carrying a ModifierCollection,
    // attached through a relationship. Attach applies, detach removes.
    let sword = world
        .spawn((
            ModifierCollection(modifiers! {
                "Damage.added{physical, sword}" => 12.0,
                "Damage.increased" => 0.15,
            }),
            AttachedTo(hero),
        ))
        .id();
    world.flush();
    report(world, hero, "after equipping the sword");

    // An enchant is a detachable ModifierSet applied by hand — keep the
    // receipt to remove exactly what the enchant added.
    let enchant = modifiers! { "Damage.added{fire, sword}" => 6.0 };
    let receipt = world
        .run_system_once(move |mut stats: StatsMutator| stats.apply(hero, &enchant).unwrap())
        .unwrap();
    report(world, hero, "after enchanting");

    world
        .run_system_once(move |mut stats: StatsMutator| stats.remove(&receipt))
        .unwrap();
    report(world, hero, "after removing the enchant");

    // Handing the sword to a squire is one operation.
    let squire = world.spawn(Stats::new()).id();
    world.entity_mut(sword).insert(AttachedTo(squire));
    world.flush();
    report(world, hero, "hero, after giving the sword away");
    report(world, squire, "squire, holding the sword");
}

fn report(world: &mut bevy_ecs::world::World, who: bevy_ecs::entity::Entity, when: &str) {
    let (physical, sword_hit, fire_sword) = world
        .run_system_once(move |mut stats: StatsMutator| {
            (
                stats.get_with_tags(who, "Damage.added", "physical").unwrap(),
                stats
                    .get_with_tags(who, "Damage.added", "physical, sword")
                    .unwrap(),
                stats
                    .get_with_tags(who, "Damage.added", "physical, fire, sword")
                    .unwrap(),
            )
        })
        .unwrap();
    println!("{when}:");
    println!("  physical damage:            {physical}");
    println!("  physical|sword damage:      {sword_hit}");
    println!("  physical|fire|sword damage: {fire_sword}");
}
