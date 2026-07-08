//! Two-way component synchronization: an ordinary `Health` component whose
//! fields stay in sync with the stat graph — no hand-written plumbing.
//!
//! `max` mirrors the resolved `Life.max` stat (read); `current` is owned by
//! gameplay code and written back into `Life.current` (write). Change
//! detection guards both directions, so nothing churns when nothing moves.
#![expect(clippy::print_stdout, reason = "example output")]

use bevy_app::App;
use bevy_ecs::prelude::Component;
use bevy_ecs::system::RunSystemOnce;
use bevy_stats::prelude::*;

#[derive(Component, StatSync, Debug)]
struct Health {
    #[stat("Life.max")]
    max: f32,
    #[stat("Life.current", write)]
    current: f32,
}

fn main() {
    let mut app = App::new();
    app.add_plugins(StatsPlugin::new());
    app.register_stats_component::<Health>();

    let hero = app
        .world_mut()
        .spawn((
            Stats::new()
                .with("Vitality", 10.0)
                .with("Life.max", "Vitality * 10"),
            // Initialize-on-spawn: `current` seeds `Life.current`, `max`
            // pulls the resolved formula value.
            Health {
                max: 0.0,
                current: 100.0,
            },
        ))
        .id();
    app.update();
    println!("on spawn:        {:?}", app.world().get::<Health>(hero).unwrap());

    // Stats -> component: a vitality buff raises max health; the component
    // sees it on the next sync sweep.
    app.world_mut()
        .run_system_once(move |mut stats: StatsMutator| {
            stats.add_modifier(hero, "Vitality", 5.0).unwrap();
        })
        .unwrap();
    app.update();
    println!("vitality buffed: {:?}", app.world().get::<Health>(hero).unwrap());

    // Component -> stats: gameplay code just mutates the component; the
    // authoritative value lands in the graph, where formulas can use it.
    app.world_mut().get_mut::<Health>(hero).unwrap().current = 42.0;
    app.update();
    let in_graph = app
        .world_mut()
        .run_system_once(move |mut stats: StatsMutator| stats.get(hero, "Life.current"))
        .unwrap();
    println!("after damage:    {:?}", app.world().get::<Health>(hero).unwrap());
    println!("Life.current in the stat graph: {in_graph}");
}
