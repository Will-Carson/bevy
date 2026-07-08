//! A role-based ability instant: a fireball whose damage pulls from several
//! participants (caster, catalyst, target) for one application — previewed
//! before committing, gated by a requirement, and leaving no lingering
//! modifier behind.
#![expect(clippy::print_stdout, reason = "example output")]

use bevy_app::App;
use bevy_ecs::system::RunSystemOnce;
use bevy_stats::prelude::*;

fn main() {
    let mut app = App::new();
    app.add_plugins(StatsPlugin::new());
    let world = app.world_mut();

    let caster = world
        .spawn(Stats::new().with("Intelligence", 18.0).with("Mana", 25.0))
        .id();
    let catalyst = world.spawn(Stats::new().with("SpellPower", 12.0)).id();
    let goblin = world
        .spawn(Stats::new().with("Life", 60.0).with("FireResist", 0.25))
        .id();

    // The ability, declared once and reused: damage scales on the caster's
    // Intelligence and the catalyst's SpellPower, mitigated by the target's
    // resistance; casting also spends the caster's mana.
    let fireball = InstantEffect::new()
        .sub(
            "target",
            "Life",
            "(caster@Intelligence * 1.5 + catalyst@SpellPower) * (1 - target@FireResist)",
        )
        .sub("caster", "Mana", 10.0);
    let can_cast = Requirement::parse("Mana >= 10 && Intelligence >= 15").unwrap();

    let roles = Roles::new()
        .with("caster", caster)
        .with("catalyst", catalyst)
        .with("target", goblin);

    world
        .run_system_once(move |mut stats: StatsMutator| {
            assert!(stats.check(caster, &can_cast).unwrap());

            // Preview: exact outcomes, nothing committed — perfect for UI.
            println!("preview:");
            for outcome in stats.preview_effect(&fireball, &roles).unwrap() {
                println!(
                    "  {:?} {} by {:.1} -> base {:.1}",
                    outcome.op, outcome.stat, outcome.amount, outcome.new_base
                );
            }
            println!(
                "goblin life before commit: {}",
                stats.get(goblin, "Life")
            );

            // Commit. One-shot: bases move, no modifier lingers.
            stats.apply_effect(&fireball, &roles).unwrap();
            println!("goblin life after commit:  {}", stats.get(goblin, "Life"));
            println!("caster mana after commit:  {}", stats.get(caster, "Mana"));

            // Second cast drains mana below the gate.
            stats.apply_effect(&fireball, &roles).unwrap();
            println!(
                "can cast a third time? {}",
                stats.check(caster, &can_cast).unwrap()
            );
        })
        .unwrap();
}
