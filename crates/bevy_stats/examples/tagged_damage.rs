//! A PoE-style tagged-damage weapon: a compound `Damage` stat defined as
//! `added × (1 + increased) × more`, where every part accumulates its own
//! tag-filtered modifiers. Insert broadly, query specifically — new tag
//! combinations resolve on first use.
#![expect(clippy::print_stdout, reason = "example output")]

use bevy_app::App;
use bevy_ecs::system::RunSystemOnce;
use bevy_stats::prelude::*;

fn main() {
    let mut app = App::new();
    app.add_plugins(
        StatsPlugin::new()
            .with_tags(["fire", "cold", "lightning", "physical", "sword", "attack", "spell"])
            .with_tag_group("elemental", ["fire", "cold", "lightning"])
            .with_segment_reduction("more", Reduction::Product),
    );
    let world = app.world_mut();

    let hero = world
        .spawn(
            Stats::new()
                // The compound: parts inherit the query's tags automatically.
                .with("Damage", "Damage.added * (1 + Damage.increased) * Damage.more")
                // A rusty sword.
                .with("Damage.added{physical, sword, attack}", 10.0)
                // "+8 fire damage to attacks" (a flaming oil).
                .with("Damage.added{fire, attack}", 8.0)
                // "20% increased sword damage" (a passive).
                .with("Damage.increased{sword}", 0.2)
                // "50% increased elemental damage" — tagged with the whole
                // group, so it only applies when the query covers all of it.
                .with("Damage.increased{elemental}", 0.5)
                // "30% more fire damage" (a support gem).
                .with("Damage.more{fire}", 0.3),
        )
        .id();

    let mut q = |tags: &'static str| {
        let v = world
            .run_system_once(move |mut stats: StatsMutator| {
                stats.get_with_tags(hero, "Damage", tags).unwrap()
            })
            .unwrap();
        println!("Damage{{{tags}}} = {v:.2}");
    };

    // A plain physical sword swing: the fire oil does not apply.
    q("physical, sword, attack"); // 10 * 1.2 * 1 = 12
    // The full fiery swing: both added parts, sword bonus, fire more-multiplier.
    q("physical, fire, sword, attack"); // 18 * 1.2 * 1.3 = 28.08
    // A fire spell: nothing applies — the sword and the oil are attack-only
    // (their tags are not a subset of the query), so the spell deals 0.
    q("fire, spell"); // 0 * 1.0 * 1.3 = 0
    // Querying the *group* covers every member: the fire oil participates,
    // and the elemental passive (tagged with the whole group) finally kicks
    // in because the query now covers all of it.
    q("elemental, attack"); // 8 * 1.5 * 1.3 = 15.6
    // Nothing enumerated these combinations up front — each resolved and
    // cached on first use.
}
