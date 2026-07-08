//! A cross-entity scaling chain: an aura empowers a hero, and a weapon
//! scales off its wielder's Strength — three entities, two links. Re-point
//! one link and the whole chain rewires and recomputes.
#![expect(clippy::print_stdout, reason = "example output")]

use bevy_app::App;
use bevy_ecs::system::RunSystemOnce;
use bevy_stats::prelude::*;

fn main() {
    let mut app = App::new();
    app.add_plugins(StatsPlugin::new());
    let world = app.world_mut();

    let war_banner = world.spawn(Stats::new().with("StrengthBonus", 5.0)).id();
    let hero = world
        .spawn(
            Stats::new()
                .with("Strength.base", 10.0)
                .with("Strength", "Strength.base + aura@StrengthBonus"),
        )
        .id();
    let ogre = world.spawn(Stats::new().with("Strength", 40.0)).id();
    // The weapon reads *its wielder's* Strength — whoever that currently is.
    let greatsword = world
        .spawn(Stats::new().with("Damage", "wielder@Strength * 2"))
        .id();

    world
        .run_system_once(move |mut stats: StatsMutator| {
            stats.set_link(hero, "aura", war_banner).unwrap();
            stats.set_link(greatsword, "wielder", hero).unwrap();
        })
        .unwrap();
    let dmg = |world: &mut bevy_ecs::world::World| {
        world
            .run_system_once(move |mut stats: StatsMutator| stats.get(greatsword, "Damage"))
            .unwrap()
    };
    println!("hero (10 base + 5 aura) wielding: damage = {}", dmg(world)); // 30

    // A change at the far end of the chain propagates through both links.
    world
        .run_system_once(move |mut stats: StatsMutator| {
            stats.set(war_banner, "StrengthBonus", 15.0).unwrap();
        })
        .unwrap();
    println!("stronger aura:                    damage = {}", dmg(world)); // 50

    // Hand the greatsword to the ogre: one operation, everything recomputes.
    world
        .run_system_once(move |mut stats: StatsMutator| {
            stats.set_link(greatsword, "wielder", ogre).unwrap();
        })
        .unwrap();
    println!("ogre (40) wielding:               damage = {}", dmg(world)); // 80

    // Despawning the wielder cleans up: the link dangles and reads as 0.
    world.despawn(ogre);
    world.flush();
    println!("wielder despawned:                damage = {}", dmg(world)); // 0
}
