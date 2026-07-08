//! Benchmarks: cached reads, single writes, and propagation through
//! dependency chains of increasing depth and fan-out.

use core::hint::black_box;

use bevy_app::App;
use bevy_ecs::entity::Entity;
use bevy_ecs::system::SystemState;
use bevy_ecs::world::World;
use bevy_stats::prelude::*;
use criterion::{Criterion, criterion_group, criterion_main};

fn bench_app() -> App {
    let mut app = App::new();
    app.add_plugins(
        StatsPlugin::new()
            .with_tags(["fire", "cold", "sword"])
            .with_segment_reduction("more", Reduction::Product),
    );
    app
}

type MutatorState = SystemState<StatsMutator<'static, 'static>>;

fn cached_read(c: &mut Criterion) {
    let mut app = bench_app();
    let world = app.world_mut();
    let hero = world
        .spawn(
            Stats::new()
                .with("Vitality", 12.0)
                .with("MaxHealth", "Vitality * 10 + 50")
                .with("Damage.added{fire, sword}", 10.0),
        )
        .id();
    let mut state: MutatorState = SystemState::new(world);
    {
        let mut stats = state.get_mut(world).unwrap();
        stats.get(hero, "MaxHealth");
        stats.get_with_tags(hero, "Damage.added", "fire, sword").unwrap();
    }
    c.bench_function("cached_read", |b| {
        b.iter(|| {
            let mut stats = state.get_mut(world).unwrap();
            black_box(stats.get(black_box(hero), "MaxHealth"))
        });
    });
    let fire_sword = {
        let stats = state.get_mut(world).unwrap();
        stats.tag_registry().parse("fire, sword").unwrap()
    };
    c.bench_function("cached_read_tagged", |b| {
        b.iter(|| {
            let mut stats = state.get_mut(world).unwrap();
            black_box(stats.get_filtered(black_box(hero), "Damage.added", fire_sword))
        });
    });
}

fn single_write(c: &mut Criterion) {
    // A write to a stat nothing depends on: pure write-side overhead.
    let mut app = bench_app();
    let world = app.world_mut();
    let hero = world.spawn(Stats::new().with("Loneliness", 1.0)).id();
    let mut state: MutatorState = SystemState::new(world);
    {
        let mut stats = state.get_mut(world).unwrap();
        stats.get(hero, "Loneliness");
    }
    let mut i = 0.0f32;
    c.bench_function("single_write_no_dependents", |b| {
        b.iter(|| {
            i += 1.0;
            let mut stats = state.get_mut(world).unwrap();
            stats.set(black_box(hero), "Loneliness", i).unwrap();
        });
    });

    // A write with a 10-stat fan-out, all cached.
    let mut app = bench_app();
    let world = app.world_mut();
    let mut seeded = Stats::new().with("Source", 1.0);
    for k in 0..10 {
        seeded = seeded.with(&format!("Derived{k}"), format!("Source * {k} + 1").as_str());
    }
    let fan = world.spawn(seeded).id();
    let mut state: MutatorState = SystemState::new(world);
    {
        let mut stats = state.get_mut(world).unwrap();
        for k in 0..10 {
            stats.get(fan, &format!("Derived{k}"));
        }
    }
    let mut i = 0.0f32;
    c.bench_function("single_write_fanout_10", |b| {
        b.iter(|| {
            i += 1.0;
            let mut stats = state.get_mut(world).unwrap();
            stats.set(black_box(fan), "Source", i).unwrap();
        });
    });
}

fn chain_propagation(c: &mut Criterion) {
    // One entity, a linear chain: Stat0 -> Stat1 -> ... -> StatN.
    for depth in [10usize, 50] {
        let mut app = bench_app();
        let world = app.world_mut();
        let mut seeded = Stats::new().with("Stat0", 1.0);
        for k in 1..=depth {
            let prev = k - 1;
            seeded = seeded.with(&format!("Stat{k}"), format!("Stat{prev} + 1").as_str());
        }
        let e = world.spawn(seeded).id();
        let mut state: MutatorState = SystemState::new(world);
        {
            let mut stats = state.get_mut(world).unwrap();
            for k in 0..=depth {
                stats.get(e, &format!("Stat{k}"));
            }
        }
        let mut i = 0.0f32;
        c.bench_function(&format!("propagate_chain_depth_{depth}"), |b| {
            b.iter(|| {
                i += 1.0;
                let mut stats = state.get_mut(world).unwrap();
                stats.set(black_box(e), "Stat0", i).unwrap();
            });
        });
    }

    // Cross-entity chain: each entity reads the previous through a link.
    let mut app = bench_app();
    let world = app.world_mut();
    let entities: Vec<Entity> = (0..10)
        .map(|k| {
            if k == 0 {
                world.spawn(Stats::new().with("Power", 1.0)).id()
            } else {
                world
                    .spawn(Stats::new().with("Power", "prev@Power + 1"))
                    .id()
            }
        })
        .collect();
    let first = entities[0];
    let last = *entities.last().unwrap();
    let mut state: MutatorState = SystemState::new(world);
    {
        let mut stats = state.get_mut(world).unwrap();
        for pair in entities.windows(2) {
            stats.set_link(pair[1], "prev", pair[0]).unwrap();
        }
        assert_eq!(stats.get(last, "Power"), 10.0);
    }
    let mut i = 0.0f32;
    c.bench_function("propagate_cross_entity_10_hops", |b| {
        b.iter(|| {
            i += 1.0;
            let mut stats = state.get_mut(world).unwrap();
            stats.set(black_box(first), "Power", i).unwrap();
        });
    });
}

fn uncached_vs_cached(c: &mut Criterion) {
    // How much a cache hit saves against a fresh evaluation of a small tree.
    let mut app = bench_app();
    let world = app.world_mut();
    let e = world
        .spawn(
            Stats::new()
                .with("A", 3.0)
                .with("B", "A * 2")
                .with("C", "A + B")
                .with("D", "min(B, C) + max(A, 2) * clamp(C, 0, 10)"),
        )
        .id();
    let mut state: MutatorState = SystemState::new(world);
    {
        let mut stats = state.get_mut(world).unwrap();
        stats.get(e, "D");
    }
    fn read_d(world: &mut World, state: &mut MutatorState, e: Entity) -> f32 {
        let mut stats = state.get_mut(world).unwrap();
        stats.get(e, "D")
    }
    c.bench_function("read_small_tree_cached", |b| {
        b.iter(|| black_box(read_d(world, &mut state, e)));
    });
}

criterion_group!(
    benches,
    cached_read,
    single_write,
    chain_propagation,
    uncached_vs_cached
);
criterion_main!(benches);
