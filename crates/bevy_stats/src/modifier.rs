//! Modifiers and detachable modifier bundles.

use crate::error::StatError;
use crate::expr::{Expression, parse_stat_path};
use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;

/// The value a modifier contributes: a flat constant or an expression over
/// other stats.
#[derive(Clone, Debug, PartialEq)]
pub enum ModifierValue {
    /// A flat constant.
    Literal(f32),
    /// An expression referencing other stats (see [`crate::expr`]).
    Expression(Expression),
}

impl ModifierValue {
    /// Parses a value from an expression source string.
    pub fn parse(src: &str) -> Result<ModifierValue, StatError> {
        Ok(ModifierValue::Expression(Expression::parse(src)?))
    }
}

impl From<f32> for ModifierValue {
    fn from(v: f32) -> Self {
        ModifierValue::Literal(v)
    }
}

impl From<Expression> for ModifierValue {
    fn from(e: Expression) -> Self {
        ModifierValue::Expression(e)
    }
}

/// Anything that can be turned into a [`ModifierValue`]: numbers, expression
/// strings, or already-built values.
///
/// String conversions parse the expression and surface syntax errors from the
/// calling API.
pub trait IntoModifierValue {
    /// Performs the conversion.
    fn into_modifier_value(self) -> Result<ModifierValue, StatError>;
}

impl IntoModifierValue for ModifierValue {
    fn into_modifier_value(self) -> Result<ModifierValue, StatError> {
        Ok(self)
    }
}

impl IntoModifierValue for Expression {
    fn into_modifier_value(self) -> Result<ModifierValue, StatError> {
        Ok(ModifierValue::Expression(self))
    }
}

impl IntoModifierValue for f32 {
    fn into_modifier_value(self) -> Result<ModifierValue, StatError> {
        Ok(ModifierValue::Literal(self))
    }
}

impl IntoModifierValue for f64 {
    fn into_modifier_value(self) -> Result<ModifierValue, StatError> {
        Ok(ModifierValue::Literal(self as f32))
    }
}

impl IntoModifierValue for i32 {
    fn into_modifier_value(self) -> Result<ModifierValue, StatError> {
        Ok(ModifierValue::Literal(self as f32))
    }
}

impl IntoModifierValue for u32 {
    fn into_modifier_value(self) -> Result<ModifierValue, StatError> {
        Ok(ModifierValue::Literal(self as f32))
    }
}

impl IntoModifierValue for &str {
    fn into_modifier_value(self) -> Result<ModifierValue, StatError> {
        ModifierValue::parse(self)
    }
}

impl IntoModifierValue for String {
    fn into_modifier_value(self) -> Result<ModifierValue, StatError> {
        ModifierValue::parse(&self)
    }
}

/// A single modifier: a value plus the tag names it is inserted under.
///
/// Tag names are resolved against the [`TagRegistry`](crate::TagRegistry)
/// when the modifier is applied to an entity, so a `Modifier` can be built
/// anywhere without resource access.
#[derive(Clone, Debug, PartialEq)]
pub struct Modifier {
    /// The contributed value.
    pub value: ModifierValue,
    /// Tag names this modifier is inserted under. Empty means untagged
    /// (applies to every query).
    pub tags: Vec<String>,
}

impl Modifier {
    /// A flat constant modifier.
    pub fn literal(v: f32) -> Modifier {
        Modifier {
            value: ModifierValue::Literal(v),
            tags: Vec::new(),
        }
    }

    /// An expression modifier. Fails on syntax errors.
    pub fn expr(src: &str) -> Result<Modifier, StatError> {
        Ok(Modifier {
            value: ModifierValue::parse(src)?,
            tags: Vec::new(),
        })
    }

    /// Adds a tag name to this modifier.
    #[must_use]
    pub fn with_tag(mut self, tag: impl Into<String>) -> Modifier {
        self.tags.push(tag.into());
        self
    }

    /// Adds several tag names to this modifier.
    #[must_use]
    pub fn with_tags<T: Into<String>>(mut self, tags: impl IntoIterator<Item = T>) -> Modifier {
        self.tags.extend(tags.into_iter().map(Into::into));
        self
    }
}

/// A portable, detachable bundle of modifiers — the building block for
/// equipment, buffs, enchants, auras, and starting-stat templates.
///
/// A `ModifierSet` is plain data: build it anywhere (a const-like fn, an
/// asset loader, the [`modifiers!`](crate::modifiers) macro), then apply it
/// to an entity with [`StatsMutator::apply`](crate::StatsMutator::apply),
/// which returns an [`AppliedModifiers`] receipt that removes exactly what
/// was applied. Or attach it via the [`ModifierCollection`] component and the
/// [`AttachedTo`] relationship for fully ECS-driven equip/unequip.
///
/// Entry paths may carry tag filters: `"Damage.added{fire}"`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModifierSet {
    pub(crate) entries: Vec<(String, Modifier)>,
}

impl ModifierSet {
    /// An empty set.
    pub fn new() -> ModifierSet {
        ModifierSet::default()
    }

    /// Adds a modifier for `path` (optionally tag-filtered, e.g.
    /// `"Damage.added{fire}"`).
    ///
    /// # Panics
    /// Panics on a syntax error in `path` or an expression `value`, with the
    /// parse error message. Use [`try_with`](Self::try_with) to handle
    /// errors instead.
    #[must_use]
    pub fn with(self, path: &str, value: impl IntoModifierValue) -> ModifierSet {
        match self.try_with(path, value) {
            Ok(set) => set,
            Err(e) => panic!("invalid modifier `{path}`: {e}"),
        }
    }

    /// Fallible version of [`with`](Self::with).
    pub fn try_with(
        mut self,
        path: &str,
        value: impl IntoModifierValue,
    ) -> Result<ModifierSet, StatError> {
        self.try_add(path, value)?;
        Ok(self)
    }

    /// Adds a modifier in place. See [`with`](Self::with).
    pub fn try_add(
        &mut self,
        path: &str,
        value: impl IntoModifierValue,
    ) -> Result<(), StatError> {
        let (stat, tags) = parse_stat_path(path)?;
        self.entries.push((
            stat,
            Modifier {
                value: value.into_modifier_value()?,
                tags,
            },
        ));
        Ok(())
    }

    /// Adds an explicit [`Modifier`] under `path` (tags in the path and on
    /// the modifier are unioned).
    pub fn add_modifier(&mut self, path: &str, modifier: Modifier) -> Result<(), StatError> {
        let (stat, mut tags) = parse_stat_path(path)?;
        let mut modifier = modifier;
        tags.append(&mut modifier.tags);
        modifier.tags = tags;
        self.entries.push((stat, modifier));
        Ok(())
    }

    /// The `(stat, modifier)` entries in the set.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &Modifier)> {
        self.entries.iter().map(|(s, m)| (s.as_str(), m))
    }

    /// Whether the set contains no modifiers.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The number of modifiers in the set.
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Builds a [`ModifierSet`] from `path => value` pairs.
///
/// ```
/// # use bevy_stats::modifiers;
/// let sword = modifiers! {
///     "Damage.added{physical, sword}" => 12.0,
///     "Damage.increased" => 0.15,
///     "Accuracy" => "wielder@Dexterity * 2",
/// };
/// ```
///
/// Values may be numbers or expression strings. Panics at construction on
/// syntax errors (build with [`ModifierSet::try_with`] to handle them).
#[macro_export]
macro_rules! modifiers {
    ( $( $path:expr => $value:expr ),* $(,)? ) => {
        $crate::ModifierSet::new() $( .with($path, $value) )*
    };
}

/// A unique handle to one applied modifier, used to remove it later.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModifierHandle(pub(crate) u64);

/// A receipt for an applied [`ModifierSet`]: removing it detaches exactly the
/// modifiers that were applied, leaving everything else intact.
#[derive(Debug)]
pub struct AppliedModifiers {
    pub(crate) target: Entity,
    pub(crate) handles: Vec<(String, ModifierHandle)>,
}

impl AppliedModifiers {
    /// The entity the set was applied to.
    pub fn target(&self) -> Entity {
        self.target
    }

    /// Whether nothing was applied.
    pub fn is_empty(&self) -> bool {
        self.handles.is_empty()
    }
}

/// A [`ModifierSet`] carried by an entity (a sword, a buff, an enchant), to
/// be granted to whatever entity it is [`AttachedTo`].
///
/// Insert `ModifierCollection` on the item entity, then insert
/// [`AttachedTo(target)`](AttachedTo) to apply it. Removing `AttachedTo` (or
/// despawning the item) detaches it symmetrically; re-inserting `AttachedTo`
/// pointing at someone else moves the whole bundle in one operation.
#[derive(Component, Clone, Debug, Default)]
pub struct ModifierCollection(pub ModifierSet);

/// Relationship: this entity's [`ModifierCollection`] applies to the target
/// entity while present.
#[derive(Component, Debug)]
#[relationship(relationship_target = AttachedItems)]
pub struct AttachedTo(pub Entity);

/// Relationship target: the item entities currently attached to this entity.
#[derive(Component, Debug, Default)]
#[relationship_target(relationship = AttachedTo)]
pub struct AttachedItems(Vec<Entity>);

impl AttachedItems {
    /// The attached item entities.
    pub fn iter(&self) -> impl Iterator<Item = Entity> + '_ {
        self.0.iter().copied()
    }
}

/// Internal receipt component recording what an attached item currently
/// grants, so detaching can remove exactly that.
#[derive(Component, Debug)]
pub(crate) struct AppliedCollection(pub(crate) AppliedModifiers);
