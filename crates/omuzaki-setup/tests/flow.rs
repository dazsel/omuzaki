//! Table tests for the first-run machine (issue 847, `docs/design/first-run.md` sections 3 and 8).

use omuzaki_setup::*;

const GRANTED: Cameras = Cameras::One(Permission::Granted);

fn probes() -> Probes {
    Probes {
        cameras: GRANTED,
        tracker: TrackerModel::Ready,
        iphone: IPhone::NotPaired,
        recommended_output: Some(OutputKind::Spout),
        assistant: true,
        language: LanguageOffer::None,
        project_open: false,
        privacy_claim_true: true,
    }
}

fn flow(p: Probes) -> SetupFlow {
    SetupFlow::new(p, Progress::default())
}

fn send(f: &mut SetupFlow, events: &[Event]) -> Vec<Effect> {
    events.iter().flat_map(|e| f.handle(*e, 0)).collect()
}

fn to_input(f: &mut SetupFlow) {
    send(f, &[Event::Next, Event::ChooseCharacter(CharacterChoice::Sample)]);
    assert_eq!(f.view().step, Step::Input);
}

#[test]
fn budgets_add_up() {
    const SUM: u32 = Step::Welcome.budget_s()
        + Step::Character.budget_s()
        + Step::Input.budget_s()
        + Step::Calibrate.budget_s()
        + Step::Output.budget_s()
        + Step::GoLive.budget_s();
    const _: () = assert!(TOTAL_BUDGET_S >= SUM);
    let each: Vec<u32> = Step::ALL.iter().map(|s| s.budget_s()).collect();
    assert_eq!(each, [5, 10, 20, 45, 20, 10]);
    assert_eq!(SUM, 110);
    assert_eq!(TOTAL_BUDGET_S, 120);
}

#[test]
fn step_ids_round_trip_and_unknown_is_none() {
    for s in Step::ALL {
        assert_eq!(Step::from_id(s.id()), Some(s));
    }
    assert_eq!(Step::from_id("nope"), None);
}

#[test]
fn every_probe_combination_fits() {
    let cameras = [
        Cameras::None,
        Cameras::One(Permission::Granted),
        Cameras::One(Permission::Denied),
        Cameras::One(Permission::Undecided),
        Cameras::Several(2),
    ];
    let trackers = [TrackerModel::Ready, TrackerModel::Downloading { percent: 30 }, TrackerModel::Unavailable];
    let outputs = [None, Some(OutputKind::Ndi)];
    for c in cameras {
        for t in trackers {
            for i in [IPhone::NotPaired, IPhone::Paired] {
                for o in outputs {
                    let p = Probes { cameras: c, tracker: t, iphone: i, recommended_output: o, ..probes() };
                    for input in [InputChoice::Webcam, InputChoice::Iphone, InputChoice::LifeLayerOnly] {
                        let mut f = flow(p);
                        to_input(&mut f);
                        f.handle(Event::ChooseInput(input), 0);
                        let sum: u32 = f.view().shown.iter().map(|s| s.budget_s()).sum();
                        assert!(sum <= TOTAL_BUDGET_S, "{p:?} {input:?}: {sum}");
                    }
                }
            }
        }
    }
}

#[test]
fn one_camera_has_no_picker_and_several_has_one() {
    let mut f = flow(probes());
    to_input(&mut f);
    assert!(!f.view().camera_picker);
    let mut f = flow(Probes { cameras: Cameras::Several(2), ..probes() });
    assert!(!f.view().camera_picker, "only the Input step has the picker");
    to_input(&mut f);
    assert!(f.view().camera_picker);
}

#[test]
fn no_camera_reaches_the_end_through_the_life_layer_and_lists_calibrate_as_skipped() {
    let mut f = flow(Probes { cameras: Cameras::None, ..probes() });
    to_input(&mut f);
    assert!(f.view().notes.contains(&Note::NoCamera));
    assert!(send(&mut f, &[Event::ChooseInput(InputChoice::Webcam)]).is_empty(), "webcam is not on offer");
    assert!(send(&mut f, &[Event::Next]).is_empty(), "no choice, no Next");
    send(&mut f, &[Event::ChooseInput(InputChoice::LifeLayerOnly), Event::Next]);
    assert_eq!(f.view().step, Step::Output);
    assert!(!f.view().shown.contains(&Step::Calibrate));
    assert_eq!(f.progress().skipped, [Step::Calibrate]);
}

#[test]
fn permission_denied_offers_the_other_paths() {
    let mut f = flow(Probes { cameras: Cameras::One(Permission::Denied), iphone: IPhone::Paired, ..probes() });
    to_input(&mut f);
    assert!(f.view().notes.contains(&Note::PermissionDenied));
    assert!(send(&mut f, &[Event::ChooseInput(InputChoice::Webcam), Event::Next]).is_empty());
    send(&mut f, &[Event::ChooseInput(InputChoice::Iphone), Event::Next]);
    assert_eq!(f.view().step, Step::Calibrate, "a paired iPhone calibrates without the webcam");
    let mut g = flow(Probes { cameras: Cameras::One(Permission::Denied), ..probes() });
    to_input(&mut g);
    send(&mut g, &[Event::ChooseInput(InputChoice::LifeLayerOnly), Event::Next]);
    assert_eq!(g.view().step, Step::Output);
}

#[test]
fn undecided_permission_is_asked_once_on_input_and_blocks_until_granted() {
    let mut f = flow(Probes { cameras: Cameras::One(Permission::Undecided), ..probes() });
    let fx = send(&mut f, &[Event::Next, Event::ChooseCharacter(CharacterChoice::Sample)]);
    assert!(!fx.contains(&Effect::RequestCameraPermission), "nothing is asked before Input");
    assert_eq!(send(&mut f, &[Event::ChooseInput(InputChoice::Webcam)]), [Effect::RequestCameraPermission]);
    assert!(send(&mut f, &[Event::ChooseInput(InputChoice::Webcam), Event::Next]).is_empty());
    f.handle(Event::Probed(probes()), 0);
    send(&mut f, &[Event::Next]);
    assert_eq!(f.view().step, Step::Calibrate);
}

#[test]
fn tracker_downloading_continues_with_life_layer() {
    let p = Probes { tracker: TrackerModel::Downloading { percent: 40 }, ..probes() };
    let mut f = flow(p);
    to_input(&mut f);
    assert!(f.view().notes.contains(&Note::TrackerDownloading(40)));
    send(&mut f, &[Event::ChooseInput(InputChoice::Webcam), Event::Next]);
    assert_eq!(f.view().step, Step::Output);
    assert_eq!(f.progress().skipped, [Step::Calibrate]);
    // The model arriving while the person is on Output lists Calibrate again, without moving them.
    f.handle(Event::Probed(probes()), 0);
    assert_eq!(f.view().step, Step::Output);
    assert!(f.view().shown.contains(&Step::Calibrate));
    assert!(f.progress().skipped.is_empty());
}

#[test]
fn iphone_calibrates_while_webcam_model_unavailable() {
    let mut f = flow(Probes { tracker: TrackerModel::Unavailable, iphone: IPhone::Paired, ..probes() });
    to_input(&mut f);
    assert!(f.view().notes.contains(&Note::TrackerUnavailable));
    send(&mut f, &[Event::ChooseInput(InputChoice::Iphone), Event::Next]);
    assert_eq!(f.view().step, Step::Calibrate);
}

#[test]
fn language_offer_note_only_on_a_traditional_system() {
    assert!(!flow(probes()).view().notes.contains(&Note::OfferSimplifiedChinese));
    let p = Probes { language: LanguageOffer::SimplifiedChineseOnTraditionalSystem, assistant: false, ..probes() };
    assert_eq!(flow(p).view().notes, [Note::NoAssistant, Note::OfferSimplifiedChinese]);
}

#[test]
fn back_lands_on_the_previous_shown_step() {
    let mut f = flow(Probes { cameras: Cameras::None, ..probes() });
    to_input(&mut f);
    send(&mut f, &[Event::ChooseInput(InputChoice::LifeLayerOnly), Event::Next]);
    assert_eq!(f.view().step, Step::Output);
    send(&mut f, &[Event::Back]);
    assert_eq!(f.view().step, Step::Input, "Calibrate is not shown, so Back skips it");
    send(&mut f, &[Event::Back, Event::Back]);
    assert_eq!(f.view().step, Step::Welcome);
    assert!(!f.view().can_back);
    assert!(send(&mut f, &[Event::Back]).is_empty());
}

#[test]
fn skip_setup_from_every_step_finishes_skipped_and_persists() {
    for step in Step::ALL {
        let mut f = SetupFlow::new(probes(), Progress { step, ..Progress::default() });
        let fx = f.handle(Event::SkipSetup, 7);
        let finished = Progress { step, skipped: Vec::new(), finished: true };
        assert_eq!(
            fx,
            [Effect::EnsureProject, Effect::Persist(finished), Effect::Finished { skipped: true, total_ms: 0 }],
            "{step:?}"
        );
        assert!(f.handle(Event::Next, 8).is_empty(), "finished flows ignore events");
    }
}

#[test]
fn resume_from_calibrate() {
    let f = SetupFlow::new(probes(), Progress { step: Step::Calibrate, ..Progress::default() });
    assert_eq!(f.view().step, Step::Calibrate);
    assert_eq!(f.view().position, Some((3, 5)));
    let g = SetupFlow::new(
        Probes { cameras: Cameras::None, ..probes() },
        Progress { step: Step::Calibrate, ..Progress::default() },
    );
    assert_eq!(g.view().step, Step::Output, "a step the probes leave out resumes at the next shown one");
}

#[test]
fn calibration_needs_a_start_and_skip_records_it() {
    let mut f = SetupFlow::new(probes(), Progress { step: Step::Calibrate, ..Progress::default() });
    assert!(send(&mut f, &[Event::CalibrationDone]).is_empty(), "never started");
    assert_eq!(send(&mut f, &[Event::Next]), [Effect::StartCalibration]);
    assert!(send(&mut f, &[Event::Next]).is_empty());
    let fx = send(&mut f, &[Event::CalibrationDone]);
    assert_eq!(fx[0], Effect::Show(Step::Output));
    let mut g = SetupFlow::new(probes(), Progress { step: Step::Calibrate, ..Progress::default() });
    send(&mut g, &[Event::Skip]);
    assert_eq!(g.progress().skipped, [Step::Calibrate]);
}

#[test]
fn input_skip_output_choice_and_finish_rules() {
    let mut f = flow(probes());
    assert!(send(&mut f, &[Event::Finish, Event::Skip, Event::ChooseOutput(OutputKind::Spout)]).is_empty());
    to_input(&mut f);
    send(&mut f, &[Event::Skip]);
    assert_eq!(f.progress().skipped, [Step::Input, Step::Calibrate]);
    assert_eq!(f.view().step, Step::Output);
    assert!(send(&mut f, &[Event::ChooseOutput(OutputKind::Ndi)]).is_empty(), "only the recommended output is offered");
    assert_eq!(send(&mut f, &[Event::ChooseOutput(OutputKind::Spout)]), [Effect::ShowTestPattern(OutputKind::Spout)]);
    send(&mut f, &[Event::Next]);
    assert!(send(&mut f, &[Event::Next]).is_empty());
    let fx = send(&mut f, &[Event::Finish]);
    let last = Progress { step: Step::GoLive, skipped: vec![Step::Input, Step::Calibrate], finished: true };
    assert_eq!(fx, [Effect::Persist(last), Effect::Finished { skipped: false, total_ms: 0 }]);
}

#[test]
fn keep_needs_an_open_project() {
    let mut f = flow(probes());
    send(&mut f, &[Event::Next]);
    assert!(send(&mut f, &[Event::ChooseCharacter(CharacterChoice::Keep)]).is_empty());
    let mut g = flow(Probes { project_open: true, ..probes() });
    send(&mut g, &[Event::Next, Event::ChooseCharacter(CharacterChoice::Keep)]);
    assert_eq!(g.view().step, Step::Input);
}

#[test]
fn no_recommended_output_leaves_the_step_out_and_probe_changes_never_jump() {
    let mut f = flow(Probes { recommended_output: None, ..probes() });
    assert_eq!(f.view().shown, [Step::Welcome, Step::Character, Step::Input, Step::Calibrate, Step::GoLive]);
    assert_eq!(f.progress().skipped, [Step::Output]);
    f.handle(Event::Probed(probes()), 0);
    assert_eq!(f.view().step, Step::Welcome);
    let mut g = SetupFlow::new(probes(), Progress { step: Step::Output, ..Progress::default() });
    g.handle(Event::Probed(Probes { recommended_output: None, ..probes() }), 0);
    assert_eq!(g.view().step, Step::Output);
    assert!(g.view().shown.contains(&Step::Output), "the current step stays listed");
}

fn alphabet() -> [Event; 12] {
    [
        Event::Next,
        Event::Back,
        Event::Skip,
        Event::SkipSetup,
        Event::ChooseCharacter(CharacterChoice::Sample),
        Event::ChooseInput(InputChoice::Webcam),
        Event::ChooseInput(InputChoice::LifeLayerOnly),
        Event::ChooseInput(InputChoice::Iphone),
        Event::CalibrationDone,
        Event::ChooseOutput(OutputKind::Spout),
        Event::Finish,
        Event::Probed(Probes { cameras: Cameras::None, recommended_output: None, ..probes() }),
    ]
}

fn check_valid(f: &SetupFlow) {
    let v = f.view();
    assert!(Step::ALL.contains(&v.step));
    assert!(v.shown.contains(&v.step));
    assert!(v.shown.windows(2).all(|w| w[0] < w[1]));
    assert_eq!(v.position.is_none(), v.step == Step::Welcome);
    assert_eq!(f.progress().step, v.step);
}

fn walk(f: &mut SetupFlow, seq: &[Event], out: &mut Vec<(Vec<Effect>, View)>) {
    for (i, e) in seq.iter().enumerate() {
        let fx = f.handle(*e, i as u64 * 10);
        check_valid(f);
        if let Some(Effect::Persist(p)) = fx.iter().rev().find(|e| matches!(e, Effect::Persist(_))) {
            assert_eq!(*p, f.progress(), "the last Persist is the current progress");
        }
        out.push((fx, f.view()));
    }
}

#[test]
fn exhaustive_short_sequences() {
    let a = alphabet();
    let n = a.len();
    for code in 0..n.pow(5) {
        let seq: Vec<Event> = (0..5).map(|i| a[code / n.pow(i) % n]).collect();
        let mut f = flow(probes());
        walk(&mut f, &seq, &mut Vec::new());
        // What was persisted can always be resumed.
        check_valid(&SetupFlow::new(probes(), f.progress()));
    }
}

#[test]
fn replaying_the_same_events_is_identical() {
    let seq = [
        Event::Next,
        Event::ChooseCharacter(CharacterChoice::Sample),
        Event::ChooseInput(InputChoice::Webcam),
        Event::Next,
        Event::Next,
        Event::CalibrationDone,
        Event::Back,
        Event::SkipSetup,
    ];
    let (mut a, mut b) = (Vec::new(), Vec::new());
    walk(&mut flow(probes()), &seq, &mut a);
    walk(&mut flow(probes()), &seq, &mut b);
    assert_eq!(a, b);
}

#[test]
fn happy_path_on_the_clock() {
    let run = |calibrate_ms: u64| {
        let mut f = flow(probes());
        let mut t = 0;
        let mut step = |f: &mut SetupFlow, ms: u64, events: &[Event]| {
            t += ms;
            events.iter().flat_map(|e| f.handle(*e, t)).collect::<Vec<_>>()
        };
        step(&mut f, 0, &[Event::Probed(probes())]);
        step(&mut f, 5_000, &[Event::Next]);
        step(&mut f, 10_000, &[Event::ChooseCharacter(CharacterChoice::Sample)]);
        step(&mut f, 20_000, &[Event::ChooseInput(InputChoice::Webcam), Event::Next]);
        step(&mut f, 1_000, &[Event::Next]);
        step(&mut f, calibrate_ms - 1_000, &[Event::CalibrationDone]);
        step(&mut f, 20_000, &[Event::ChooseOutput(OutputKind::Spout), Event::Next]);
        let fx = step(&mut f, 10_000, &[Event::Finish]);
        (fx, f)
    };
    let (fx, f) = run(45_000);
    assert_eq!(fx.last(), Some(&Effect::Finished { skipped: false, total_ms: 110_000 }));
    for s in Step::ALL {
        assert_eq!(f.timers().active_ms(s), u64::from(s.budget_s()) * 1000, "{s:?}");
    }
    let (fx, _) = run(135_000);
    assert_eq!(fx.last(), Some(&Effect::Finished { skipped: false, total_ms: 200_000 }), "the report says over 120 s");
}

#[test]
fn elapsed_is_per_sitting() {
    let mut f = flow(probes());
    f.handle(Event::Probed(probes()), 0);
    f.handle(Event::Next, 4_000);
    f.handle(Event::Back, 6_000);
    f.handle(Event::Next, 7_000);
    assert_eq!(f.timers().active_ms(Step::Welcome), 4_000 + 1_000);
    assert_eq!(f.timers().active_ms(Step::Character), 2_000);
    assert_eq!(f.view().elapsed_ms[Step::Welcome.index()], 5_000);
    let g = flow(probes());
    assert_eq!(g.timers().active_ms(Step::Welcome), 0, "a restart is a new flow with fresh timers");
}

#[test]
fn waiting_on_the_os_dialog_is_reported_apart() {
    let mut f = flow(Probes { cameras: Cameras::One(Permission::Undecided), ..probes() });
    f.handle(Event::Next, 0);
    f.handle(Event::ChooseCharacter(CharacterChoice::Sample), 1_000);
    f.handle(Event::ChooseInput(InputChoice::Webcam), 3_000);
    f.handle(Event::Probed(probes()), 13_000);
    assert_eq!(f.timers().active_ms(Step::Input), 12_000);
    assert_eq!(f.timers().waited_ms(Step::Input), 10_000);
    f.handle(Event::Next, 14_000);
    assert_eq!(f.timers().waited_ms(Step::Input), 10_000);
}

#[test]
fn memory_store_keeps_what_was_saved() {
    let store = MemoryStore::default();
    assert_eq!(store.load(), Progress::default());
    let p = Progress { step: Step::Output, skipped: vec![Step::Calibrate], finished: false };
    assert_eq!(store.save(&p), Ok(()));
    assert_eq!(store.load(), p);
}
