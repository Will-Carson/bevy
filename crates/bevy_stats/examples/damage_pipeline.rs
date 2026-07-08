//! A multi-phase damage resolution: raw hit → mitigation → life, expressed
//! as a sequence of instant effects. Ops inside one effect share a
//! pre-effect snapshot; sequencing *between* effects gives you phases, with
//! every intermediate value inspectable (and previewable) as a real stat.
#![expect(clippy::print_stdout, reason = "example output")]

use bevy_app::App;
use bevy_ecs::system::RunSystemOnce;
use bevy_stats::prelude::*;

fn main() {
    let mut app = App::new();
    app.add_plugins(StatsPlugin::new().with_segment_reduction("more", Reduction::Product));
    let world = app.world_mut();

    let hero = world
        .spawn(
            Stats::new()
                .with("Strength", 24.0)
                .with("Damage.added", "Strength / 4")
                .with("Damage", "(WeaponDamage + Damage.added) * Damage.more"),
        )
        .id();
    // The weapon feeds the hero's damage through a link.
    let axe = world.spawn(Stats::new().with("Damage", 14.0)).id();
    world
        .run_system_once(move |mut stats: StatsMutator| {
            stats.add_modifier(hero, "WeaponDamage", "weapon@Damage").unwrap();
            stats.set_link(hero, "weapon", axe).unwrap();
        })
        .unwrap();

    let knight = world
        .spawn(
            Stats::new()
                .with("Armor", 30.0)
                .with("Life", 120.0)
                // Armor gives diminishing mitigation, capped at 75%.
                .with("Mitigation", "clamp(Armor / (Armor + 50), 0, 0.75)"),
        )
        .id();

    // Phase 1: stamp the raw hit onto the defender as a transient stat.
    let land_hit = InstantEffect::new().set("defender", "Incoming.raw", "attacker@Damage");
    // Phase 2: mitigate — reads the phase-1 result and the defender's own stats.
    let mitigate = InstantEffect::new().set(
        "defender",
        "Incoming.final",
        "Incoming.raw * (1 - Mitigation)",
    );
    // Phase 3: apply to life.
    let apply = InstantEffect::new().sub("defender", "Life", "Incoming.final");

    let roles = Roles::new().with("attacker", hero).with("defender", knight);
    world
        .run_system_once(move |mut stats: StatsMutator| {
            println!("hero damage:   {}", stats.get(hero, "Damage"));
            println!("knight life:   {}", stats.get(knight, "Life"));
            for (phase, effect) in [("raw hit", &land_hit), ("mitigate", &mitigate), ("apply", &apply)]
            {
                let outcomes = stats.apply_effect(effect, &roles).unwrap();
                for o in &outcomes {
                    println!("phase {phase:>8}: {} <- {:.2}", o.stat, o.new_base);
                }
            }
            println!("knight life after the exchange: {:.2}", stats.get(knight, "Life"));

            // Buff the hero mid-fight and resolve another swing: every phase
            // picks up the new numbers automatically.
            stats.add_modifier(hero, "Damage.more", 0.5).unwrap();
            for effect in [&land_hit, &mitigate, &apply] {
                stats.apply_effect(effect, &roles).unwrap();
            }
            println!("knight life after a buffed swing: {:.2}", stats.get(knight, "Life"));
        })
        .unwrap();
}
