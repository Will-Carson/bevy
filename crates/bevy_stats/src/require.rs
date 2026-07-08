//! Boolean requirements: expression-based gates over an entity's stats.
//!
//! Use them as equipment prerequisites, ability conditions, or state-machine
//! edge guards:
//!
//! ```
//! # use bevy_stats::Requirement;
//! let can_wield = Requirement::parse("Strength >= 20 && Intelligence >= 15").unwrap();
//! ```
//!
//! Check with [`StatsMutator::check`](crate::StatsMutator::check) or
//! [`StatsReader::check`](crate::StatsReader::check). Comparison and logical
//! operators evaluate to `1`/`0`; a requirement holds when the expression is
//! non-zero.

use crate::access::{StatsMutator, StatsReader};
use crate::effect::Roles;
use crate::error::StatError;
use crate::expr::Expression;
use bevy_ecs::entity::Entity;

/// A parsed boolean gate over stats. See the [module docs](self).
#[derive(Clone, Debug, PartialEq)]
pub struct Requirement {
    pub(crate) expr: Expression,
}

impl Requirement {
    /// Parses a requirement expression, e.g. `"Intelligence >= 15"`.
    pub fn parse(src: &str) -> Result<Requirement, StatError> {
        Ok(Requirement {
            expr: Expression::parse(src)?,
        })
    }

    /// The underlying expression.
    pub fn expression(&self) -> &Expression {
        &self.expr
    }
}

impl From<Expression> for Requirement {
    fn from(expr: Expression) -> Self {
        Requirement { expr }
    }
}

impl StatsMutator<'_, '_> {
    /// Evaluates a requirement against an entity's current stats.
    pub fn check(&self, entity: Entity, requirement: &Requirement) -> Result<bool, StatError> {
        Ok(self.eval(entity, &requirement.expr)? != 0.0)
    }

    /// Evaluates a requirement with role bindings available as `role@Stat`.
    pub fn check_with_roles(
        &self,
        entity: Entity,
        requirement: &Requirement,
        roles: &Roles,
    ) -> Result<bool, StatError> {
        Ok(self.eval_with_roles(entity, &requirement.expr, &roles.map)? != 0.0)
    }
}

impl StatsReader<'_, '_> {
    /// Evaluates a requirement against an entity's current stats.
    pub fn check(&self, entity: Entity, requirement: &Requirement) -> Result<bool, StatError> {
        Ok(self.eval(entity, &requirement.expr)? != 0.0)
    }
}
