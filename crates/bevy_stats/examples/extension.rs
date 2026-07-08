//! Building a game's typed vocabulary on top of the string-keyed core:
//! reusable stat-structure "builders", typed accessor extensions, and a
//! manual `StatsBound` impl syncing a third-party-style component (a physics
//! `Mass`) with a stat.
#![expect(clippy::print_stdout, reason = "example output")]

use bevy_app::App;
use bevy_ecs::entity::Entity;
use bevy_ecs::prelude::Component;
use bevy_ecs::system::RunSystemOnce;
use bevy_stats::prelude::*;

// --- A reusable stat-structure builder -------------------------------------
// Games compose their archetypes out of functions returning seeded `Stats`
// (or `ModifierSet`s) instead of formalizing "classes" in the type system.

fn creature(vitality: f32, strength: f32) -> Stats {
    Stats::new()
        .with("Vitality", vitality)
        .with("Strength", strength)
        .with("Life.max", "Vitality * 10 + Strength * 2")
        .with("CarryWeight", "Strength * 5")
        .with("Mass", "10 + CarryWeight / 20")
}

// --- Typed helpers over the string-keyed core -------------------------------

trait RpgStats {
    fn life_max(&mut self, entity: Entity) -> f32;
    fn strength(&mut self, entity: Entity) -> f32;
}

impl RpgStats for StatsMutator<'_, '_> {
    fn life_max(&mut self, entity: Entity) -> f32 {
        self.get(entity, "Life.max")
    }
    fn strength(&mut self, entity: Entity) -> f32 {
        self.get(entity, "Strength")
    }
}

// --- Integrating a third-party component ------------------------------------
// Pretend `Mass` comes from a physics crate: we can still sync it by
// implementing `StatsBound` manually (the derive is only for our own types).

#[derive(Component, Default, Debug)]
struct Mass(f32);

impl StatsBound for Mass {
    fn write_stats(&self, _: Entity, _: &mut StatsMutator) {}
    fn read_stats(&mut self, entity: Entity, stats: &mut StatsMutator) -> bool {
        let mass = stats.get(entity, "Mass");
        let changed = self.0 != mass;
        self.0 = mass;
        changed
    }
}

fn main() {
    let mut app = App::new();
    app.add_plugins(StatsPlugin::new());
    app.register_stats_component::<Mass>();

    let hero = app.world_mut().spawn((creature(12.0, 20.0), Mass::default())).id();
    app.update();

    let (life, strength) = app
        .world_mut()
        .run_system_once(move |mut stats: StatsMutator| {
            (stats.life_max(hero), stats.strength(hero))
        })
        .unwrap();
    println!("life {life}, strength {strength}");
    println!("physics mass synced: {:?}", app.world().get::<Mass>(hero).unwrap());

    // Overburden the hero: the physics component follows the stat graph.
    app.world_mut()
        .run_system_once(move |mut stats: StatsMutator| {
            stats.add_modifier(hero, "CarryWeight", 400.0).unwrap();
        })
        .unwrap();
    app.update();
    println!("overburdened mass:   {:?}", app.world().get::<Mass>(hero).unwrap());
}
