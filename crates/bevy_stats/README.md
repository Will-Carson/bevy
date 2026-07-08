# `bevy_stats`

A dependency-graph attribute/stat system for [Bevy](https://bevy.org).

Define interconnected stats (strength, health, damage, resistances, …) as
declarative relationships instead of hand-written update code. Change one
value and everything derived from it — on the same entity or across linked
entities — recomputes automatically, exactly once, in dependency order.
Reads are cached; writes touch only what actually depends on them.

## Quick start

```rust
use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_stats::prelude::*;

fn main() {
    App::new()
        .add_plugins(
            StatsPlugin::new()
                .with_tags(["fire", "cold", "lightning", "sword"])
                .with_tag_group("elemental", ["fire", "cold", "lightning"])
                // `Damage.more`, `Speed.more`, ... multiply (1 + v) factors.
                .with_segment_reduction("more", Reduction::Product),
        )
        .add_systems(Startup, spawn)
        .add_systems(Update, level_up)
        .run();
}

fn spawn(mut commands: Commands) {
    // Stats as formulas, not systems: changing Vitality updates MaxHealth
    // on its own. Literals seed replaceable base values.
    commands.spawn(
        Stats::new()
            .with("Vitality", 12.0)
            .with("MaxHealth", "Vitality * 10 + 50"),
    );
}

fn level_up(query: Query<Entity, With<Stats>>, mut stats: StatsMutator) {
    for hero in &query {
        stats.set(hero, "Vitality", 13.0).unwrap();
        assert_eq!(stats.get(hero, "MaxHealth"), 180.0); // already recomputed
    }
}
```

## The pieces

| Concept | Surface |
|---|---|
| Stat container | `Stats` component; string keys, dot-hierarchical (`Damage.added`) |
| Reading | `StatsMutator::get` / `get_filtered` (cached), `StatsReader` (read-only) |
| Writing | `StatsMutator::set` (replaceable base), `add_modifier`, `apply`/`remove` |
| Modifier bundles | `ModifierSet` + `modifiers! { ... }`; attach via `ModifierCollection` + `AttachedTo(entity)` |
| Expressions | `"Damage.added * (1 + Damage.increased)"`, `min`/`max`/`abs`/`clamp`, comparisons and `&& || !` yielding 1/0 |
| Cross-entity | `wielder@Strength` reads through a named link; re-point with `set_link` |
| Tags | `TagSet` (≤64 bits), subset matching, groups (`elemental` = fire ∪ cold ∪ lightning) |
| One-shot effects | `InstantEffect` + `Roles` (attacker/target/…), `preview_effect` / `apply_effect` |
| Requirements | `Requirement::parse("Strength >= 20")`, `stats.check(entity, &req)` |
| Component sync | `#[derive(StatSync)]` with `#[stat("Life.max")]` fields, or implement `StatsBound` by hand |
| Reactions | `StatChanged` entity event, `Changed<Stats>` query filter |

## Semantics worth knowing

- **Reductions**: `Sum` (default; empty ⇒ 0), `Product` (each value `v`
  contributes a factor `1 + v`; empty ⇒ 1), or `Reduction::custom(...)`.
  Configure per exact name or per last dot-segment (`"more"`).
- **Tag matching**: a modifier participates in a query iff its tags are a
  **subset** of the query's tags. Untagged modifiers apply everywhere;
  a `fire|sword` modifier does *not* apply to a plain `fire` query.
  Insert broadly, query specifically. New tag combinations resolve and
  cache on first use — no up-front enumeration.
- **Bases vs. modifiers**: `set()` writes a single replaceable, untagged
  base slot; modifiers accumulate and detach symmetrically via handles or
  `AppliedModifiers` receipts. Replacing a base never disturbs formula or
  tagged contributions.
- **Expression edge cases**: division by (near-)zero yields 0; non-finite
  power results yield 0; `==`/`!=` compare within `1e-6`; missing stats
  evaluate to their reduction identity.
- **Lifecycle**: despawning a stat-bearing entity tears down all its
  dependency edges and recomputes downstream readers (dangling links read
  as missing stats).

## Examples

Run from the repository root:

```sh
cargo run -p bevy_stats --example equipment      # equip / enchant / unequip flow
cargo run -p bevy_stats --example cross_entity   # aura -> hero -> weapon scaling chain
cargo run -p bevy_stats --example tagged_damage  # PoE-style tagged damage weapon
cargo run -p bevy_stats --example component_sync # derive-based two-way sync
cargo run -p bevy_stats --example ability_instant# role-based ability instants
cargo run -p bevy_stats --example damage_pipeline# multi-phase damage resolution
cargo run -p bevy_stats --example extension      # typed helpers over the string core
```

Benchmarks: `cargo bench -p bevy_stats`.
