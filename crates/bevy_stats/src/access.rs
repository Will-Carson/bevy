//! [`StatsMutator`] and [`StatsReader`]: the system parameters through which
//! stats are read and written.
//!
//! All mutation goes through [`StatsMutator`], which keeps the dependency
//! graph consistent: when a value changes, exactly the stats that (directly
//! or transitively) depend on it are recomputed, once each, in dependency
//! order — including across entity boundaries through named links.

use crate::error::StatError;
use crate::expr::{
    CExpr, CStatRef, Expression, RefResolver, collect_crefs, compile, eval, parse_stat_path,
};
use crate::modifier::{
    AppliedModifiers, IntoModifierValue, Modifier, ModifierHandle, ModifierSet, ModifierValue,
};
use crate::stats::{Dependent, PendingEntry, StatConfig, Stats, StoredModifier, StoredValue};
use crate::tags::{TagRegistry, TagSet};
use bevy_ecs::change_detection::DetectChangesMut;
use bevy_ecs::entity::Entity;
use bevy_ecs::event::EntityEvent;
use bevy_ecs::resource::Resource;
use bevy_ecs::system::{Commands, Query, Res, ResMut, SystemParam};
use bevy_platform::collections::{HashMap, HashSet};
use core::cell::RefCell;

/// Allocates unique ids for [`ModifierHandle`]s.
#[derive(Resource, Default, Debug)]
pub struct ModifierIdGen(u64);

impl ModifierIdGen {
    fn next_id(&mut self) -> u64 {
        self.0 += 1;
        self.0
    }
}

/// The entities whose resolved stat values changed since the last sync
/// sweep. Fed by [`StatsMutator`]'s recomputation, consumed by the
/// [`StatsSyncSet::ReadFromStats`](crate::StatsSyncSet) systems, and cleared
/// at the end of `PostUpdate` each frame.
#[derive(Resource, Default, Debug)]
pub struct DirtyStats(pub(crate) HashSet<Entity>);

/// The reverse dependency edges of the stat graph, stored globally so they
/// survive `Stats` component replacement (an edge is owned by the
/// *dependent's* modifier, not by the source entity).
///
/// Edges are reference-counted: several modifiers may create the same
/// (source stat → dependent stat) edge.
#[derive(Resource, Default, Debug)]
pub struct StatGraph {
    edges: HashMap<Entity, HashMap<String, HashMap<Dependent, u32>>>,
}

impl StatGraph {
    fn adjust(
        &mut self,
        source_entity: Entity,
        source_stat: String,
        dependent: Dependent,
        delta: i32,
    ) {
        let by_stat = self.edges.entry(source_entity).or_default();
        let map = by_stat.entry(source_stat.clone()).or_default();
        if delta > 0 {
            *map.entry(dependent).or_insert(0) += delta as u32;
        } else if let Some(count) = map.get_mut(&dependent) {
            *count = count.saturating_sub((-delta) as u32);
            if *count == 0 {
                map.remove(&dependent);
            }
        }
        // Keep the store tidy: drop empty levels so despawned entities do
        // not accumulate.
        if map.is_empty() {
            by_stat.remove(&source_stat);
            if by_stat.is_empty() {
                self.edges.remove(&source_entity);
            }
        }
    }

    fn dependents_of(&self, entity: Entity, stat: &str) -> impl Iterator<Item = &Dependent> {
        self.edges
            .get(&entity)
            .and_then(|by_stat| by_stat.get(stat))
            .into_iter()
            .flat_map(HashMap::keys)
    }

    /// The number of distinct stats (on any entity) whose modifier
    /// expressions currently read `stat` on `entity`. Introspection for
    /// debugging and tests.
    pub fn dependent_count(&self, entity: Entity, stat: &str) -> usize {
        self.edges
            .get(&entity)
            .and_then(|by_stat| by_stat.get(stat))
            .map_or(0, HashMap::len)
    }
}

/// [`EntityEvent`] triggered (via [`Commands`]) whenever a resolved stat
/// value on an entity actually changes. Only stats that have been evaluated
/// at least once (and are therefore cached) produce events.
#[derive(EntityEvent, Debug, Clone)]
pub struct StatChanged {
    /// The entity whose stat changed.
    pub entity: Entity,
    /// The name of the stat that changed.
    pub stat: String,
}

// ---------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------

/// Read access to `Stats` components, abstract over `Query<&Stats>` and
/// `Query<&mut Stats>`.
pub(crate) trait StatsLookup {
    fn stats_of(&self, entity: Entity) -> Option<&Stats>;
}

impl StatsLookup for Query<'_, '_, &'static mut Stats> {
    fn stats_of(&self, entity: Entity) -> Option<&Stats> {
        self.get(entity).ok()
    }
}

impl StatsLookup for Query<'_, '_, &'static Stats> {
    fn stats_of(&self, entity: Entity) -> Option<&Stats> {
        self.get(entity).ok()
    }
}

/// Everything evaluation needs.
pub(crate) struct EvalEnv<'a> {
    pub lookup: &'a dyn StatsLookup,
    pub config: &'a StatConfig,
    /// An entity treated as already-gone (used while cleaning up a despawn).
    pub excluded: Option<Entity>,
    /// Cycle guard: the (entity, stat, tags) frames currently being computed.
    pub stack: RefCell<Vec<(Entity, String, TagSet)>>,
}

impl<'a> EvalEnv<'a> {
    pub fn new(lookup: &'a dyn StatsLookup, config: &'a StatConfig) -> Self {
        EvalEnv {
            lookup,
            config,
            excluded: None,
            stack: RefCell::new(Vec::new()),
        }
    }
}

/// Named ad-hoc sources (effect roles). Resolution order for `name@Stat`:
/// roles first, then the owning entity's links.
pub(crate) struct EvalFrame<'a> {
    pub env: &'a EvalEnv<'a>,
    /// The entity owning the expression being evaluated; bare stat
    /// references and links resolve against it.
    pub owner: Entity,
    /// Ad-hoc role bindings (instant effects, requirement checks).
    pub roles: Option<&'a HashMap<String, Entity>>,
    /// The tags of the enclosing query; references without an explicit
    /// filter inherit them.
    pub query_tags: TagSet,
}

impl RefResolver for EvalFrame<'_> {
    fn resolve(&self, r: &CStatRef) -> f32 {
        let target = match &r.source {
            None => self.owner,
            Some(name) => {
                let from_role = self.roles.and_then(|roles| roles.get(name)).copied();
                let resolved = from_role.or_else(|| {
                    self.env
                        .lookup
                        .stats_of(self.owner)
                        .and_then(|s| s.link(name))
                });
                match resolved {
                    Some(e) => e,
                    // A dangling or unset link reads as a missing stat.
                    None => return self.env.config.reduction_for(&r.stat).reduce(&[]),
                }
            }
        };
        let tags = r.tags.unwrap_or(self.query_tags);
        eval_stat(self.env, target, &r.stat, tags)
    }
}

/// Evaluates one stat on one entity under a tag query, using caches where
/// they exist. This is the pull side of the system; consistency is
/// guaranteed by [`StatsMutator`] clearing and refilling caches in dependency
/// order on every change.
pub(crate) fn eval_stat(env: &EvalEnv<'_>, entity: Entity, stat: &str, tags: TagSet) -> f32 {
    let missing = || env.config.reduction_for(stat).reduce(&[]);
    if env.excluded == Some(entity) {
        return missing();
    }
    let Some(stats) = env.lookup.stats_of(entity) else {
        return missing();
    };
    let Some(node) = stats.nodes.get(stat) else {
        return missing();
    };
    if let Some(v) = node.cache.get(&tags) {
        return *v;
    }
    // Cycle guard.
    {
        let mut stack = env.stack.borrow_mut();
        if stack
            .iter()
            .any(|(e, s, t)| *e == entity && s == stat && *t == tags)
        {
            log::warn!("stat dependency cycle detected at `{stat}`; evaluating as 0");
            return 0.0;
        }
        stack.push((entity, stat.to_string(), tags));
    }
    let mut values = Vec::with_capacity(node.modifiers.len() + 1);
    if let Some(base) = node.base {
        values.push(base);
    }
    for modifier in &node.modifiers {
        if !modifier.tags.applies_to(tags) {
            continue;
        }
        match &modifier.value {
            StoredValue::Literal(v) => values.push(*v),
            StoredValue::Expr(cexpr) => {
                let frame = EvalFrame {
                    env,
                    owner: entity,
                    roles: None,
                    query_tags: tags,
                };
                values.push(eval(cexpr, &frame));
            }
        }
    }
    let result = env.config.reduction_for(stat).reduce(&values);
    env.stack.borrow_mut().pop();
    result
}

// ---------------------------------------------------------------------------
// StatsMutator
// ---------------------------------------------------------------------------

/// Full read/write access to the stat graph.
///
/// Every mutation recomputes exactly the (transitively) affected stats, once
/// each, in dependency order, and marks the affected `Stats` components as
/// changed only when a resolved value actually moved — so change detection
/// and [`StatChanged`] events stay quiet for no-op writes.
#[derive(SystemParam)]
pub struct StatsMutator<'w, 's> {
    pub(crate) stats: Query<'w, 's, &'static mut Stats>,
    pub(crate) tags: Res<'w, TagRegistry>,
    pub(crate) config: Res<'w, StatConfig>,
    graph: ResMut<'w, StatGraph>,
    ids: ResMut<'w, ModifierIdGen>,
    dirty: ResMut<'w, DirtyStats>,
    commands: Commands<'w, 's>,
}

impl StatsMutator<'_, '_> {
    // -- reads --------------------------------------------------------------

    /// Resolves a stat's value with no tag filter. Missing entities, stats,
    /// or modifiers evaluate to the stat's reduction identity (`0` for sum,
    /// `1` for product). The result is cached, making repeated reads cheap.
    pub fn get(&mut self, entity: Entity, stat: &str) -> f32 {
        self.get_filtered(entity, stat, TagSet::NONE)
    }

    /// Resolves a stat's value under a tag query: only modifiers whose tags
    /// are a subset of `tags` participate. A new tag combination is resolved
    /// and cached on first use.
    pub fn get_filtered(&mut self, entity: Entity, stat: &str, tags: TagSet) -> f32 {
        if let Some(v) = self
            .stats
            .get(entity)
            .ok()
            .and_then(|s| s.nodes.get(stat))
            .and_then(|n| n.cache.get(&tags))
        {
            return *v;
        }
        let env = EvalEnv::new(&self.stats, &self.config);
        let value = eval_stat(&env, entity, stat, tags);
        // Cache without tripping change detection: a read is not a change.
        if let Ok(mut stats) = self.stats.get_mut(entity) {
            stats
                .bypass_change_detection()
                .nodes
                .entry(stat.to_string())
                .or_default()
                .cache
                .insert(tags, value);
        }
        value
    }

    /// Like [`get_filtered`](Self::get_filtered), with the tag query given as
    /// a name list such as `"fire, sword"`. Fails on unregistered tag names.
    pub fn get_with_tags(
        &mut self,
        entity: Entity,
        stat: &str,
        tags: &str,
    ) -> Result<f32, StatError> {
        let tags = self.tags.parse(tags)?;
        Ok(self.get_filtered(entity, stat, tags))
    }

    /// Evaluates an ad-hoc expression against an entity's stats (bare
    /// references and `link@Stat` sources resolve exactly as they would in a
    /// modifier owned by `entity`).
    pub fn eval(&self, entity: Entity, expression: &Expression) -> Result<f32, StatError> {
        self.eval_with_roles(entity, expression, &HashMap::default())
    }

    /// Evaluates an ad-hoc expression with named role bindings taking
    /// precedence over the entity's links for `name@Stat` references.
    pub fn eval_with_roles(
        &self,
        entity: Entity,
        expression: &Expression,
        roles: &HashMap<String, Entity>,
    ) -> Result<f32, StatError> {
        let compiled = compile(&expression.root, &self.tags)?;
        let env = EvalEnv::new(&self.stats, &self.config);
        let frame = EvalFrame {
            env: &env,
            owner: entity,
            roles: Some(roles),
            query_tags: TagSet::NONE,
        };
        Ok(eval(&compiled, &frame))
    }

    /// The [`TagRegistry`], for resolving tag names to [`TagSet`]s.
    pub fn tag_registry(&self) -> &TagRegistry {
        &self.tags
    }

    // -- writes -------------------------------------------------------------

    /// Sets a stat's replaceable *base* value, creating the stat if needed.
    ///
    /// The base is a single untagged slot that participates in the reduction
    /// like a modifier but is **replaced** (not accumulated) by subsequent
    /// `set` calls — formula-based and tagged contributions on the same stat
    /// are unaffected. Setting the same value again is a no-op that triggers
    /// no recomputation or change detection.
    pub fn set(&mut self, entity: Entity, stat: &str, value: f32) -> Result<(), StatError> {
        let (name, tag_names) = parse_stat_path(stat)?;
        if !tag_names.is_empty() {
            return Err(StatError::InvalidPath {
                path: stat.to_string(),
                reason: "base values are untagged; tag the modifiers instead",
            });
        }
        let mut stats = self
            .stats
            .get_mut(entity)
            .map_err(|_| StatError::NoStats(entity))?;
        let node = stats
            .bypass_change_detection()
            .nodes
            .entry(name.clone())
            .or_default();
        if node.base == Some(value) {
            return Ok(());
        }
        node.base = Some(value);
        self.recompute([(entity, name)], None);
        Ok(())
    }

    /// Adds one modifier to `path` (optionally tag-filtered, e.g.
    /// `"Damage.added{fire}"`). The value may be a number or an expression
    /// string. Returns a handle for later removal.
    pub fn add_modifier(
        &mut self,
        entity: Entity,
        path: &str,
        value: impl IntoModifierValue,
    ) -> Result<ModifierHandle, StatError> {
        let modifier = Modifier {
            value: value.into_modifier_value()?,
            tags: Vec::new(),
        };
        let (stat, stored) = self.compile_modifier(path, &modifier)?;
        let handle = self.install_modifier(entity, &stat, stored)?;
        self.recompute([(entity, stat)], None);
        Ok(handle)
    }

    /// Removes a modifier previously added with
    /// [`add_modifier`](Self::add_modifier).
    pub fn remove_modifier(
        &mut self,
        entity: Entity,
        stat: &str,
        handle: ModifierHandle,
    ) -> Result<(), StatError> {
        self.uninstall_modifier(entity, stat, handle)?;
        self.recompute([(entity, stat.to_string())], None);
        Ok(())
    }

    /// Applies a whole [`ModifierSet`] to an entity, returning a receipt
    /// that [`remove`](Self::remove) uses to detach exactly what was applied.
    ///
    /// Validation (paths, tag names, expressions) happens up front: on error
    /// nothing is applied.
    pub fn apply(
        &mut self,
        entity: Entity,
        set: &ModifierSet,
    ) -> Result<AppliedModifiers, StatError> {
        // Validate and compile everything before touching the graph.
        let mut compiled = Vec::with_capacity(set.entries.len());
        for (path, modifier) in &set.entries {
            compiled.push(self.compile_modifier_parts(path, modifier)?);
        }
        if !self.stats.contains(entity) {
            return Err(StatError::NoStats(entity));
        }
        let mut handles = Vec::with_capacity(compiled.len());
        let mut seeds: Vec<(Entity, String)> = Vec::new();
        for (stat, stored) in compiled {
            let handle = self.install_modifier(entity, &stat, stored)?;
            if !seeds.iter().any(|(_, s)| *s == stat) {
                seeds.push((entity, stat.clone()));
            }
            handles.push((stat, handle));
        }
        self.recompute(seeds, None);
        Ok(AppliedModifiers {
            target: entity,
            handles,
        })
    }

    /// Detaches a previously applied [`ModifierSet`]. Idempotent: modifiers
    /// that no longer exist (already removed, or the target despawned) are
    /// skipped.
    pub fn remove(&mut self, applied: &AppliedModifiers) {
        self.remove_applied_ref(applied);
    }

    pub(crate) fn remove_applied_ref(&mut self, applied: &AppliedModifiers) {
        let entity = applied.target;
        if !self.stats.contains(entity) {
            return; // target is gone; nothing to detach
        }
        let mut seeds: Vec<(Entity, String)> = Vec::new();
        for (stat, handle) in &applied.handles {
            if self.uninstall_modifier(entity, stat, *handle).is_ok()
                && !seeds.iter().any(|(_, s)| s == stat)
            {
                seeds.push((entity, stat.clone()));
            }
        }
        self.recompute(seeds, None);
    }

    /// Removes a stat node entirely: its base, all its modifiers, and its
    /// caches. Dependents remain and now see the stat's reduction identity.
    pub fn remove_stat(&mut self, entity: Entity, stat: &str) -> Result<(), StatError> {
        let Ok(stats) = self.stats.get(entity) else {
            return Err(StatError::NoStats(entity));
        };
        let Some(node) = stats.nodes.get(stat) else {
            return Ok(());
        };
        // Collect outgoing edges to release.
        let exprs: Vec<CExpr> = node
            .modifiers
            .iter()
            .filter_map(|m| match &m.value {
                StoredValue::Expr(e) => Some(e.clone()),
                StoredValue::Literal(_) => None,
            })
            .collect();
        for expr in &exprs {
            self.adjust_expr_edges(entity, stat, expr, -1);
        }
        if let Ok(mut stats) = self.stats.get_mut(entity) {
            stats.bypass_change_detection().nodes.remove(stat);
        }
        self.recompute([(entity, stat.to_string())], None);
        Ok(())
    }

    // -- links --------------------------------------------------------------

    /// Points a named cross-entity link at `target`, rewiring and recomputing
    /// every stat (on this entity and downstream, across any number of hops)
    /// whose modifier expressions read through the link.
    pub fn set_link(
        &mut self,
        entity: Entity,
        link: &str,
        target: Entity,
    ) -> Result<(), StatError> {
        self.relink(entity, link, Some(target))
    }

    /// Removes a named link. Expressions reading through it now see missing
    /// stats (reduction identities) until it is set again.
    pub fn clear_link(&mut self, entity: Entity, link: &str) -> Result<(), StatError> {
        self.relink(entity, link, None)
    }

    fn relink(
        &mut self,
        entity: Entity,
        link: &str,
        target: Option<Entity>,
    ) -> Result<(), StatError> {
        let Ok(stats) = self.stats.get(entity) else {
            return Err(StatError::NoStats(entity));
        };
        let old = stats.link(link);
        if old == target {
            return Ok(());
        }
        // Every (remote stat, local stat) occurrence reading through `link`.
        let mut uses: Vec<(String, String)> = Vec::new();
        for (local, node) in &stats.nodes {
            for modifier in &node.modifiers {
                if let StoredValue::Expr(expr) = &modifier.value {
                    let mut refs = Vec::new();
                    collect_crefs(expr, &mut refs);
                    for r in refs {
                        if r.source.as_deref() == Some(link) {
                            uses.push((r.stat.clone(), local.clone()));
                        }
                    }
                }
            }
        }
        if let Some(old_target) = old {
            self.adjust_edges(old_target, entity, &uses, -1);
        }
        {
            let mut stats = self
                .stats
                .get_mut(entity)
                .map_err(|_| StatError::NoStats(entity))?;
            let inner = stats.bypass_change_detection();
            match target {
                Some(t) => {
                    inner.links.insert(link.to_string(), t);
                }
                None => {
                    inner.links.remove(link);
                }
            }
        }
        if let Some(new_target) = target {
            self.adjust_edges(new_target, entity, &uses, 1);
        }
        let mut seeds: Vec<(Entity, String)> = Vec::new();
        for (_, local) in &uses {
            if !seeds.iter().any(|(_, s)| s == local) {
                seeds.push((entity, local.clone()));
            }
        }
        self.recompute(seeds, None);
        Ok(())
    }

    // -- internals ----------------------------------------------------------

    /// Parses, tag-resolves, and compiles a modifier for `path`.
    fn compile_modifier(
        &self,
        path: &str,
        modifier: &Modifier,
    ) -> Result<(String, StoredModifier), StatError> {
        self.compile_modifier_parts(path, modifier)
    }

    fn compile_modifier_parts(
        &self,
        path: &str,
        modifier: &Modifier,
    ) -> Result<(String, StoredModifier), StatError> {
        let (stat, mut tag_names) = parse_stat_path(path)?;
        tag_names.extend(modifier.tags.iter().cloned());
        let tags = self
            .tags
            .resolve_all(tag_names.iter().map(String::as_str))?;
        let value = match &modifier.value {
            ModifierValue::Literal(v) => StoredValue::Literal(*v),
            ModifierValue::Expression(e) => StoredValue::Expr(compile(&e.root, &self.tags)?),
        };
        Ok((
            stat,
            StoredModifier {
                id: 0, // assigned at install
                tags,
                value,
            },
        ))
    }

    /// Installs a compiled modifier and registers its dependency edges.
    /// Does not recompute.
    fn install_modifier(
        &mut self,
        entity: Entity,
        stat: &str,
        mut stored: StoredModifier,
    ) -> Result<ModifierHandle, StatError> {
        if !self.stats.contains(entity) {
            return Err(StatError::NoStats(entity));
        }
        stored.id = self.ids.next_id();
        let handle = ModifierHandle(stored.id);
        if let StoredValue::Expr(expr) = &stored.value {
            let expr = expr.clone();
            self.adjust_expr_edges(entity, stat, &expr, 1);
        }
        let mut stats = self
            .stats
            .get_mut(entity)
            .map_err(|_| StatError::NoStats(entity))?;
        stats
            .bypass_change_detection()
            .nodes
            .entry(stat.to_string())
            .or_default()
            .modifiers
            .push(stored);
        Ok(handle)
    }

    /// Removes a modifier and releases its dependency edges. Does not
    /// recompute.
    fn uninstall_modifier(
        &mut self,
        entity: Entity,
        stat: &str,
        handle: ModifierHandle,
    ) -> Result<(), StatError> {
        let mut stats = self
            .stats
            .get_mut(entity)
            .map_err(|_| StatError::NoStats(entity))?;
        let inner = stats.bypass_change_detection();
        let node = inner
            .nodes
            .get_mut(stat)
            .ok_or_else(|| StatError::UnknownModifier {
                stat: stat.to_string(),
            })?;
        let index = node
            .modifiers
            .iter()
            .position(|m| m.id == handle.0)
            .ok_or_else(|| StatError::UnknownModifier {
                stat: stat.to_string(),
            })?;
        let removed = node.modifiers.remove(index);
        if let StoredValue::Expr(expr) = &removed.value {
            self.adjust_expr_edges(entity, stat, expr, -1);
        }
        Ok(())
    }

    /// Adjusts the reference counts of every dependency edge created by one
    /// expression owned by (`owner`, `dependent_stat`).
    fn adjust_expr_edges(&mut self, owner: Entity, dependent_stat: &str, expr: &CExpr, delta: i32) {
        let mut refs = Vec::new();
        collect_crefs(expr, &mut refs);
        let uses: Vec<(Entity, String)> = refs
            .iter()
            .filter_map(|r| {
                let target = match &r.source {
                    None => owner,
                    Some(link) => self
                        .stats
                        .get(owner)
                        .ok()
                        .and_then(|s| s.link(link))?,
                };
                Some((target, r.stat.clone()))
            })
            .collect();
        for (target, remote_stat) in uses {
            self.adjust_edge(target, remote_stat, owner, dependent_stat.to_string(), delta);
        }
    }

    /// Adjusts edges for a list of (remote stat, local stat) uses against one
    /// remote entity.
    fn adjust_edges(&mut self, remote: Entity, owner: Entity, uses: &[(String, String)], delta: i32) {
        for (remote_stat, local_stat) in uses {
            self.adjust_edge(remote, remote_stat.clone(), owner, local_stat.clone(), delta);
        }
    }

    fn adjust_edge(
        &mut self,
        source_entity: Entity,
        source_stat: String,
        dependent_entity: Entity,
        dependent_stat: String,
        delta: i32,
    ) {
        self.graph.adjust(
            source_entity,
            source_stat,
            Dependent {
                entity: dependent_entity,
                stat: dependent_stat,
            },
            delta,
        );
    }

    /// The number of distinct stats currently depending on `stat` of
    /// `entity`. See [`StatGraph::dependent_count`].
    pub fn dependent_count(&self, entity: Entity, stat: &str) -> usize {
        self.graph.dependent_count(entity, stat)
    }

    /// The entities whose resolved values changed since the last sync sweep
    /// (see [`DirtyStats`]).
    pub(crate) fn dirty_entities(&self) -> Vec<Entity> {
        self.dirty.0.iter().copied().collect()
    }

    /// Recomputes the transitive dependents of `seeds`, once each, in
    /// dependency order, refreshing every cached tag query and marking
    /// components changed only where a value actually moved.
    pub(crate) fn recompute(
        &mut self,
        seeds: impl IntoIterator<Item = (Entity, String)>,
        excluded: Option<Entity>,
    ) {
        // Phase 1: dirty closure over the dependents graph (BFS).
        let mut order: Vec<(Entity, String)> = Vec::new();
        let mut seen: HashSet<(Entity, String)> = HashSet::default();
        for seed in seeds {
            if Some(seed.0) != excluded && seen.insert(seed.clone()) {
                order.push(seed);
            }
        }
        let mut i = 0;
        while i < order.len() {
            let (entity, stat) = order[i].clone();
            i += 1;
            let mut new_deps = Vec::new();
            for dep in self.graph.dependents_of(entity, &stat) {
                if Some(dep.entity) == excluded {
                    continue;
                }
                let key = (dep.entity, dep.stat.clone());
                if seen.insert(key.clone()) {
                    new_deps.push(key);
                }
            }
            order.append(&mut new_deps);
        }
        if order.is_empty() {
            return;
        }

        // Phase 2: clear the caches of every dirty node, remembering which
        // tag queries were live so we can refresh exactly those.
        let mut live_keys: HashMap<(Entity, String), Vec<TagSet>> = HashMap::default();
        for (entity, stat) in &order {
            if let Ok(mut stats) = self.stats.get_mut(*entity)
                && let Some(node) = stats.bypass_change_detection().nodes.get_mut(stat)
            {
                let keys: Vec<TagSet> = node.cache.keys().copied().collect();
                node.cache.clear();
                if !keys.is_empty() {
                    live_keys.insert((*entity, stat.clone()), keys);
                }
            }
        }

        // Phase 3: topological order (Kahn) over the dirty subgraph, so each
        // stat is recomputed exactly once, after everything it reads.
        let index_of: HashMap<(Entity, String), usize> = order
            .iter()
            .enumerate()
            .map(|(i, k)| (k.clone(), i))
            .collect();
        let mut indegree = vec![0usize; order.len()];
        let mut edges: Vec<Vec<usize>> = vec![Vec::new(); order.len()];
        for (i, (entity, stat)) in order.iter().enumerate() {
            for dep in self.graph.dependents_of(*entity, stat) {
                if let Some(&j) = index_of.get(&(dep.entity, dep.stat.clone())) {
                    edges[i].push(j);
                    indegree[j] += 1;
                }
            }
        }
        let mut queue: Vec<usize> = (0..order.len()).filter(|&i| indegree[i] == 0).collect();
        let mut topo: Vec<usize> = Vec::with_capacity(order.len());
        let mut head = 0;
        while head < queue.len() {
            let i = queue[head];
            head += 1;
            topo.push(i);
            for &j in &edges[i] {
                indegree[j] -= 1;
                if indegree[j] == 0 {
                    queue.push(j);
                }
            }
        }
        if topo.len() < order.len() {
            // Cycle: append the rest in discovery order; the evaluation-time
            // cycle guard breaks the loop deterministically.
            for (i, &degree) in indegree.iter().enumerate() {
                if degree > 0 {
                    topo.push(i);
                }
            }
        }

        // Phase 4: re-evaluate each live tag query and write it back,
        // tracking real changes.
        for &i in &topo {
            let (entity, stat) = &order[i];
            let Some(keys) = live_keys.get(&(*entity, stat.clone())) else {
                continue;
            };
            let mut computed = Vec::with_capacity(keys.len());
            {
                let mut env = EvalEnv::new(&self.stats, &self.config);
                env.excluded = excluded;
                for &tags in keys {
                    computed.push(eval_stat(&env, *entity, stat, tags));
                }
            }
            let Ok(mut stats) = self.stats.get_mut(*entity) else {
                continue;
            };
            let inner = stats.bypass_change_detection();
            let Some(node) = inner.nodes.get_mut(stat) else {
                continue;
            };
            let mut changed = false;
            for (&tags, &value) in keys.iter().zip(computed.iter()) {
                if node.cache.insert(tags, value) != Some(value) {
                    changed = true;
                }
            }
            if changed {
                stats.set_changed();
                self.dirty.0.insert(*entity);
                self.commands.trigger(StatChanged {
                    entity: *entity,
                    stat: stat.clone(),
                });
            }
        }
    }

    /// Drains the pending starting-stat entries queued on a freshly inserted
    /// [`Stats`] component into the graph.
    pub(crate) fn drain_pending(&mut self, entity: Entity) {
        let Ok(mut stats) = self.stats.get_mut(entity) else {
            return;
        };
        let pending = core::mem::take(&mut stats.bypass_change_detection().pending);
        if pending.is_empty() {
            return;
        }
        let mut seeds: Vec<(Entity, String)> = Vec::new();
        for entry in pending {
            let result = match entry {
                PendingEntry::Base(path, value) => {
                    match self.stats.get_mut(entity) {
                        Ok(mut stats) => {
                            let node = stats
                                .bypass_change_detection()
                                .nodes
                                .entry(path.clone())
                                .or_default();
                            node.base = Some(value);
                            if !seeds.iter().any(|(_, s)| *s == path) {
                                seeds.push((entity, path));
                            }
                            Ok(())
                        }
                        Err(_) => Ok(()),
                    }
                }
                PendingEntry::Modifier(path, modifier) => {
                    match self.compile_modifier(&path, &modifier) {
                        Ok((stat, stored)) => match self.install_modifier(entity, &stat, stored) {
                            Ok(_) => {
                                if !seeds.iter().any(|(_, s)| *s == stat) {
                                    seeds.push((entity, stat));
                                }
                                Ok(())
                            }
                            Err(e) => Err(e),
                        },
                        Err(e) => Err(e),
                    }
                }
            };
            if let Err(e) = result {
                log::error!("failed to install starting stat on {entity}: {e}");
            }
        }
        self.recompute(seeds, None);
    }

    /// Tears down all graph edges of a `Stats` component that is being
    /// removed (or whose entity is despawning), and recomputes downstream.
    pub(crate) fn teardown(&mut self, entity: Entity) {
        let Ok(stats) = self.stats.get(entity) else {
            return;
        };
        // Outgoing edges: every expression we own.
        let mut owned_exprs: Vec<(String, CExpr)> = Vec::new();
        for (stat, node) in &stats.nodes {
            for modifier in &node.modifiers {
                if let StoredValue::Expr(expr) = &modifier.value {
                    owned_exprs.push((stat.clone(), expr.clone()));
                }
            }
        }
        // Incoming edges: everyone who depends on our stats. The edges
        // themselves are owned by the dependents' modifiers and stay in the
        // graph (they are released when those modifiers go away), but every
        // dependent must recompute against the source being gone.
        let mut seeds: Vec<(Entity, String)> = Vec::new();
        if let Some(by_stat) = self.graph.edges.get(&entity) {
            for deps in by_stat.values() {
                for dep in deps.keys() {
                    if dep.entity == entity {
                        continue;
                    }
                    let key = (dep.entity, dep.stat.clone());
                    if !seeds.contains(&key) {
                        seeds.push(key);
                    }
                }
            }
        }
        for (stat, expr) in &owned_exprs {
            self.adjust_expr_edges(entity, stat, expr, -1);
        }
        self.recompute(seeds, Some(entity));
    }
}

// ---------------------------------------------------------------------------
// StatsReader
// ---------------------------------------------------------------------------

/// Read-only access to resolved stat values.
///
/// Reads hit the cache when the value has been computed before; otherwise the
/// value is computed on the fly (without being cached, since this parameter
/// holds no write access). Use [`StatsMutator`] where warm caches matter.
#[derive(SystemParam)]
pub struct StatsReader<'w, 's> {
    stats: Query<'w, 's, &'static Stats>,
    tags: Res<'w, TagRegistry>,
    config: Res<'w, StatConfig>,
}

impl StatsReader<'_, '_> {
    /// Resolves a stat's value with no tag filter.
    pub fn get(&self, entity: Entity, stat: &str) -> f32 {
        self.get_filtered(entity, stat, TagSet::NONE)
    }

    /// Resolves a stat's value under a tag query (subset rule).
    pub fn get_filtered(&self, entity: Entity, stat: &str, tags: TagSet) -> f32 {
        let env = EvalEnv::new(&self.stats, &self.config);
        eval_stat(&env, entity, stat, tags)
    }

    /// Resolves a stat under a tag query given as a name list (`"fire, sword"`).
    pub fn get_with_tags(
        &self,
        entity: Entity,
        stat: &str,
        tags: &str,
    ) -> Result<f32, StatError> {
        let tags = self.tags.parse(tags)?;
        Ok(self.get_filtered(entity, stat, tags))
    }

    /// Evaluates an ad-hoc expression against an entity's stats.
    pub fn eval(&self, entity: Entity, expression: &Expression) -> Result<f32, StatError> {
        let compiled = compile(&expression.root, &self.tags)?;
        let env = EvalEnv::new(&self.stats, &self.config);
        let frame = EvalFrame {
            env: &env,
            owner: entity,
            roles: None,
            query_tags: TagSet::NONE,
        };
        Ok(eval(&compiled, &frame))
    }

    /// The [`TagRegistry`], for resolving tag names to [`TagSet`]s.
    pub fn tag_registry(&self) -> &TagRegistry {
        &self.tags
    }
}
