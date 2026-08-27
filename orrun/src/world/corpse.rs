//! Stable-identity corpse loot, selection, and marker lifecycle.
use super::combat_layer::{disable_shadow, sparkle_mesh, SPARKLE_LIFT_M};
use crate::combat::ActorId;
use crate::loot::{GroundPile, LootSite};
use engine::world::{EntityId, World};
use engine::{GlobalPlace, GlobalPosition};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

#[derive(Clone, Debug)]
pub struct DeadActorRecord {
    actor_id: ActorId,
    mob_id: String,
    death_position: GlobalPosition,
    presentation_entity: EntityId,
    loot_site: Option<LootSite>,
}
impl DeadActorRecord {
    pub fn new(
        actor_id: ActorId,
        mob_id: String,
        death_position: GlobalPosition,
        presentation_entity: EntityId,
        loot_site: Option<LootSite>,
    ) -> Self {
        assert_ne!(actor_id, ActorId::PLAYER, "player cannot be corpse loot");
        assert!(!mob_id.is_empty(), "corpse mob id must not be empty");
        assert!(
            death_position.x.is_finite()
                && death_position.y.is_finite()
                && death_position.z.is_finite(),
            "corpse position must be finite"
        );
        Self {
            actor_id,
            mob_id,
            death_position,
            presentation_entity,
            loot_site,
        }
    }
}

#[derive(Debug, Error)]
pub enum CorpseError {
    #[error("corpse {0:?} has no lootable pile")]
    InvalidSelection(ActorId),
    #[error(transparent)]
    Engine(#[from] engine::EngineError),
}

#[derive(Default)]
pub struct CorpseLifecycle {
    piles: BTreeMap<ActorId, GroundPile>,
    looted: BTreeSet<ActorId>,
    selected: Option<ActorId>,
    markers: BTreeMap<ActorId, EntityId>,
}
impl CorpseLifecycle {
    pub fn pile(&self) -> Option<&GroundPile> {
        self.selected.and_then(|id| self.piles.get(&id))
    }
    pub fn count(&self) -> usize {
        self.piles.len()
    }
    pub fn contains(&self, id: ActorId) -> bool {
        self.piles.contains_key(&id)
    }
    pub fn first_actor_id(&self) -> Option<ActorId> {
        self.piles.keys().next().copied()
    }
    pub fn selected(&self) -> Option<ActorId> {
        self.selected
    }
    pub fn is_open(&self) -> bool {
        self.selected.is_some()
    }
    pub fn open(&mut self, id: ActorId) -> Result<(), CorpseError> {
        self.select(id)
    }
    pub fn select(&mut self, id: ActorId) -> Result<(), CorpseError> {
        if !self.piles.contains_key(&id) || self.looted.contains(&id) {
            return Err(CorpseError::InvalidSelection(id));
        }
        self.selected = Some(id);
        Ok(())
    }
    pub fn close(&mut self) {
        self.selected = None;
    }
    pub fn is_looted(&self, id: ActorId) -> bool {
        self.looted.contains(&id)
    }
    pub fn has_marker(&self, id: ActorId) -> bool {
        self.markers.contains_key(&id)
    }
    pub fn reconcile(
        &mut self,
        world: &mut World,
        record: DeadActorRecord,
    ) -> Result<(), CorpseError> {
        let id = record.actor_id;
        if self.looted.contains(&id) {
            return Ok(());
        }
        if self.piles.contains_key(&id) {
            assert!(
                self.markers.contains_key(&id),
                "corpse pile exists without marker"
            );
            return Ok(());
        }
        assert!(
            !self.markers.contains_key(&id),
            "corpse marker exists without pile"
        );
        world
            .animated_entity(record.presentation_entity)
            .map_err(|error| {
                engine::EngineError::Model(format!(
                    "corpse presentation {} for actor {id:?} is not live: {error}",
                    record.presentation_entity
                ))
            })?;
        let marker = world.spawn_anchored(
            sparkle_mesh()?,
            GlobalPlace::at(GlobalPosition::at(
                record.death_position.x,
                record.death_position.y + SPARKLE_LIFT_M,
                record.death_position.z,
            )),
        )?;
        if let Err(error) = disable_shadow(world, marker, &format!("loot marker for actor {id:?}"))
        {
            world.despawn(marker);
            return Err(error.into());
        }
        let pile = crate::loot::roll_pile(&record.mob_id, id, record.loot_site);
        assert!(
            self.markers.insert(id, marker).is_none(),
            "duplicate corpse marker"
        );
        self.insert_reconciled_pile(pile);
        Ok(())
    }
    fn insert_reconciled_pile(&mut self, pile: GroundPile) {
        let id = pile.actor_id;
        assert!(
            self.markers.contains_key(&id),
            "initial corpse pile requires committed marker"
        );
        assert!(
            !self.looted.contains(&id),
            "cannot add loot to looted corpse"
        );
        assert!(
            self.piles.insert(id, pile).is_none(),
            "duplicate corpse loot pile"
        );
    }
    pub fn override_reconciled_pile(&mut self, pile: GroundPile) {
        let id = pile.actor_id;
        assert!(
            self.markers.contains_key(&id),
            "corpse pile override requires marker"
        );
        assert!(
            self.piles.insert(id, pile).is_some(),
            "replacing missing corpse pile"
        );
    }
    pub fn finish_loot(&mut self, world: &mut World, id: ActorId) {
        assert!(
            self.piles.remove(&id).is_some(),
            "finishing missing corpse loot"
        );
        assert!(self.looted.insert(id), "finishing already looted corpse");
        self.strip_marker(world, id);
        if self.selected == Some(id) {
            self.selected = None;
        }
    }
    fn strip_marker(&mut self, world: &mut World, id: ActorId) {
        let marker = self
            .markers
            .remove(&id)
            .unwrap_or_else(|| panic!("corpse {id:?} has no marker"));
        world
            .entity(marker)
            .unwrap_or_else(|error| panic!("corpse {id:?} marker {marker} is not live: {error}"));
        world.despawn(marker);
    }
    pub fn marker_visible(&self, world: &World) -> bool {
        self.markers.values().any(|id| world.entity(*id).is_ok())
    }
    pub fn clear(&mut self, world: &mut World) {
        for (actor_id, marker) in &self.markers {
            world.entity(*marker).unwrap_or_else(|error| {
                panic!("cannot clear corpse {actor_id:?}: marker {marker} is not live: {error}")
            });
            assert!(
                self.piles.contains_key(actor_id),
                "corpse marker exists without pile during clear"
            );
        }
        assert_eq!(
            self.markers.len(),
            self.piles.len(),
            "corpse pile/marker cardinality differs during clear"
        );
        for marker in std::mem::take(&mut self.markers).into_values() {
            world.despawn(marker);
        }
        self.piles.clear();
        self.looted.clear();
        self.selected = None;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use engine::anim::AnimatedModel;
    use engine::{EngineLimits, Place};
    use std::sync::Arc;

    fn animated_corpse(world: &mut World, actor_id: ActorId) -> DeadActorRecord {
        let spec = crate::combat::catalog::mesh_spec("wolf").expect("wolf combat mesh");
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join(spec.source);
        let root = path.parent().expect("wolf asset parent");
        let model = AnimatedModel::load_with(&path, root, &EngineLimits::default())
            .expect("load animated wolf fixture");
        let entity = world
            .spawn_animated_shared(Arc::new(model), Place::default())
            .expect("spawn animated corpse fixture");
        DeadActorRecord::new(
            actor_id,
            "wolf".to_owned(),
            GlobalPosition::ORIGIN,
            entity,
            None,
        )
    }

    fn reconciled(actor_index: i32) -> (World, CorpseLifecycle, ActorId) {
        let mut world = World::new();
        let mut corpses = CorpseLifecycle::default();
        let id = ActorId::from_runtime_index(actor_index);
        let record = animated_corpse(&mut world, id);
        corpses.reconcile(&mut world, record).unwrap();
        (world, corpses, id)
    }

    #[test]
    fn selection_is_stable_actor_identity_and_close_controls_open_state() {
        let (_world, mut corpses, id) = reconciled(42);
        assert!(!corpses.is_open());
        corpses.open(id).unwrap();
        assert!(corpses.is_open());
        assert_eq!(corpses.selected(), Some(id));
        corpses.close();
        assert!(!corpses.is_open());
        assert_eq!(corpses.selected(), None);
    }

    #[test]
    fn invalid_selection_returns_typed_error_and_preserves_selection() {
        let (_world, mut corpses, selected) = reconciled(1);
        corpses.open(selected).unwrap();
        let missing = ActorId::from_runtime_index(2);
        assert!(
            matches!(corpses.select(missing), Err(CorpseError::InvalidSelection(id)) if id == missing)
        );
        assert_eq!(corpses.selected(), Some(selected));
    }

    #[test]
    fn partial_pile_override_preserves_marker_and_open_selection() {
        let (world, mut corpses, id) = reconciled(3);
        corpses.open(id).unwrap();
        let mut partial = corpses.pile().unwrap().clone();
        partial.coin = 1;
        partial.items.clear();
        corpses.override_reconciled_pile(partial.clone());
        assert_eq!(corpses.pile(), Some(&partial));
        assert_eq!(corpses.selected(), Some(id));
        assert!(corpses.has_marker(id));
        assert!(corpses.marker_visible(&world));
    }

    #[test]
    fn full_loot_completion_clears_pile_selection_marker_and_sets_looted() {
        let (mut world, mut corpses, id) = reconciled(4);
        corpses.open(id).unwrap();
        corpses.finish_loot(&mut world, id);
        assert!(!corpses.contains(id));
        assert!(!corpses.is_open());
        assert!(!corpses.has_marker(id));
        assert!(!corpses.marker_visible(&world));
        assert!(corpses.is_looted(id));
    }

    #[test]
    fn clear_removes_all_state_and_live_marker_entities() {
        let (mut world, mut corpses, first) = reconciled(5);
        let second = ActorId::from_runtime_index(6);
        let record = animated_corpse(&mut world, second);
        corpses.reconcile(&mut world, record).unwrap();
        let markers: Vec<EntityId> = corpses.markers.values().copied().collect();
        corpses.open(first).unwrap();
        corpses.clear(&mut world);
        assert_eq!(corpses.count(), 0);
        assert_eq!(corpses.selected(), None);
        assert!(corpses.looted.is_empty());
        assert!(corpses.markers.is_empty());
        for marker in markers {
            assert!(world.entity(marker).is_err());
        }
    }

    #[test]
    fn invalid_presentation_setup_is_atomic() {
        let mut world = World::new();
        let mut corpses = CorpseLifecycle::default();
        let id = ActorId::from_runtime_index(7);
        let static_entity = world.spawn(sparkle_mesh().expect("static marker fixture"));
        let record = DeadActorRecord::new(
            id,
            "wolf".to_owned(),
            GlobalPosition::ORIGIN,
            static_entity,
            None,
        );
        assert!(matches!(
            corpses.reconcile(&mut world, record),
            Err(CorpseError::Engine(_))
        ));
        assert!(!corpses.contains(id));
        assert!(!corpses.has_marker(id));
        assert!(!corpses.is_looted(id));
        assert!(!corpses.is_open());
    }
}
