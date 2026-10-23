//! Eyes lead target-driven head turns (#174, FE-07): the timing of a 40 degree look, a small look that moves only the
//! eyes, the comfort zone, a person's head left alone, the blink cue, `Look` cues, the life layer not leading the same
//! turn twice, determinism, and zero allocation in the frame.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::cast_precision_loss, clippy::float_cmp)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_anim::gaze::{
    EntityId, GazeCoordinator, GazeScene, GazeSolver, GazeTarget, GazeTurn, HeadShare, X_RADIANS_PER_UNIT,
};
use omuzaki_anim::life::Life;
use omuzaki_anim_api::{Bus, Channel, ChannelMask, Cues, LifeConfig, LifeLayer, Source, Tick};
use omuzaki_core::glam::{Mat4, Vec3};

struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    // `const` init with no destructor: reading it never allocates, so it is safe inside the allocator.
    static MEASURING: Cell<bool> = const { Cell::new(false) };
}

// SAFETY: every method forwards to `System` unchanged; the counter is the only addition.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if MEASURING.try_with(Cell::get).unwrap_or(false) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Frames per second: fine enough to time a 60 ms window.
const RATE: f32 = 240.0;

/// The camera sits 2 m in front; the entity 7 is a prop.
struct Scene;

impl GazeScene for Scene {
    fn camera_position(&self) -> Option<Vec3> {
        Some(Vec3::new(0.0, 0.0, 2.0))
    }

    fn entity_position(&self, id: EntityId) -> Option<Vec3> {
        (id == EntityId(7)).then(|| at(-30.0, 2.0))
    }
}

/// A point `degrees` toward the character's left, `distance` metres away, at eye height.
fn at(degrees: f32, distance: f32) -> Vec3 {
    let a = degrees.to_radians();
    Vec3::new(a.sin(), 0.0, a.cos()) * distance
}

/// Half the distance between the pupils, metres: the test's eye frame is in metres.
const HALF_IPD: f32 = 0.032;

fn coordinator(seed: u64) -> GazeCoordinator {
    GazeCoordinator::new(GazeSolver::new(HALF_IPD), HeadShare::default(), seed)
}

/// One frame on a fresh bus (what the rest pose and the scores leave), with `prepare` run on it first.
fn step(gaze: &mut GazeCoordinator, index: u64, target: GazeTarget, prepare: impl Fn(&mut Bus)) -> (GazeTurn, Bus) {
    let mut bus = Bus::default();
    prepare(&mut bus);
    let turn = gaze.step(Tick::at_rate(index, RATE), Mat4::IDENTITY, target, &Scene, &mut bus);
    (turn, bus)
}

/// The yaw the eyes look with inside the head, radians.
fn eye_yaw(turn: &GazeTurn) -> f32 {
    turn.eyes.look().0 * X_RADIANS_PER_UNIT
}

/// Runs `frames` frames looking at `target` from straight ahead and returns every frame's turn.
fn look_from_rest(seed: u64, target: GazeTarget, frames: u64) -> Vec<(GazeTurn, Bus)> {
    let mut gaze = coordinator(seed);
    // A second at the camera first: everything at rest.
    for i in 0..240 {
        step(&mut gaze, i, GazeTarget::Camera, |_| {});
    }
    (240..240 + frames).map(|i| step(&mut gaze, i, target, |_| {})).collect()
}

/// The first time (seconds after the change) at which `f` holds.
fn first(frames: &[(GazeTurn, Bus)], f: impl Fn(&GazeTurn) -> bool) -> Option<f64> {
    frames.iter().position(|(turn, _)| f(turn)).map(|i| (i + 1) as f64 / f64::from(RATE))
}

#[test]
fn a_40_degree_look_turns_the_eyes_first_and_the_head_after_them() {
    let goal = 40_f32.to_radians();
    let reach = HeadShare::default().eye_reach_rad;
    let mut starts = Vec::new();
    for seed in 0..12 {
        let frames = look_from_rest(seed, GazeTarget::Point(at(40.0, 2.0)), 480);
        // The head: starts after 120 to 200 ms, settles within 600 ms, never past its goal by more than 3 degrees.
        let start = first(&frames, |t| t.head[0] > 1e-4).unwrap();
        // The eyes: 90% of the way within 60 ms. The way is where they stand when the head sets off, all of the look
        // up to their limit (the far eye stops at 40 degrees, one look unit).
        let started_at = frames.iter().position(|(t, _)| t.head[0] > 1e-4).unwrap();
        let held = eye_yaw(&frames[started_at - 1].0);
        assert!(held > 0.97 * goal, "seed {seed}: the eyes hold {} degrees", held.to_degrees());
        let eyes_90 = first(&frames, |t| eye_yaw(t) >= 0.9 * held).unwrap();
        assert!(eyes_90 <= 0.060, "seed {seed}: the eyes reach 90% after {:.1} ms", eyes_90 * 1e3);
        assert!(
            (0.120..=0.200 + 1.0 / f64::from(RATE)).contains(&start),
            "seed {seed}: the head starts after {start:.3} s"
        );
        let final_head = goal - reach;
        let settle = frames.iter().rposition(|(t, _)| (t.head[0] - final_head).abs() > 0.5_f32.to_radians()).unwrap();
        let settle = (settle + 1) as f64 / f64::from(RATE);
        assert!(settle <= 0.600, "seed {seed}: the head settles after {settle:.3} s");
        let overshoot = frames.iter().map(|(t, _)| t.head[0] - final_head).fold(f32::MIN, f32::max);
        assert!(overshoot <= 3_f32.to_radians(), "seed {seed}: the head overshoots by {overshoot}");
        // At the end the eyes hold the 15 degrees they can reach and the head the rest, and the bus has the head.
        let (last, bus) = frames.last().unwrap();
        assert!((eye_yaw(last) - reach).abs() < 0.01, "seed {seed}: the eyes end at {}", eye_yaw(last).to_degrees());
        assert!((bus.values[Channel::HEAD_YAW] - final_head).abs() < 1e-3);
        assert_eq!(bus.owners.of(Channel::HEAD_YAW), Source::Score);
        assert!(!last.leading, "seed {seed}: the turn is over");
        starts.push(start);
        if seed == 0 {
            println!(
                "40 degrees: eyes 90% at {:.1} ms, head starts at {:.1} ms, settles by {:.1} ms, overshoot {:.3} deg",
                eyes_90 * 1e3,
                start * 1e3,
                settle * 1e3,
                overshoot.max(0.0).to_degrees()
            );
        }
    }
    // The wait varies with the seed, across the window.
    let (lo, hi) = starts.iter().fold((f64::MAX, f64::MIN), |(lo, hi), s| (lo.min(*s), hi.max(*s)));
    println!("head start over 12 seeds: {:.1} to {:.1} ms", lo * 1e3, hi * 1e3);
    assert!(hi - lo > 0.02, "the head's wait is drawn from its span: {starts:?}");
}

#[test]
fn a_10_degree_look_moves_only_the_eyes() {
    let frames = look_from_rest(3, GazeTarget::Point(at(10.0, 2.0)), 720);
    assert!(frames.iter().all(|(t, bus)| t.head == [0.0; 2] && bus.values[Channel::HEAD_YAW] == 0.0));
    let (last, bus) = frames.last().unwrap();
    assert!((eye_yaw(last) - 10_f32.to_radians()).abs() < 0.01);
    assert_eq!(bus.owners.of(Channel::HEAD_YAW), Source::Rest, "the head is not claimed for a look it has no part in");
    assert!(frames.iter().all(|(t, _)| !t.leading && !t.shifted));
}

#[test]
fn the_head_stays_in_its_comfort_zone_and_the_eyes_take_the_rest() {
    let share = HeadShare::default();
    let frames = look_from_rest(5, GazeTarget::Point(at(70.0, 2.0)), 720);
    let (last, _) = frames.last().unwrap();
    assert!((last.head[0] - share.max_yaw_rad).abs() < 1e-4, "the head stops at {}", last.head[0].to_degrees());
    assert!((eye_yaw(last) + last.head[0] - 70_f32.to_radians()).abs() < 0.01, "the eyes cover the other 25 degrees");
    // Up: the pitch limit holds the same way (pitch is positive down, so up is negative).
    let up = Vec3::new(0.0, 60_f32.to_radians().sin(), 60_f32.to_radians().cos()) * 2.0;
    let frames = look_from_rest(5, GazeTarget::Point(up), 720);
    assert!((frames.last().unwrap().0.head[1] + share.max_pitch_rad).abs() < 1e-4);
}

#[test]
fn a_person_driving_the_head_is_never_turned_and_the_eyes_do_the_look() {
    let mut gaze = coordinator(1);
    let tracked = |bus: &mut Bus| {
        bus.values[Channel::HEAD_YAW] = 0.1;
        bus.owners.claim(Source::Tracker, ChannelMask::HEAD);
    };
    let mut last = None;
    for i in 0..480 {
        let (turn, bus) = step(&mut gaze, i, GazeTarget::Point(at(40.0, 2.0)), tracked);
        assert_eq!(bus.values[Channel::HEAD_YAW], 0.1, "frame {i}: the tracked head is left as it is");
        assert_eq!(bus.owners.of(Channel::HEAD_YAW), Source::Tracker);
        assert_eq!(turn.head, [0.0; 2]);
        last = Some(turn);
    }
    // The eyes aim from the tracked head: 40 degrees minus its 0.1 rad turn.
    let eyes = eye_yaw(&last.unwrap());
    assert!((eyes - (40_f32.to_radians() - 0.1)).abs() < 0.01, "the eyes look {} degrees", eyes.to_degrees());
}

#[test]
fn a_large_shift_asks_the_life_layer_for_a_blink_once_and_marks_the_turn_as_led() {
    let frames = look_from_rest(2, GazeTarget::Point(at(40.0, 2.0)), 480);
    let shifted: Vec<usize> = frames.iter().enumerate().filter(|(_, (t, _))| t.shifted).map(|(i, _)| i).collect();
    assert_eq!(shifted, [0], "one blink cue, on the frame the look changes");
    let mut cues = Cues::default();
    frames[0].0.apply(&mut cues);
    assert!(cues.look_shift && cues.eyes_lead_turn);
    frames[1].0.apply(&mut cues);
    assert!(!cues.look_shift && cues.eyes_lead_turn);
    frames.last().unwrap().0.apply(&mut cues);
    assert!(!cues.look_shift && !cues.eyes_lead_turn);
}

#[test]
fn a_look_cue_holds_its_target_then_hands_back() {
    let mut gaze = coordinator(4);
    for i in 0..240 {
        step(&mut gaze, i, GazeTarget::Camera, |_| {});
    }
    // The prop, 30 degrees to the character's right, for half a second.
    gaze.look(GazeTarget::Entity(EntityId(7)), 0.5);
    let during: Vec<GazeTurn> = (240..360).map(|i| step(&mut gaze, i, GazeTarget::Camera, |_| {}).0).collect();
    let peak = during.iter().map(|t| t.head[0]).fold(0.0_f32, f32::min);
    assert!(peak < -0.2, "the head turns toward the prop: {peak}");
    assert!(during.iter().any(|t| (eye_yaw(t) + t.head[0] + 30_f32.to_radians()).abs() < 0.02));
    // After the hold, back to the camera: head and eyes home.
    let after: Vec<GazeTurn> = (360..840).map(|i| step(&mut gaze, i, GazeTarget::Camera, |_| {}).0).collect();
    let last = after.last().unwrap();
    assert!(last.head[0].abs() < 1e-3 && eye_yaw(last).abs() < 1e-3, "{last:?}");
    // A hold of zero lasts until the next look.
    gaze.look(GazeTarget::Point(at(25.0, 2.0)), 0.0);
    let held: Vec<GazeTurn> = (840..2040).map(|i| step(&mut gaze, i, GazeTarget::Camera, |_| {}).0).collect();
    assert!((eye_yaw(held.last().unwrap()) + held.last().unwrap().head[0] - 25_f32.to_radians()).abs() < 0.01);
}

/// The head the life layer leaves after a coordinated look, frame by frame, with or without telling it about the turn.
fn life_head(tell: bool) -> Vec<f32> {
    let mut gaze = coordinator(9);
    let mut life = Life::new(LifeConfig::new(21));
    let mut heads = Vec::new();
    for i in 0..960 {
        let target = if i < 240 { GazeTarget::Camera } else { GazeTarget::Point(at(60.0, 2.0)) };
        let mut bus = Bus::default();
        let tick = Tick::at_rate(i, RATE);
        let turn = gaze.step(tick, Mat4::IDENTITY, target, &Scene, &mut bus);
        let mut cues = Cues::default();
        if tell {
            turn.apply(&mut cues);
        }
        life.step(tick, &cues, &mut bus);
        if i >= 240 {
            heads.push(bus.values[Channel::HEAD_YAW]);
        }
    }
    heads
}

#[test]
fn the_life_layer_does_not_lead_a_turn_the_eyes_already_lead() {
    let goal = 45_f32.to_radians();
    let crossed = |heads: &[f32]| heads.iter().position(|h| *h >= 0.5 * goal).map(|i| i as f64 / f64::from(RATE));
    let (told, untold) = (life_head(true), life_head(false));
    let (told, untold) = (crossed(&told).unwrap(), crossed(&untold).unwrap());
    println!("the head is half way after {:.0} ms told, {:.0} ms untold", told * 1e3, untold * 1e3);
    // Untold, the life layer sees the coordinator's head turn as a fast source turn and holds it back again.
    assert!(untold - told >= 0.05, "told {told:.3} s, untold {untold:.3} s");
}

#[test]
fn the_same_seed_gives_the_same_look_and_a_frame_allocates_nothing() {
    let a = look_from_rest(6, GazeTarget::Point(at(40.0, 2.0)), 240);
    let b = look_from_rest(6, GazeTarget::Point(at(40.0, 2.0)), 240);
    assert!(a.iter().zip(&b).all(|((x, _), (y, _))| x == y));

    let mut gaze = coordinator(8);
    let target = |i: u64| GazeTarget::Point(at(if (i / 240).is_multiple_of(2) { -35.0 } else { 40.0 }, 2.0));
    let mut bus = Bus::default();
    let mut run = |from: u64, to: u64| {
        for i in from..to {
            bus = Bus::default();
            gaze.step(Tick::at_rate(i, RATE), Mat4::IDENTITY, target(i), &Scene, &mut bus);
        }
    };
    run(0, 480);
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    MEASURING.with(|m| m.set(true));
    run(480, 2400);
    MEASURING.with(|m| m.set(false));
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - before;
    assert_eq!(allocations, 0, "1920 coordinated frames made {allocations} allocations");
}
