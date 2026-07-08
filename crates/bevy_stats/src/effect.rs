//! One-shot ("instant") effects: apply a set/add/subtract to a target once,
//! without leaving a lingering modifier.
//!
//! An [`InstantEffect`] is a list of operations, each addressed to a named
//! *role* (attacker, weapon, target, …). At application time a [`Roles`] map
//! binds roles to entities for that single application. Operation
//! expressions may read stats from any participant via `role@Stat`.
//!
//! All operations in one effect are evaluated against the state *before* the
//! effect, then written together (so two operations never observe each
//! other's writes). Sequential phases — armor before resistance before life —
//! are expressed as separate effects applied in order.
//!
//! [`StatsMutator::preview_effect`] computes the exact outcomes without
//! committing them, for damage previews and UI.

use crate::access::{EvalEnv, EvalFrame, StatsMutator};
use crate::error::StatError;
use crate::expr::{compile, eval, parse_stat_path};
use crate::modifier::{IntoModifierValue, ModifierValue};
use bevy_ecs::change_detection::DetectChangesMut;
use bevy_ecs::entity::Entity;
use bevy_platform::collections::HashMap;

/// How an instant operation writes the target stat's base value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstantOp {
    /// Replace the base value.
    Set,
    /// Add to the base value.
    Add,
    /// Subtract from the base value.
    Sub,
}

#[derive(Clone, Debug)]
struct EffectStep {
    role: String,
    stat: String,
    op: InstantOp,
    value: ModifierValue,
}

/// A reusable one-shot effect: an ordered list of set/add/subtract
/// operations over role-addressed stats. See the [module docs](self).
#[derive(Clone, Debug, Default)]
pub struct InstantEffect {
    steps: Vec<EffectStep>,
}

impl InstantEffect {
    /// An empty effect.
    pub fn new() -> InstantEffect {
        InstantEffect::default()
    }

    /// Adds a *set* operation: `role`'s `stat` base becomes the value.
    ///
    /// # Panics
    /// Panics on syntax errors in `stat` or an expression `value`; use
    /// [`try_step`](Self::try_step) to handle them.
    #[must_use]
    pub fn set(self, role: &str, stat: &str, value: impl IntoModifierValue) -> InstantEffect {
        self.step(role, stat, InstantOp::Set, value)
    }

    /// Adds an *add* operation: the value is added to `role`'s `stat` base.
    #[must_use]
    pub fn add(self, role: &str, stat: &str, value: impl IntoModifierValue) -> InstantEffect {
        self.step(role, stat, InstantOp::Add, value)
    }

    /// Adds a *subtract* operation: the value is subtracted from `role`'s
    /// `stat` base.
    #[must_use]
    pub fn sub(self, role: &str, stat: &str, value: impl IntoModifierValue) -> InstantEffect {
        self.step(role, stat, InstantOp::Sub, value)
    }

    /// Panicking builder core; see [`try_step`](Self::try_step).
    #[must_use]
    pub fn step(
        self,
        role: &str,
        stat: &str,
        op: InstantOp,
        value: impl IntoModifierValue,
    ) -> InstantEffect {
        match self.try_step(role, stat, op, value) {
            Ok(effect) => effect,
            Err(e) => panic!("invalid instant effect step `{stat}`: {e}"),
        }
    }

    /// Fallible builder: adds one operation.
    pub fn try_step(
        mut self,
        role: &str,
        stat: &str,
        op: InstantOp,
        value: impl IntoModifierValue,
    ) -> Result<InstantEffect, StatError> {
        let (stat, tags) = parse_stat_path(stat)?;
        if !tags.is_empty() {
            return Err(StatError::InvalidPath {
                path: stat,
                reason: "instant effects write base values, which are untagged",
            });
        }
        self.steps.push(EffectStep {
            role: role.to_string(),
            stat,
            op,
            value: value.into_modifier_value()?,
        });
        Ok(self)
    }
}

/// Binds role names to entities for one application of an [`InstantEffect`].
#[derive(Clone, Debug, Default)]
pub struct Roles {
    pub(crate) map: HashMap<String, Entity>,
}

impl Roles {
    /// An empty role map.
    pub fn new() -> Roles {
        Roles::default()
    }

    /// Binds `role` to `entity`.
    #[must_use]
    pub fn with(mut self, role: &str, entity: Entity) -> Roles {
        self.map.insert(role.to_string(), entity);
        self
    }

    /// The entity bound to `role`, if any.
    pub fn get(&self, role: &str) -> Option<Entity> {
        self.map.get(role).copied()
    }
}

/// The computed result of one instant operation, as returned by
/// [`StatsMutator::preview_effect`] and [`StatsMutator::apply_effect`].
#[derive(Clone, Debug, PartialEq)]
pub struct InstantOutcome {
    /// The entity written to.
    pub entity: Entity,
    /// The stat written to.
    pub stat: String,
    /// The operation kind.
    pub op: InstantOp,
    /// The evaluated operand.
    pub amount: f32,
    /// The base value the stat will have after the write.
    pub new_base: f32,
}

impl StatsMutator<'_, '_> {
    /// Computes what [`apply_effect`](Self::apply_effect) would do, without
    /// committing anything.
    pub fn preview_effect(
        &mut self,
        effect: &InstantEffect,
        roles: &Roles,
    ) -> Result<Vec<InstantOutcome>, StatError> {
        self.resolve_effect(effect, roles)
    }

    /// Applies an [`InstantEffect`]: evaluates every operation against the
    /// pre-effect state, then writes all base values and recomputes
    /// downstream once. Returns the outcomes actually applied.
    pub fn apply_effect(
        &mut self,
        effect: &InstantEffect,
        roles: &Roles,
    ) -> Result<Vec<InstantOutcome>, StatError> {
        let outcomes = self.resolve_effect(effect, roles)?;
        let mut seeds: Vec<(Entity, String)> = Vec::new();
        for outcome in &outcomes {
            let Ok(mut stats) = self.stats.get_mut(outcome.entity) else {
                continue;
            };
            let node = stats
                .bypass_change_detection()
                .nodes
                .entry(outcome.stat.clone())
                .or_default();
            if node.base == Some(outcome.new_base) {
                continue;
            }
            node.base = Some(outcome.new_base);
            let key = (outcome.entity, outcome.stat.clone());
            if !seeds.contains(&key) {
                seeds.push(key);
            }
        }
        self.recompute(seeds, None);
        Ok(outcomes)
    }

    fn resolve_effect(
        &mut self,
        effect: &InstantEffect,
        roles: &Roles,
    ) -> Result<Vec<InstantOutcome>, StatError> {
        // Later steps must not observe earlier steps' writes, but they *can*
        // stack on the same base slot (two `add`s accumulate), so track the
        // pending base per (entity, stat).
        let mut pending: HashMap<(Entity, String), f32> = HashMap::default();
        let mut outcomes = Vec::with_capacity(effect.steps.len());
        for step in &effect.steps {
            let target = roles
                .get(&step.role)
                .ok_or_else(|| StatError::UnknownRole(step.role.clone()))?;
            let amount = match &step.value {
                ModifierValue::Literal(v) => *v,
                ModifierValue::Expression(e) => {
                    let compiled = compile(&e.root, &self.tags)?;
                    let env = EvalEnv::new(&self.stats, &self.config);
                    let frame = EvalFrame {
                        env: &env,
                        owner: target,
                        roles: Some(&roles.map),
                        query_tags: crate::tags::TagSet::NONE,
                    };
                    eval(&compiled, &frame)
                }
            };
            let key = (target, step.stat.clone());
            let current = pending.get(&key).copied().or_else(|| {
                self.stats
                    .get(target)
                    .ok()
                    .and_then(|s| s.nodes.get(&step.stat))
                    .and_then(|n| n.base)
            });
            let new_base = match step.op {
                InstantOp::Set => amount,
                InstantOp::Add => current.unwrap_or(0.0) + amount,
                InstantOp::Sub => current.unwrap_or(0.0) - amount,
            };
            pending.insert(key, new_base);
            outcomes.push(InstantOutcome {
                entity: target,
                stat: step.stat.clone(),
                op: step.op,
                amount,
                new_base,
            });
        }
        Ok(outcomes)
    }
}
