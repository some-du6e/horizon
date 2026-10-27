//! Spears' jab (`PiercingWeapon`): an attack press with a piercing weapon in
//! the main hand stabs every mob along the weapon's attack range instead of
//! the one in the crosshair, then runs the Lunge enchantment
//! (`post_piercing_attack`). The client sends `STAB`
//! (`MultiPlayerGameMode.piercingAttack`); the server jabs and lunges
//! (`ServerGamePacketListenerImpl.handlePlayerAction`) and sends the lunge's
//! motion to the player, whose client moves with it.
//!
//! Holding use charges the spear (`kinetic_weapon`): each tick of the use,
//! the mobs along its reach are hit by how fast the player moves at them
//! (`KineticWeapon.damageEntities`), and a hit kicks the spear in the hand
//! (entity event 2, `LivingEntity.onKineticHit`).
use super::{Game, lan::LanHost, ray_box, view_vector};
use horizon_client::{
    first_person::{Hand, SwingAnimation},
    items::PiercingWeapon,
};
use horizon_core::geometry::Vec3;
use horizon_protocol::packets::play;

/// `ServerboundPlayerActionPacket.Action.STAB`.
const STAB: i32 = 8;

/// `LevelBasedValue.perLevel(base)`: `base` at level 1, `base` more per level.
fn per_level(base: f32, level: u32) -> f32 {
    base + base * (level - 1) as f32
}

/// Lunge's `ApplyEntityImpulse`: `Entity.getLookQuaternion` turns (0, 0, 1)
/// to the look, the coordinate scale (1, 0, 1) keeps its horizontal part and
/// the magnitude is 0.458 per level.
fn lunge_impulse(yaw: f32, pitch: f32, level: u32) -> Vec3 {
    let (yaw, pitch) = (yaw.to_radians(), pitch.to_radians());
    let push = f64::from(pitch.cos()) * f64::from(per_level(0.458, level));
    Vec3::new(
        -f64::from(yaw.sin()) * push,
        0.0,
        f64::from(yaw.cos()) * push,
    )
}

impl Game<'_> {
    /// The main-hand item's `piercing_weapon`.
    pub(super) fn held_piercing_weapon(&self) -> Option<PiercingWeapon> {
        let stack = self.inventory[self.selected].as_ref()?;
        self.res.items().get(&stack.item)?.piercing_weapon.clone()
    }
    /// The main-hand item's `attack_animation`.
    pub(super) fn held_attack_animation(&self) -> SwingAnimation {
        self.inventory[self.selected]
            .as_ref()
            .and_then(|s| self.res.items().get(&s.item))
            .map_or(SwingAnimation::DEFAULT, |i| i.attack_animation)
    }
    /// `Player.cannotAttackWithItem(mainHand, tolerance)` with `ticker` as the
    /// attack strength ticker: the item's `minimum_attack_charge` is not met
    /// yet (the client asks with no tolerance, the server with 5 ticks).
    pub(super) fn cannot_attack_with_held(&self, ticker: i32, tolerance: i32) -> bool {
        let Some(info) = self.inventory[self.selected]
            .as_ref()
            .and_then(|s| self.res.items().get(&s.item))
        else {
            return false;
        };
        let delay = horizon_client::attack_strength::delay(self.main_hand_attack_speed());
        let strength = (ticker + tolerance) as f32 / delay;
        info.minimum_attack_charge > 0.0 && strength < info.minimum_attack_charge
    }
    /// `MultiPlayerGameMode.piercingAttack` on a server's client: `STAB`, then
    /// this player's own half of the jab.
    pub(super) fn remote_piercing_attack(&mut self, weapon: &PiercingWeapon) {
        self.send(play::Serverbound::PlayerAction(play::PlayerAction {
            action: STAB,
            pos: [0; 3],
            direction: 0,
            sequence: 0,
        }));
        self.local_piercing_attack(weapon);
    }
    /// The client's side of `piercingAttack`: `onAttack`
    /// (`resetOnlyAttackStrengthTicker`), the swing and the weapon's sound,
    /// which the server plays for everyone else.
    pub(super) fn local_piercing_attack(&mut self, weapon: &PiercingWeapon) {
        self.hand_strength.reset_attack();
        self.swing_hand(Hand::Main, self.held_attack_animation());
        if let Some(sound) = &weapon.sound {
            let at = self.player.as_ref().map(|p| p.position.components());
            self.audio.play(sound, at, 1.0, 1.0);
        }
    }
    /// ServerGamePacketListenerImpl.handlePlayerAction STAB from a guest:
    /// the jab, the weapon's sound for the players around and the lunge's
    /// motion for the guest.
    pub(super) fn guest_stab(&mut self, host: &mut LanHost, conn: u32) {
        let jab = self.as_guest(host, conn, |g| {
            if g.cannot_attack_with_held(g.attack_ticks as i32, 5) {
                return None;
            }
            let weapon = g.held_piercing_weapon()?;
            let at = g.player.as_ref()?.position.components();
            let (hit, motion) = g.piercing_weapon_attack(&weapon);
            Some((weapon.sound, at, hit, motion))
        });
        let Some(Some((sound, at, hit, motion))) = jab else {
            return;
        };
        // PiercingWeapon.makeSound: `playSound(causer, ...)`, the jabbing
        // player's client plays its own.
        if let Some(sound) = sound {
            let source = horizon_client::settings::sound_source(&sound) as i32;
            host.hub.sound(&sound, at, 1.0, 1.0, source, Some(conn));
        }
        // The stabbed mobs now answer to this guest (game/lan_mobs.rs).
        for id in hit {
            host.mob_players.insert(id, conn);
        }
        self.send_guest_motion(host, conn, motion);
    }
    /// `PiercingWeapon.attack` by the player, server side: every mob along
    /// the attack range is stabbed, then `onAttack`, `postPiercingAttack`
    /// (Lunge), the hit sound and the swing. The stabbed mobs' ids, and the
    /// motion a lunge left the player with.
    pub(super) fn piercing_weapon_attack(
        &mut self,
        weapon: &PiercingWeapon,
    ) -> (Vec<u32>, Option<Vec3>) {
        // Attributes.ATTACK_DAMAGE, and the strength the stabs scale it by.
        let damage = 1.0
            + self.attack_item_info().map_or(0.0, |i| i.attack_damage)
            + self.effect_attack_bonus();
        let scale = self.attack_strength_scale();
        let mut hit = Vec::new();
        for index in self.mobs_along_attack_range() {
            if self.stab_mob(
                index,
                damage,
                scale,
                [true, weapon.deals_knockback, weapon.dismounts],
            ) {
                hit.push(self.mobs[index].id);
            }
        }
        self.attack_ticks = 0;
        let motion = self.lunge();
        let at = self.player.as_ref().map(|p| p.position.components());
        if !hit.is_empty()
            && let Some(sound) = &weapon.hit_sound
        {
            self.audio.play(sound, at, 1.0, 1.0);
        }
        self.swing_hand(Hand::Main, self.held_attack_animation());
        (hit, motion)
    }
    /// `ProjectileUtil.getHitEntitiesAlong` over the mobs for the main hand's
    /// attack range: the look's segment from the minimum reach to the
    /// maximum (longer by the player's motion along the look), cut at the
    /// first block. A mob is hit when the segment passes through its box, or
    /// through the box grown by the range's hitbox margin.
    fn mobs_along_attack_range(&self) -> Vec<usize> {
        let (Some(eye), Some(p)) = (self.eye_position(), self.player.as_ref()) else {
            return Vec::new();
        };
        let range = self.inventory[self.selected]
            .as_ref()
            .and_then(|s| self.res.items().get(&s.item))
            .map_or(&serde_json::Value::Null, |i| &i.attack_range);
        let value = |key: &str, default: f32| range[key].as_f64().map_or(default, |v| v as f32);
        // AttackRange.effectiveMinRange / effectiveMaxRange for a player.
        let (min, max) = if self.creative {
            (
                value("min_creative_reach", 0.0),
                value("max_creative_reach", 5.0),
            )
        } else {
            (value("min_reach", 0.0), value("max_reach", 3.0))
        };
        let margin = f64::from(value("hitbox_margin", 0.3));
        let look = view_vector(self.yaw, self.pitch);
        let along = p.velocity.x * look[0] + p.velocity.y * look[1] + p.velocity.z * look[2];
        let (start, mut end) = (f64::from(min), f64::from(max) + along.max(0.0));
        if let Some(block) = self.clip_ray(eye, look, end) {
            end = (0..3)
                .map(|a| (block.point[a] - eye[a]).powi(2))
                .sum::<f64>()
                .sqrt();
            if end < start {
                return Vec::new();
            }
        }
        let from = [0, 1, 2].map(|a| eye[a] + look[a] * start);
        let length = end - start;
        self.mobs
            .iter()
            .enumerate()
            .filter(|(_, m)| m.alive() && self.riding != Some(m.id))
            .filter(|(_, m)| {
                let bounds = m.bounds();
                ray_box(from, look, length, bounds).is_some()
                    || margin > 0.0
                        && ray_box(
                            from,
                            look,
                            length,
                            bounds.inflate(Vec3::new(margin, margin, margin)),
                        )
                        .is_some()
            })
            .map(|(i, _)| i)
            .collect()
    }
    /// `Player.stabAttack` on the mob at `index` with `base` attack damage
    /// and what it `[deals damage, deals knockback, dismounts]`: the
    /// enchantments' bonus and the damage, scaled by the attack strength
    /// unless the stab comes from the hand charging the spear, then a 0.4
    /// knockback and the weapon's own (Knockback), the rider taken off its
    /// mount, and the rest of a landed hit. Whether the mob was affected.
    fn stab_mob(&mut self, index: usize, base: f32, scale: f32, deals: [bool; 3]) -> bool {
        let [deals_damage, deals_knockback, dismounts] = deals;
        let kind = format!(
            "minecraft:{}",
            self.mobs[index].kind.id.trim_start_matches("minecraft:")
        );
        let mut magic = self.enchant_damage_bonus(&kind);
        let mut base = base;
        if self.using.is_none_or(|(slot, _)| slot != self.selected) {
            magic *= scale;
            base *= 0.2 + scale * scale * 0.8;
        }
        let Some(from) = self.player.as_ref().map(|p| p.position.components()) else {
            return false;
        };
        let old_health = self.mobs[index].health;
        let source = self.attack_source(from);
        let hurt = deals_damage && self.hurt_by_player(index, base + magic, source);
        if deals_knockback {
            // causeExtraKnockback(0.4), then causeExtraKnockback(getKnockback).
            let (xd, zd) = self.extra_knockback_direction();
            self.mobs[index].knockback(0.4, xd, zd);
            let (enchanted, _) = self.extra_knockback(&kind, 0.0);
            if enchanted > 0.0 {
                self.mobs[index].knockback(f64::from(enchanted), xd, zd);
            }
            self.stop_sprint_after_extra_knockback();
        }
        // Entity.stopRiding, unless the mob's type keeps its seat.
        let mob = &self.mobs[index];
        let dismounted = dismounts
            && mob.extra.mount.is_some()
            && !self
                .res
                .stations()
                .enchantments
                .entity_tags
                .get("minecraft:cannot_be_dismounted_by_item_usage")
                .is_some_and(|tag| tag.contains(&*mob.kind.id));
        if dismounted {
            let m = &mut self.mobs[index];
            m.extra.mount = None;
            m.extra.passenger = false;
        }
        if !hurt && !deals_knockback && !dismounted {
            return false;
        }
        // LivingEntity.setLastHurtMob (tamed wolves join in).
        self.combat.last_hurt_mob = Some((self.mobs[index].id, self.world.game_time));
        if hurt {
            self.weapon_hit(index);
        }
        // Player.damageStatsAndHearts.
        let mob = &self.mobs[index];
        let actual = old_health - mob.health;
        let (feet, height) = (mob.position.components(), mob.kind.height);
        self.stats
            .custom("damage_dealt", (actual * 10.0).round() as i32);
        if actual > 2.0 {
            let indicator = horizon_client::particles::ParticleOptions::simple(
                horizon_client::particles::ParticleType::DamageIndicator,
            );
            self.particles.send_particles(
                &indicator,
                [feet[0], feet[1] + height * 0.5, feet[2]],
                [0.1, 0.0, 0.1],
                0.2,
                (actual * 0.5) as u32,
            );
        }
        if !self.creative
            && let Some(p) = self.player.as_mut()
        {
            p.cause_food_exhaustion(0.1);
        }
        self.last_hurt_by_player
            .insert(self.mobs[index].id, self.lan_acting);
        // The hurt or death sound the mob raised, as Player.attack plays it.
        for event in std::mem::take(&mut self.mobs[index].outbox) {
            if let horizon_client::mobs::MobEvent::Sound(s, at, volume, pitch) = event {
                self.audio
                    .play(&format!("minecraft:{s}"), Some(at), volume, pitch);
            } else {
                self.mobs[index].outbox.push(event);
            }
        }
        true
    }
    /// `Item.use` for a kinetic weapon in the acting hand: `startUsingItem`
    /// (a new `recentKineticEnemies`) and `KineticWeapon.makeSound`, which a
    /// server's guest plays itself (`guest_spear_use_sound`). Whether the
    /// stack is one.
    pub(super) fn start_spear_use(&mut self, stack: &horizon_client::items::ItemStack) -> bool {
        let Some(info) = self.res.items().get(&stack.item) else {
            return false;
        };
        if info.kinetic_weapon.is_none() {
            return false;
        }
        let sound = info.kinetic_sound.clone();
        if self.using.is_none() {
            self.using = Some((self.held_slot(), 0));
            let who = self.lan_acting;
            for m in &mut self.mobs {
                m.extra.stabbed_by.retain(|(by, _)| *by != who);
            }
            if self.guest_use.is_none()
                && let Some(sound) = sound
            {
                let at = self.player.as_ref().map(|p| p.position.components());
                self.audio.play(&sound, at, 1.0, 1.0);
            }
        }
        true
    }
    /// The sound of the kinetic weapon a guest just started using, and where
    /// (`playSound(player, ...)`: the others hear it, the guest plays its own).
    pub(super) fn guest_spear_use_sound(&self) -> Option<(String, [f64; 3])> {
        let (slot, ticks) = self.using?;
        if ticks != 0 {
            return None;
        }
        let stack = self.stack_in(slot)?;
        let sound = self.res.items().get(&stack.item)?.kinetic_sound.clone()?;
        Some((sound, self.player.as_ref()?.position.components()))
    }
    /// `KineticWeapon.damageEntities`, `ticks_used` ticks into the use of a
    /// kinetic weapon `k` in the acting hand: past its delay, every mob along
    /// the reach not hit within the contact cooldown is stabbed by the
    /// conditions the player's and its speeds along the look meet, for the
    /// base attack damage plus the relative speed times the multiplier.
    pub(super) fn kinetic_use_tick(
        &mut self,
        k: horizon_client::spear_anim::KineticTimings,
        ticks_used: u32,
    ) {
        let used = ticks_used as i32 - k.delay_ticks;
        if used < 0 {
            return;
        }
        let Some(p) = self.player.as_ref() else {
            return;
        };
        // KineticWeapon.getMotion: the known speed per second, the player's
        // move this tick.
        let look = view_vector(self.yaw, self.pitch);
        let along = |v: Vec3| (v.x * look[0] + v.y * look[1] + v.z * look[2]) * 20.0;
        let speed = along(p.position.subtract(p.old_position));
        let (now, who) = (self.world.game_time, self.lan_acting);
        let mut affected = false;
        for index in self.mobs_along_attack_range() {
            let m = &mut self.mobs[index];
            let cooldown = i64::from(k.contact_cooldown_ticks);
            if m.extra
                .stabbed_by
                .iter()
                .any(|&(by, at)| by == who && now - at < cooldown)
            {
                continue;
            }
            // LivingEntity.rememberStabbedEntity.
            m.extra.stabbed_by.retain(|(by, _)| *by != who);
            m.extra.stabbed_by.push((who, now));
            let relative = (speed - along(m.position.subtract(m.previous))).max(0.0);
            let deals = k
                .conditions
                .map(|c| c.is_some_and(|c| c.test(used, speed, relative, 1.0)));
            if deals.iter().any(|&d| d) {
                // getAttributeBaseValue(ATTACK_DAMAGE): 1 for a player.
                let damage = 1.0 + (relative * f64::from(k.damage_multiplier)).floor() as f32;
                let [dismount, knockback, damages] = deals;
                affected |= self.stab_mob(index, damage, 1.0, [damages, knockback, dismount]);
            }
        }
        if affected {
            self.kinetic_hit = true;
        }
    }
    /// `LivingEntity.onKineticHit` for `m` (entity event 2): at most every 10
    /// ticks, the spear in use kicks. Whether it did.
    fn kinetic_feedback(m: &mut horizon_client::mobs::Mob) -> bool {
        if m.extra
            .kinetic_hit_age
            .is_some_and(|hit| m.age.wrapping_sub(hit) <= 10)
        {
            return false;
        }
        m.extra.kinetic_hit_age = Some(m.age);
        true
    }
    /// `KineticWeapon.makeLocalHitSound` for the kinetic weapon `item` used
    /// at `at`.
    fn kinetic_hit_sound(&mut self, item: Option<&str>, at: [f64; 3]) {
        let sound = item
            .and_then(|i| self.res.items().get(i))
            .and_then(|i| i.kinetic_hit_sound.clone());
        if let Some(sound) = sound {
            self.audio.play_unshared(&sound, Some(at), 1.0, 1.0);
        }
    }
    /// The window's own player's charge hit something (`onKineticHit`): its
    /// spear kicks and it hears the hit.
    pub(super) fn local_kinetic_hit(&mut self) {
        let fed = self
            .player_model
            .as_mut()
            .is_some_and(Self::kinetic_feedback);
        let item = self
            .using
            .and_then(|(slot, _)| self.stack_in(slot))
            .map(|s| s.item.clone());
        if fed && let Some(at) = self.player.as_ref().map(|p| p.position.components()) {
            self.kinetic_hit_sound(item.as_deref(), at);
        }
    }
    /// ClientPacketListener.handleEntityEvent 2 on a server's client: this
    /// player's or another entity's charge hit something.
    pub(super) fn remote_kinetic_hit(&mut self, id: i32) {
        if self.remote.as_ref().is_some_and(|r| r.entity_id == id) {
            self.local_kinetic_hit();
            return;
        }
        let Some(m) = self.mobs.iter_mut().find(|m| m.id == id as u32) else {
            return;
        };
        if !Self::kinetic_feedback(m) || m.extra.using_item == 0 {
            return;
        }
        let item = if m.extra.using_off_hand {
            m.extra.off_hand.clone()
        } else {
            m.extra.main_hand.clone()
        };
        let at = m.position.components();
        self.kinetic_hit_sound(item.as_deref(), at);
    }
    /// `EnchantmentHelper.doPostPiercingAttackEffects` for Lunge, the one
    /// enchantment with `post_piercing_attack` effects. Off a vehicle, not
    /// gliding, out of water, and in creative or with more than 6 food: the
    /// spear wears by one, the player tires by 4 per level, is pushed along
    /// its horizontal look by 0.458 per level (`ApplyEntityImpulse`) and the
    /// level's lunge sound plays. The player's new motion.
    fn lunge(&mut self) -> Option<Vec3> {
        let level = self.inventory[self.selected]
            .as_ref()?
            .enchantment_level("minecraft:lunge");
        let p = self.player.as_ref()?;
        if level == 0
            || self.riding.is_some()
            || p.fall_flying
            || p.fluids.was_touching_water
            || !(self.creative || p.health.food.food > 6)
        {
            return None;
        }
        self.damage_held(1);
        let impulse = lunge_impulse(self.yaw, self.pitch, level);
        let p = self.player.as_mut()?;
        p.cause_food_exhaustion(per_level(4.0, level));
        p.velocity = p.velocity.add_vector(impulse);
        p.impulse.apply_grace_time(10);
        let (at, motion) = (p.position.components(), p.velocity);
        let sound = format!("minecraft:item.spear.lunge_{}", level.clamp(1, 3));
        self.audio.play(&sound, Some(at), 1.0, 1.0);
        Some(motion)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lunge_pushes_along_the_horizontal_look() {
        // Facing south (yaw 0): +Z, 0.458 per level.
        let v = lunge_impulse(0.0, 0.0, 1);
        assert!(v.x.abs() < 1e-9 && v.y == 0.0);
        assert!((v.z - 0.458).abs() < 1e-6);
        let v = lunge_impulse(0.0, 0.0, 3);
        assert!((v.z - 3.0 * 0.458).abs() < 1e-6);
        // Facing west (yaw 90): -X.
        let v = lunge_impulse(90.0, 0.0, 1);
        assert!((v.x + 0.458).abs() < 1e-6 && v.z.abs() < 1e-6);
        // Looking up or down shortens the push and never lifts the player.
        let v = lunge_impulse(0.0, -60.0, 2);
        assert!(v.y == 0.0 && (v.z - 2.0 * 0.458 * 0.5).abs() < 1e-6);
        let v = lunge_impulse(0.0, 90.0, 1);
        assert!(v.horizontal_distance() < 1e-6);
    }

    #[test]
    fn lunge_exhaustion_is_four_per_level() {
        assert_eq!(per_level(4.0, 1), 4.0);
        assert_eq!(per_level(4.0, 3), 12.0);
    }
}
