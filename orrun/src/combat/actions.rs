//! Data-driven player action staging and execution.

use super::types::WorldCombat;
use crate::gamedata::{ActionId, ActionTarget};
use crate::resolution::TargetSelection;

impl WorldCombat {
    pub fn action_cd_frac(&self, action_id: &ActionId) -> f32 {
        let left = self
            .player()
            .canonical_actor
            .as_ref()
            .expect("canonical player")
            .actions()
            .cooldown_s(action_id);
        let max = self
            .game_data
            .action(action_id)
            .unwrap_or_else(|| panic!("unknown live action {action_id}"))
            .cooldown_s();
        if left <= 0.0 || max <= 0.0 {
            0.0
        } else {
            (left / max).clamp(0.0, 1.0) as f32
        }
    }

    pub fn action_cast_frac(&self) -> Option<f32> {
        let cast = self.player().canonical_actor.as_ref()?.actions().cast()?;
        (cast.total_s() > 0.0 && cast.remaining_s() > 0.0)
            .then(|| (cast.remaining_s() / cast.total_s()).clamp(0.0, 1.0) as f32)
    }

    pub fn action_cast_label(&self) -> Option<&str> {
        let cast = self.player().canonical_actor.as_ref()?.actions().cast()?;
        Some(
            self.game_data
                .action(cast.action_id())
                .unwrap_or_else(|| panic!("unknown live cast action {}", cast.action_id()))
                .name(),
        )
    }

    pub fn press_action(
        &mut self,
        action_id: &ActionId,
        player_x: f64,
        player_z: f64,
        _facing_x: f64,
        _facing_z: f64,
    ) -> bool {
        let (target_class, mana_cost, cast_s) = {
            let action = self
                .game_data
                .action(action_id)
                .unwrap_or_else(|| panic!("unknown live action {action_id}"));
            (action.target(), action.mana_cost(), action.cast_s())
        };
        let player_actor = self
            .player()
            .canonical_actor
            .as_ref()
            .expect("canonical player");
        if self.is_dead() || !player_actor.can_act() || player_actor.actions().cast().is_some() {
            return false;
        }
        if self
            .player()
            .canonical_actor
            .as_ref()
            .expect("canonical player")
            .actions()
            .cooldown_s(action_id)
            > 0.0
        {
            return false;
        }
        let target = match target_class {
            ActionTarget::ActorSelf | ActionTarget::Friendly => None,
            ActionTarget::Hostile | ActionTarget::Any => {
                let Some(lock) = self.lock_id() else {
                    self.note_fail("No target");
                    return false;
                };
                if self.hostile_actor_index(lock).is_none() {
                    self.note_fail("No target");
                    return false;
                }
                Some(lock)
            }
            ActionTarget::None => panic!("action {action_id} has no executable target"),
        };
        if self.player().resources.mana() < mana_cost {
            self.note_fail("Not enough mana");
            return false;
        }
        if cast_s > 0.0 {
            let target_actor = target.and_then(|idx| {
                self.hostiles()
                    .iter()
                    .find(|h| h.idx == idx)
                    .map(|h| crate::resolution::ResolutionActorId::new(h.actor_id().canonical()))
            });
            self.player_mut()
                .canonical_actor
                .as_mut()
                .expect("canonical player")
                .actions_mut()
                .start_cast(action_id.clone(), target_actor, cast_s);
            true
        } else {
            self.finish_action(action_id.clone(), target, player_x, player_z)
        }
    }

    fn finish_action(
        &mut self,
        action_id: ActionId,
        target: Option<i32>,
        player_x: f64,
        player_z: f64,
    ) -> bool {
        let target_class = self
            .game_data
            .action(&action_id)
            .expect("validated live action")
            .target();
        let selection = match target_class {
            ActionTarget::ActorSelf | ActionTarget::Friendly => TargetSelection::Single(0),
            ActionTarget::Hostile | ActionTarget::Any => {
                let Some(index) = target.and_then(|idx| self.hostile_actor_index(idx)) else {
                    self.note_fail("No target");
                    return false;
                };
                TargetSelection::Single(index)
            }
            ActionTarget::None => panic!("action {action_id} has no executable target"),
        };
        match self.execute_canonical(0, &action_id, selection, player_x, player_z) {
            Ok(resolution) => {
                let cooldown = self
                    .game_data
                    .action(&action_id)
                    .expect("validated live action")
                    .cooldown_s();
                if cooldown > 0.0 {
                    self.player_mut()
                        .canonical_actor
                        .as_mut()
                        .expect("canonical player")
                        .actions_mut()
                        .start_cooldown(action_id, cooldown);
                }
                self.pending_resolutions.push(resolution);
                true
            }
            Err(crate::resolution::ResolutionError::OutOfRange(_)) => {
                self.note_fail("Out of range");
                false
            }
            Err(crate::resolution::ResolutionError::NoTarget(_)) => {
                self.note_fail("No target");
                false
            }
            Err(crate::resolution::ResolutionError::InsufficientMana { .. }) => {
                self.note_fail("Not enough mana");
                false
            }
            Err(err) => panic!("canonical player action {action_id} failed: {err}"),
        }
    }

    pub(crate) fn tick_player_actions(&mut self, player_x: f64, player_z: f64, dt: f64) {
        if self.player().canonical_actor.is_none() {
            let actors = self.canonical_actors(player_x, player_z);
            self.sync_canonical_actors(actors, super::types::ActorId::PLAYER);
        }
        let completed = {
            let actor = self
                .player_mut()
                .canonical_actor
                .as_mut()
                .expect("canonical player");
            actor.tick_runtime(dt);
            actor.actions_mut().take_completed_cast()
        };
        if let Some(cast) = completed {
            let target = cast.target().and_then(|id| {
                self.hostiles()
                    .iter()
                    .find(|h| h.actor_id().canonical() == id.get())
                    .map(|h| h.idx)
            });
            self.finish_action(cast.action_id().clone(), target, player_x, player_z);
        }
    }

    pub(crate) fn note_fail(&mut self, line: &'static str) {
        self.log_mut().push(line);
        self.set_fail_tell(Some(line));
        self.set_fail_tell_timer(1.2);
    }

    pub fn fail_tell(&self) -> Option<&'static str> {
        (self.fail_tell_timer() > 0.0)
            .then_some(self.fail_tell_value())
            .flatten()
    }
}
