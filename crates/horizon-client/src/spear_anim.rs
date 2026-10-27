//! Spears' animations (`SpearAnimations`): the first-person jab and charge,
//! and the third-person arm and held item, timed by the `kinetic_weapon`
//! component. The curves are `net.minecraft.util.Ease`'s.
use crate::mobs::anim::Poses;
use glam::{Mat4, Vec3};
use horizon_core::geometry::trig::Mth;
use serde_json::Value;
use std::f32::consts::PI;

const DEG: f32 = PI / 180.0;

/// What the animations and a charge read of `kinetic_weapon`: the raise
/// delay, each condition's `max_duration_ticks` (0 when absent),
/// `forward_movement`, and the conditions themselves.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct KineticTimings {
    pub delay_ticks: i32,
    pub dismount_ticks: i32,
    pub knockback_ticks: i32,
    pub damage_ticks: i32,
    pub forward_movement: f32,
    /// `contact_cooldown_ticks`: before the same entity can be hit again.
    pub contact_cooldown_ticks: i32,
    pub damage_multiplier: f32,
    /// The dismount, knockback and damage conditions.
    pub conditions: [Option<KineticCondition>; 3],
}
/// `KineticWeapon.Condition`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct KineticCondition {
    pub max_duration_ticks: i32,
    pub min_speed: f32,
    pub min_relative_speed: f32,
}
impl KineticCondition {
    fn parse(v: &Value) -> Option<Self> {
        let speed = |key: &str| v[key].as_f64().unwrap_or(0.0) as f32;
        Some(Self {
            max_duration_ticks: v["max_duration_ticks"].as_i64()? as i32,
            min_speed: speed("min_speed"),
            min_relative_speed: speed("min_relative_speed"),
        })
    }
    /// `Condition.test`.
    pub fn test(self, ticks_used: i32, speed: f64, relative_speed: f64, factor: f64) -> bool {
        ticks_used <= self.max_duration_ticks
            && speed >= f64::from(self.min_speed) * factor
            && relative_speed >= f64::from(self.min_relative_speed) * factor
    }
}
impl KineticTimings {
    pub fn parse(v: &Value) -> Option<Self> {
        let keys = [
            "dismount_conditions",
            "knockback_conditions",
            "damage_conditions",
        ];
        let conditions = keys.map(|key| KineticCondition::parse(&v[key]));
        let ticks = |i: usize| conditions[i].map_or(0, |c| c.max_duration_ticks);
        v.is_object().then(|| Self {
            delay_ticks: v["delay_ticks"].as_i64().unwrap_or(0) as i32,
            dismount_ticks: ticks(0),
            knockback_ticks: ticks(1),
            damage_ticks: ticks(2),
            forward_movement: v["forward_movement"].as_f64().unwrap_or(0.0) as f32,
            contact_cooldown_ticks: v["contact_cooldown_ticks"].as_i64().unwrap_or(10) as i32,
            damage_multiplier: v["damage_multiplier"].as_f64().unwrap_or(1.0) as f32,
            conditions,
        })
    }
}

/// `Mth.clamp(Mth.inverseLerp(time, start, end), 0, 1)`.
fn progress(time: f32, start: f32, end: f32) -> f32 {
    ((time - start) / (end - start)).clamp(0.0, 1.0)
}
mod ease {
    use super::{Mth, PI};
    pub fn in_quad(x: f32) -> f32 {
        x * x
    }
    pub fn in_out_sine(x: f32) -> f32 {
        -((PI * x).mcos() - 1.0) / 2.0
    }
    pub fn out_back(x: f32) -> f32 {
        1.0 + 2.70158 * (x - 1.0).powi(3) + 1.70158 * (x - 1.0).powi(2)
    }
    pub fn in_out_back(x: f32) -> f32 {
        if x < 0.5 {
            4.0 * x * x * (7.189819 * x - 2.5949094) / 2.0
        } else {
            let dt = 2.0 * x - 2.0;
            (dt * dt * (3.5949094 * dt + 2.5949094) + 2.0) / 2.0
        }
    }
    pub fn out_quart(x: f32) -> f32 {
        1.0 - (1.0 - x).powi(4)
    }
    pub fn out_cubic(x: f32) -> f32 {
        1.0 - (1.0 - x).powi(3)
    }
    pub fn in_out_expo(x: f32) -> f32 {
        let x64 = f64::from(x);
        if x < 0.5 {
            if x == 0.0 {
                0.0
            } else {
                (2f64.powf(20.0 * x64 - 10.0) / 2.0) as f32
            }
        } else if x == 1.0 {
            1.0
        } else {
            ((2.0 - 2f64.powf(-20.0 * x64 + 10.0)) / 2.0) as f32
        }
    }
    pub fn in_out_elastic(x: f32) -> f32 {
        if x == 0.0 || x == 1.0 {
            return x;
        }
        let x64 = f64::from(x);
        let c5 = f64::from(PI * 4.0 / 9.0);
        let sin = ((20.0 * x64 - 11.125) * c5).sin();
        if x < 0.5 {
            (-(2f64.powf(20.0 * x64 - 10.0) * sin) / 2.0) as f32
        } else {
            (2f64.powf(-20.0 * x64 + 10.0) * sin / 2.0 + 1.0) as f32
        }
    }
    pub fn out_circ(x: f32) -> f32 {
        (1.0 - (x - 1.0) * (x - 1.0)).sqrt()
    }
    pub fn in_circ(x: f32) -> f32 {
        -(1.0 - x * x).sqrt() + 1.0
    }
}

/// `SpearAnimations.UseParams.fromKineticWeapon` at `time` ticks of use.
struct UseParams {
    raise: f32,
    raise_start: f32,
    raise_middle: f32,
    raise_end: f32,
    sway: f32,
    lower: f32,
    raise_back: f32,
    sway_intensity: f32,
    sway_slow: f32,
    sway_fast: f32,
}
impl UseParams {
    fn new(k: KineticTimings, time: f32) -> Self {
        let finish_raising = k.delay_ticks as f32;
        let finish_swaying = (k.dismount_ticks + k.delay_ticks) as f32;
        let finish_lowering = (k.knockback_ticks + k.delay_ticks) as f32;
        let finish_raising_back = (k.damage_ticks + k.delay_ticks) as f32;
        let raise = progress(time, 0.0, finish_raising);
        let sway = progress(time, finish_swaying - 20.0, finish_swaying);
        let raise_back = progress(time, finish_raising_back - 5.0, finish_raising_back);
        let sway_intensity = 2.0 * ease::out_circ(sway) - 2.0 * ease::in_circ(raise_back);
        Self {
            raise,
            raise_start: progress(raise, 0.0, 0.5),
            raise_middle: progress(raise, 0.5, 0.8),
            raise_end: progress(raise, 0.8, 1.0),
            sway,
            lower: ease::out_cubic(ease::in_out_elastic(progress(
                time - 20.0,
                finish_lowering - 40.0,
                finish_lowering,
            ))),
            raise_back,
            sway_intensity,
            sway_slow: (time * 19.0 * DEG).msin() * sway_intensity,
            sway_fast: (time * 30.0 * DEG).msin() * sway_intensity,
        }
    }
}

/// `SpearAnimations.hitFeedbackAmount`: the kick a charge's hit gives the
/// spear over the 10 ticks after it (`ticksSinceKineticHitFeedback`).
fn hit_feedback(ticks_since: f32) -> f32 {
    0.4 * (ease::out_quart(progress(ticks_since, 1.0, 3.0))
        - ease::in_out_sine(progress(ticks_since, 3.0, 10.0)))
}

/// `LivingEntity.getTicksSinceLastKineticHitFeedback` from the age the hit
/// came at (`None` before any): 0 without one.
pub fn ticks_since_hit(hit_age: Option<u32>, age: u32, partial: f32) -> f32 {
    hit_age.map_or(0.0, |hit| age.wrapping_sub(hit) as f32 + partial)
}

/// `PoseStack.rotateAround`: `rotation` about the point `(x, y, z)`.
fn rotate_around(rotation: Mat4, x: f32, y: f32, z: f32) -> Mat4 {
    let at = Vec3::new(x, y, z);
    Mat4::from_translation(at) * rotation * Mat4::from_translation(-at)
}

/// `SpearAnimations.firstPersonUse` after the hand's
/// `translate(invert * 0.56, -0.52, -0.72)`: the spear raised, held and
/// lowered over the charge, and kicked by a hit `since_hit` ticks ago.
pub fn first_person_use(left: bool, time: f32, k: KineticTimings, since_hit: f32) -> Mat4 {
    let p = UseParams::new(k, time);
    let invert = if left { -1.0 } else { 1.0 };
    Mat4::from_translation(Vec3::new(
        invert * (p.raise * 0.15 + p.raise_end * -0.05 + p.sway * -0.1 + p.sway_slow * 0.005),
        p.raise * -0.075 + p.raise_middle * 0.075 + p.sway_fast * 0.01,
        p.raise_start * 0.05 + p.raise_end * -0.05 + p.sway_slow * 0.005,
    )) * rotate_around(
        Mat4::from_rotation_x(
            (-65.0 * ease::in_out_back(p.raise) - 35.0 * p.lower
                + 100.0 * p.raise_back
                + -0.5 * p.sway_fast)
                * DEG,
        ),
        0.0,
        0.1,
        0.0,
    ) * rotate_around(
        Mat4::from_rotation_y(
            -(invert * (-90.0 * progress(p.raise, 0.5, 0.55) + 90.0 * p.sway + 2.0 * p.sway_slow))
                * DEG,
        ),
        invert * 0.15,
        0.0,
        0.0,
    ) * Mat4::from_translation(Vec3::new(0.0, -hit_feedback(since_hit), 0.0))
}

/// `SpearAnimations.firstPersonAttack` after `applyItemArmTransform`: the
/// jab, `attack` the swing's progress.
pub fn first_person_attack(left: bool, attack: f32) -> Mat4 {
    let invert = if left { -1.0 } else { 1.0 };
    let start = ease::in_out_sine(progress(attack, 0.0, 0.05));
    let middle = ease::out_back(progress(attack, 0.05, 0.2));
    let end = ease::in_out_expo(progress(attack, 0.4, 1.0));
    Mat4::from_translation(Vec3::new(
        invert * 0.1 * (start - middle),
        -0.075 * (start - end),
        0.65 * (start - middle),
    )) * Mat4::from_rotation_x(-70.0 * (start - end) * DEG)
        * Mat4::from_translation(Vec3::new(0.0, 0.0, -0.25 * (end - middle)))
}

/// `SpearAnimations.thirdPersonHandUse` (`ArmPose.SPEAR`): the arm holding a
/// spear points along the look, and while it charges (`use_ticks` > 0, with
/// `k` the weapon's timings) rises, sways and lowers.
pub fn third_person_hand_use(
    p: &mut Poses,
    right: bool,
    lowered: bool,
    use_ticks: f32,
    k: Option<KineticTimings>,
) {
    let arm = if right { "right_arm" } else { "left_arm" };
    let invert = if right { 1.0 } else { -1.0 };
    let (hx, hy) = (p.get("head")[3], p.get("head")[4]);
    let mut y = -0.1 * invert + hy;
    let mut x = -PI / 2.0 + hx + 0.8;
    if lowered {
        x -= 0.959_931_1;
    }
    y = DEG * (y / DEG).clamp(-60.0, 60.0);
    x = DEG * (x / DEG).clamp(-120.0, 30.0);
    let mut z = p.get(arm)[5];
    if use_ticks > 0.0
        && let Some(k) = k
    {
        let u = UseParams::new(k, use_ticks);
        y += -invert * u.sway_fast * DEG * u.sway_intensity;
        z += -invert * u.sway_slow * DEG * u.sway_intensity * 0.5;
        x += DEG
            * (-40.0 * u.raise_start + 30.0 * u.raise_middle - 20.0 * u.raise_end
                + 20.0 * u.lower
                + 10.0 * u.raise_back
                + 0.6 * u.sway_slow * u.sway_intensity);
    }
    let a = p.get(arm);
    (a[3], a[4], a[5]) = (x, y, z);
}

/// `SpearAnimations.thirdPersonAttackHand`: a stab swing of the arm, after
/// `setupAttackAnimation` turned the body.
pub fn third_person_attack_hand(p: &mut Poses, swing: f32, right: bool) {
    let body_y = p.get("body")[4];
    p.get("right_arm")[4] -= body_y;
    p.get("left_arm")[4] -= body_y;
    p.get("left_arm")[3] -= body_y;
    let prepare = ease::in_out_sine(progress(swing, 0.0, 0.05));
    let attack = ease::in_quad(progress(swing, 0.05, 0.2));
    let retract = ease::in_out_expo(progress(swing, 0.4, 1.0));
    let arm = if right { "right_arm" } else { "left_arm" };
    p.get(arm)[3] += (90.0 * prepare - 120.0 * attack + 30.0 * retract) * DEG;
}

/// `SpearAnimations.thirdPersonUseItem` (`ArmPose.SPEAR.animateUseItem`):
/// the held spear while it charges, `time` its ticks of use.
pub fn third_person_use_item(
    left: bool,
    swing: f32,
    time: f32,
    k: KineticTimings,
    since_hit: f32,
) -> Mat4 {
    if time == 0.0 {
        return Mat4::IDENTITY;
    }
    let attack = ease::in_quad(progress(swing, 0.05, 0.2));
    let retract = ease::in_out_expo(progress(swing, 0.4, 1.0));
    let u = UseParams::new(k, time);
    let invert = if left { -1.0 } else { 1.0 };
    let raise = 1.0 - ease::out_back(1.0 - u.raise);
    let kick = hit_feedback(since_hit);
    Mat4::from_translation(Vec3::new(
        0.0,
        -kick * 0.4,
        -k.forward_movement * (raise - u.raise_back) + kick,
    )) * rotate_around(
        Mat4::from_rotation_x(-(70.0 * (u.raise - u.raise_back) - 40.0 * (attack - retract)) * DEG),
        0.0,
        -0.03125,
        0.125,
    ) * rotate_around(
        Mat4::from_rotation_y(invert * 90.0 * (u.raise - u.sway + 3.0 * retract + attack) * DEG),
        0.0,
        0.0,
        0.125,
    )
}

/// `SpearAnimations.thirdPersonAttackItem`: the held item thrust out by a
/// stab swing, `forward` the main hand's `forward_movement` (0 without one).
pub fn third_person_attack_item(swing: f32, forward: f32) -> Mat4 {
    if swing <= 0.0 {
        return Mat4::IDENTITY;
    }
    let attack = ease::in_quad(progress(swing, 0.05, 0.2));
    let retract = ease::in_out_expo(progress(swing, 0.4, 1.0));
    rotate_around(
        Mat4::from_rotation_x(-70.0 * (attack - retract) * DEG),
        0.0,
        -0.125,
        0.125,
    ) * Mat4::from_translation(Vec3::new(0.0, forward * (attack - retract), 0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    const IRON: KineticTimings = KineticTimings {
        delay_ticks: 12,
        dismount_ticks: 50,
        knockback_ticks: 135,
        damage_ticks: 225,
        forward_movement: 0.38,
        contact_cooldown_ticks: 10,
        damage_multiplier: 0.95,
        conditions: [None; 3],
    };

    #[test]
    fn eases_match_their_endpoints() {
        for f in [
            ease::in_quad,
            ease::in_out_sine,
            ease::out_back,
            ease::in_out_back,
            ease::out_cubic,
            ease::in_out_expo,
            ease::in_out_elastic,
            ease::out_circ,
            ease::in_circ,
        ] {
            assert!(f(0.0).abs() < 1e-5 && (f(1.0) - 1.0).abs() < 1e-5);
        }
    }

    #[test]
    fn the_jab_returns_to_rest() {
        assert_eq!(first_person_attack(false, 0.0), Mat4::IDENTITY);
        assert!(first_person_attack(false, 1.0).abs_diff_eq(Mat4::IDENTITY, 1e-5));
        // Mid-jab the item is pushed and turned.
        assert!(!first_person_attack(false, 0.2).abs_diff_eq(Mat4::IDENTITY, 1e-3));
        assert_eq!(third_person_attack_item(0.0, 0.38), Mat4::IDENTITY);
        assert!(third_person_attack_item(1.0, 0.38).abs_diff_eq(Mat4::IDENTITY, 1e-5));
    }

    #[test]
    fn the_charge_raises_the_spear() {
        // At rest when the use starts, moved once the delay raised it.
        assert!(first_person_use(false, 0.0, IRON, 0.0).abs_diff_eq(Mat4::IDENTITY, 1e-5));
        let raised = first_person_use(false, IRON.delay_ticks as f32, IRON, 0.0);
        assert!(!raised.abs_diff_eq(Mat4::IDENTITY, 1e-2));
        // The left hand mirrors the right one's sideways part.
        let left = first_person_use(true, IRON.delay_ticks as f32, IRON, 0.0);
        assert!((left.w_axis.x + raised.w_axis.x).abs() < 1e-5);
        // Unused, the third-person item does not move.
        assert_eq!(
            third_person_use_item(false, 0.0, 0.0, IRON, 0.0),
            Mat4::IDENTITY
        );
    }

    #[test]
    fn a_hit_kicks_the_spear_for_ten_ticks() {
        assert_eq!(hit_feedback(0.0), 0.0);
        // Up to 0.4 two ticks in, back to rest by the tenth.
        assert!((hit_feedback(3.0) - 0.4).abs() < 1e-5);
        assert!(hit_feedback(10.0).abs() < 1e-5);
        assert_eq!(ticks_since_hit(None, 50, 0.5), 0.0);
        assert_eq!(ticks_since_hit(Some(48), 50, 0.5), 2.5);
    }

    #[test]
    fn kinetic_timings_parse() {
        let k = KineticTimings::parse(&serde_json::json!({
            "delay_ticks": 12,
            "dismount_conditions": {"max_duration_ticks": 50, "min_speed": 1.0},
            "forward_movement": 0.38
        }))
        .unwrap();
        assert_eq!(
            (k.delay_ticks, k.dismount_ticks, k.damage_ticks),
            (12, 50, 0)
        );
        assert_eq!((k.contact_cooldown_ticks, k.damage_multiplier), (10, 1.0));
        let dismount = k.conditions[0].unwrap();
        assert!(k.conditions[1].is_none() && k.conditions[2].is_none());
        // Condition.test: within its time, at its speed.
        assert!(dismount.test(50, 1.0, 0.0, 1.0));
        assert!(!dismount.test(51, 1.0, 0.0, 1.0));
        assert!(!dismount.test(10, 0.9, 0.0, 1.0));
        assert_eq!(KineticTimings::parse(&Value::Null), None);
    }
}
