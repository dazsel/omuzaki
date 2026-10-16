//! Property test over long random event orders (seeded, so it is the same every run): whatever the host answers, in
//! whatever order, the machine never leaves a step before the answer to its own latest operation, and never keeps a
//! selected card with nothing in flight and no failure to show.

use super::*;

const GRANTED: Cameras = Cameras::One(Permission::Granted);

fn probes() -> Probes {
    Probes {
        cameras: GRANTED,
        tracker: TrackerModel::Ready,
        iphone: IPhone::Paired,
        recommended_output: Some(OutputKind::Spout),
        assistant: true,
        language: LanguageOffer::None,
        project_open: false,
        privacy_claim_true: true,
    }
}

/// A small deterministic generator (an LCG), so no dependency and no clock.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        usize::try_from(self.0 >> 33).unwrap_or(0) % n
    }
}

fn pick_event(flow: &SetupFlow, rng: &mut Rng) -> Event {
    // Half of the answers go to a request that is really outstanding, so deep states are reached.
    let outstanding: Vec<u32> =
        [flow.input_op.request(), flow.output_op.request(), flow.character_op.request(), flow.calibration_op.request()]
            .into_iter()
            .flatten()
            .collect();
    let request = if !outstanding.is_empty() && rng.below(2) == 0 {
        outstanding[rng.below(outstanding.len())]
    } else {
        u32::try_from(rng.below(8)).unwrap_or(0)
    };
    match rng.below(24) {
        0 | 1 => Event::Next,
        2 => Event::Back,
        3 => Event::Skip,
        4 => Event::ChooseCharacter(CharacterChoice::Sample),
        5 => Event::ChooseCharacter(CharacterChoice::ImportVrm),
        6 => Event::ChooseInput(InputChoice::Webcam),
        7 => Event::ChooseInput(InputChoice::Iphone),
        8 => Event::ChooseInput(InputChoice::LifeLayerOnly),
        9 => Event::ChooseOutput(OutputKind::Spout),
        10 => Event::ChooseOutput(OutputKind::Ndi),
        11 => Event::Probed(probes()),
        12 => Event::Probed(Probes { cameras: Cameras::None, ..probes() }),
        13 => Event::Probed(Probes { recommended_output: Some(OutputKind::Ndi), ..probes() }),
        14 => Event::Probed(Probes { recommended_output: None, ..probes() }),
        15 => Event::Probed(Probes { tracker: TrackerModel::Unavailable, iphone: IPhone::NotPaired, ..probes() }),
        16 | 17 | 22 => Event::Applied { request },
        18 => Event::Refused { request },
        19 | 20 => Event::CalibrationDone { request },
        21 => Event::CalibrationFailed { request },
        _ => Event::Finish,
    }
}

fn check_move(before: &SetupFlow, after: &SetupFlow, event: Event) {
    // A step is left forward only on the answer to its own latest operation, or by the person's explicit skip.
    if after.step <= before.step || after.finished {
        return;
    }
    let ctx = format!("{:?} -> {:?} on {event:?}", before.step, after.step);
    match before.step {
        Step::Character => {
            assert!(after.character_op.settled(after.character) || after.character.is_none(), "{ctx}");
        }
        Step::Input => assert!(after.input_op.settled(after.input), "{ctx}"),
        Step::Calibrate => {
            assert!(after.calibration_op.applied().is_some() || after.skipped.contains(&Step::Calibrate), "{ctx}");
        }
        Step::Output => {
            assert!(after.output_op.settled(after.output) || after.skipped.contains(&Step::Output), "{ctx}");
        }
        Step::Welcome | Step::GoLive => {}
    }
}

fn check_state(f: &SetupFlow) {
    let v = f.view();
    assert!(v.shown.contains(&v.step));
    // No selected card with nothing in flight, unless it is held or its failure is shown.
    assert!(
        f.output.is_none()
            || f.output_op.settled(f.output)
            || f.output_op.request().is_some()
            || f.output_op.failed(f.output),
        "output {:?}",
        f.output
    );
    assert!(
        f.character.is_none()
            || f.character_op.settled(f.character)
            || f.character_op.request().is_some()
            || f.character_op.failed(f.character),
        "character {:?}",
        f.character
    );
    assert!(!f.calibration_wanted || f.calibration_op.request().is_some(), "a wanted run is started");
    assert!(
        f.then.is_none() || f.input_op.settled(f.input) || f.input_op.request().is_some() || f.input_op.failed(f.input),
        "a waiting Continue has something to wait for"
    );
    // A step past Input has a source.
    assert!(f.step <= Step::Input || f.input.is_some());
}

#[test]
fn random_event_orders_keep_the_invariants() {
    let starts: [fn() -> SetupFlow; 3] = [
        || SetupFlow::new(probes(), Progress::default(), Current::default()),
        || {
            let current = Current { input: Some(InputChoice::Webcam), output: Some(OutputKind::Spout) };
            SetupFlow::new(probes(), Progress::default(), current)
        },
        || SetupFlow::new(probes(), Progress { step: Step::Calibrate, ..Progress::default() }, Current::default()),
    ];
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for round in 0..9_000 {
        let mut f = starts[round % starts.len()]();
        for i in 0..24 {
            let event = pick_event(&f, &mut rng);
            let before = f.clone();
            let fx = f.handle(event, i * 10);
            check_move(&before, &f, event);
            check_state(&f);
            // Persist always carries the progress as it is, and nothing is shown before it is saved.
            if let Some(show) = fx.iter().position(|e| matches!(e, Effect::Show(_))) {
                assert!(fx[..show].iter().any(|e| matches!(e, Effect::Persist(_))), "{fx:?}");
            }
        }
    }
}
