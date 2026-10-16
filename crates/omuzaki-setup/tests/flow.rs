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
    SetupFlow::new(p, Progress::default(), Current::default())
}

fn webcam() -> Current {
    Current { input: Some(InputChoice::Webcam), output: None }
}

/// Stands for "the run the host was last told to start" in a `CalibrationDone` or `CalibrationFailed`.
const LAST: u32 = u32::MAX;

thread_local! {
    static RUN: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

fn feed(f: &mut SetupFlow, e: Event, now_ms: u64) -> Vec<Effect> {
    let run = RUN.with(std::cell::Cell::get);
    let e = match e {
        Event::CalibrationDone { request: LAST } => Event::CalibrationDone { request: run },
        Event::CalibrationFailed { request: LAST } => Event::CalibrationFailed { request: run },
        e => e,
    };
    let fx = f.handle(e, now_ms);
    for x in &fx {
        if let Effect::StartCalibration { request } = x {
            RUN.with(|r| r.set(*request));
        }
    }
    fx
}

/// The host answers every operation at once, successfully; the effects of the answers follow the asking ones.
fn answered(f: &mut SetupFlow, mut fx: Vec<Effect>, now_ms: u64) -> Vec<Effect> {
    let mut i = 0;
    while i < fx.len() {
        let request = match fx[i] {
            Effect::SelectInput { request, .. }
            | Effect::ShowTestPattern { request, .. }
            | Effect::ApplyCharacter { request, .. } => Some(request),
            _ => None,
        };
        if let Some(request) = request {
            let more = feed(f, Event::Applied { request }, now_ms);
            fx.extend(more);
        }
        i += 1;
    }
    fx
}

/// Events with a host that answers everything at once.
fn send(f: &mut SetupFlow, events: &[Event]) -> Vec<Effect> {
    events
        .iter()
        .flat_map(|e| {
            let fx = feed(f, *e, 0);
            answered(f, fx, 0)
        })
        .collect()
}

/// The `request` of the first host operation among `fx`.
fn request_of(fx: &[Effect]) -> u32 {
    fx.iter()
        .find_map(|e| match e {
            Effect::SelectInput { request, .. }
            | Effect::ShowTestPattern { request, .. }
            | Effect::ApplyCharacter { request, .. }
            | Effect::StartCalibration { request } => Some(*request),
            _ => None,
        })
        // No operation among the effects: an id nothing owns, so the test fails on what it expected next.
        .unwrap_or(u32::MAX - 1)
}

/// Events with a host that answers nothing unless the test says so.
fn send_raw(f: &mut SetupFlow, events: &[Event]) -> Vec<Effect> {
    events.iter().flat_map(|e| feed(f, *e, 0)).collect()
}

fn no_effects() -> Vec<Effect> {
    Vec::new()
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
    assert_eq!(send(&mut f, &[Event::ChooseInput(InputChoice::Webcam)]), no_effects(), "webcam is not on offer");
    assert_eq!(send(&mut f, &[Event::Next]), no_effects(), "no choice, no Next");
    send(&mut f, &[Event::ChooseInput(InputChoice::LifeLayerOnly), Event::Applied { request: 0 }, Event::Next]);
    assert_eq!(f.view().step, Step::Output);
    assert!(!f.view().shown.contains(&Step::Calibrate));
    assert_eq!(f.progress().skipped, [Step::Calibrate]);
}

#[test]
fn permission_denied_offers_the_other_paths() {
    let mut f = flow(Probes { cameras: Cameras::One(Permission::Denied), iphone: IPhone::Paired, ..probes() });
    to_input(&mut f);
    assert!(f.view().notes.contains(&Note::PermissionDenied));
    assert_eq!(
        send(&mut f, &[Event::ChooseInput(InputChoice::Webcam), Event::Applied { request: 0 }, Event::Next]),
        no_effects()
    );
    send(&mut f, &[Event::ChooseInput(InputChoice::Iphone), Event::Applied { request: 0 }, Event::Next]);
    assert_eq!(f.view().step, Step::Calibrate, "a paired iPhone calibrates without the webcam");
    let mut g = flow(Probes { cameras: Cameras::One(Permission::Denied), ..probes() });
    to_input(&mut g);
    send(&mut g, &[Event::ChooseInput(InputChoice::LifeLayerOnly), Event::Applied { request: 0 }, Event::Next]);
    assert_eq!(g.view().step, Step::Output);
}

#[test]
fn undecided_permission_is_asked_once_on_input_and_blocks_until_granted() {
    let mut f = flow(Probes { cameras: Cameras::One(Permission::Undecided), ..probes() });
    let fx = send(&mut f, &[Event::Next, Event::ChooseCharacter(CharacterChoice::Sample)]);
    assert!(!fx.contains(&Effect::RequestCameraPermission), "nothing is asked before Input");
    let fx = send_raw(&mut f, &[Event::ChooseInput(InputChoice::Webcam)]);
    assert!(matches!(fx.as_slice(), [Effect::RequestCameraPermission, Effect::SelectInput { .. }]), "{fx:?}");
    let request = request_of(&fx);
    assert_eq!(send_raw(&mut f, &[Event::ChooseInput(InputChoice::Webcam), Event::Applied { request }]), no_effects());
    assert_eq!(send_raw(&mut f, &[Event::Next]), no_effects(), "the OS has not answered");
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
    send(&mut f, &[Event::ChooseInput(InputChoice::Webcam), Event::Applied { request: 0 }, Event::Next]);
    assert_eq!(f.view().step, Step::Output);
    assert_eq!(f.progress().skipped, [Step::Calibrate]);
    // The model arriving while the person is on Output lists Calibrate again, without moving them.
    f.handle(Event::Probed(probes()), 0);
    assert_eq!(f.view().step, Step::Output);
    assert!(f.view().shown.contains(&Step::Calibrate));
    assert_eq!(f.progress().skipped, Vec::<Step>::new());
}

#[test]
fn iphone_calibrates_while_webcam_model_unavailable() {
    let mut f = flow(Probes { tracker: TrackerModel::Unavailable, iphone: IPhone::Paired, ..probes() });
    to_input(&mut f);
    assert!(f.view().notes.contains(&Note::TrackerUnavailable));
    send(&mut f, &[Event::ChooseInput(InputChoice::Iphone), Event::Applied { request: 0 }, Event::Next]);
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
    send(&mut f, &[Event::ChooseInput(InputChoice::LifeLayerOnly), Event::Applied { request: 0 }, Event::Next]);
    assert_eq!(f.view().step, Step::Output);
    send(&mut f, &[Event::Back]);
    assert_eq!(f.view().step, Step::Input, "Calibrate is not shown, so Back skips it");
    send(&mut f, &[Event::Back, Event::Back]);
    assert_eq!(f.view().step, Step::Welcome);
    assert!(!f.view().can_back);
    assert_eq!(send(&mut f, &[Event::Back]), no_effects());
}

#[test]
fn skip_setup_from_every_step_finishes_skipped_and_persists() {
    for step in Step::ALL {
        let mut f = SetupFlow::new(probes(), Progress { step, ..Progress::default() }, webcam());
        let fx = f.handle(Event::SkipSetup, 7);
        let finished = Progress { step, skipped: Vec::new(), finished: true };
        assert_eq!(
            fx,
            [Effect::EnsureProject, Effect::Persist(finished), Effect::Finished { skipped: true, total_ms: 0 }],
            "{step:?}"
        );
        assert_eq!(f.handle(Event::Next, 8), no_effects(), "finished flows ignore events");
    }
}

#[test]
fn resume_from_calibrate() {
    let f = SetupFlow::new(probes(), Progress { step: Step::Calibrate, ..Progress::default() }, webcam());
    assert_eq!(f.view().step, Step::Calibrate);
    assert_eq!(f.view().position, Some((3, 5)));
    let g = SetupFlow::new(
        Probes { cameras: Cameras::None, ..probes() },
        Progress { step: Step::Calibrate, ..Progress::default() },
        webcam(),
    );
    assert_eq!(g.view().step, Step::Output, "a step the probes leave out resumes at the next shown one");
}

#[test]
fn calibration_needs_a_start_and_skip_records_it() {
    let mut f = SetupFlow::new(probes(), Progress { step: Step::Calibrate, ..Progress::default() }, webcam());
    assert_eq!(send(&mut f, &[Event::CalibrationDone { request: LAST }]), no_effects(), "never started");
    assert_eq!(send(&mut f, &[Event::Next]), [Effect::StartCalibration { request: RUN.with(std::cell::Cell::get) }]);
    assert_eq!(send(&mut f, &[Event::Next]), no_effects());
    let fx = send(&mut f, &[Event::CalibrationDone { request: LAST }]);
    assert_eq!(fx[1], Effect::Show(Step::Output));
    let mut g = SetupFlow::new(probes(), Progress { step: Step::Calibrate, ..Progress::default() }, webcam());
    send(&mut g, &[Event::Skip]);
    assert_eq!(g.progress().skipped, [Step::Calibrate]);
}

#[test]
fn input_skip_output_choice_and_finish_rules() {
    let mut f = flow(probes());
    assert_eq!(send(&mut f, &[Event::Finish, Event::Skip, Event::ChooseOutput(OutputKind::Spout)]), no_effects());
    to_input(&mut f);
    send(&mut f, &[Event::Skip]);
    assert_eq!(f.progress().skipped, [Step::Input, Step::Calibrate]);
    assert_eq!(f.view().step, Step::Output);
    assert_eq!(
        send(&mut f, &[Event::ChooseOutput(OutputKind::Ndi)]),
        no_effects(),
        "only the recommended output is offered"
    );
    let fx = send(&mut f, &[Event::ChooseOutput(OutputKind::Spout)]);
    assert!(matches!(fx.as_slice(), [Effect::ShowTestPattern { output: OutputKind::Spout, .. }]), "{fx:?}");
    send(&mut f, &[Event::Next]);
    assert_eq!(send(&mut f, &[Event::Next]), no_effects());
    let fx = send(&mut f, &[Event::Finish]);
    let last = Progress { step: Step::GoLive, skipped: vec![Step::Input, Step::Calibrate], finished: true };
    assert_eq!(fx, [Effect::Persist(last), Effect::Finished { skipped: false, total_ms: 0 }]);
}

#[test]
fn keep_needs_an_open_project() {
    let mut f = flow(probes());
    send(&mut f, &[Event::Next]);
    assert_eq!(send(&mut f, &[Event::ChooseCharacter(CharacterChoice::Keep)]), no_effects());
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
    let mut g = SetupFlow::new(probes(), Progress { step: Step::Output, ..Progress::default() }, Current::default());
    g.handle(Event::Probed(Probes { recommended_output: None, ..probes() }), 0);
    assert_eq!(g.view().step, Step::Output);
    assert!(g.view().shown.contains(&Step::Output), "the current step stays listed");
    assert_eq!(g.view().notes, [Note::OutputUnavailable], "the step stays and says why");
    send(&mut g, &[Event::Skip]);
    assert_eq!(g.view().step, Step::GoLive, "Later still moves on");
}

fn alphabet() -> [Event; 15] {
    [
        Event::Next,
        Event::Back,
        Event::Skip,
        Event::SkipSetup,
        Event::ChooseCharacter(CharacterChoice::Sample),
        Event::ChooseInput(InputChoice::Webcam),
        Event::ChooseInput(InputChoice::LifeLayerOnly),
        Event::ChooseInput(InputChoice::Iphone),
        Event::CalibrationDone { request: LAST },
        Event::ChooseOutput(OutputKind::Spout),
        Event::Finish,
        Event::Applied { request: 0 },
        Event::Refused { request: 0 },
        Event::Applied { request: 1 },
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
        let fx = feed(f, *e, i as u64 * 10);
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
        check_valid(&SetupFlow::new(probes(), f.progress(), Current::default()));
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
        Event::CalibrationDone { request: LAST },
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
            events
                .iter()
                .flat_map(|e| {
                    let fx = feed(f, *e, t);
                    answered(f, fx, t)
                })
                .collect::<Vec<_>>()
        };
        step(&mut f, 0, &[Event::Probed(probes())]);
        step(&mut f, 5_000, &[Event::Next]);
        step(&mut f, 10_000, &[Event::ChooseCharacter(CharacterChoice::Sample)]);
        step(&mut f, 20_000, &[Event::ChooseInput(InputChoice::Webcam), Event::Next]);
        step(&mut f, 1_000, &[Event::Next]);
        step(&mut f, calibrate_ms - 1_000, &[Event::CalibrationDone { request: LAST }]);
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
    let fx = f.handle(Event::ChooseCharacter(CharacterChoice::Sample), 1_000);
    f.handle(Event::Applied { request: request_of(&fx) }, 1_000);
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

fn persist_index(fx: &[Effect]) -> Option<usize> {
    fx.iter().position(|e| matches!(e, Effect::Persist(_)))
}

#[test]
fn a_fresh_run_preselects_the_webcam_and_continue_accepts_it() {
    let mut f = flow(probes());
    to_input(&mut f);
    assert_eq!(f.view().input, Some(InputChoice::Webcam));
    send(&mut f, &[Event::Next, Event::Applied { request: 0 }]);
    assert_eq!(f.view().step, Step::Calibrate);
    let g = flow(Probes { cameras: Cameras::None, ..probes() });
    assert_eq!(g.view().input, None, "nothing to preselect without a camera");
    let d = flow(Probes { cameras: Cameras::One(Permission::Denied), ..probes() });
    assert_eq!(d.view().input, None);
}

#[test]
fn a_preselected_webcam_asks_the_os_on_continue() {
    let mut f = flow(Probes { cameras: Cameras::One(Permission::Undecided), ..probes() });
    to_input(&mut f);
    let fx = send_raw(&mut f, &[Event::Next]);
    assert!(matches!(fx.as_slice(), [Effect::RequestCameraPermission, Effect::SelectInput { .. }]), "{fx:?}");
    assert_eq!(send_raw(&mut f, &[Event::Next]), no_effects());
    assert_eq!(f.timers().waited_ms(Step::Welcome), 0);
}

#[test]
fn a_rerun_keeps_the_current_input_and_output() {
    let p = Probes { iphone: IPhone::Paired, project_open: true, ..probes() };
    let current = Current { input: Some(InputChoice::Iphone), output: Some(OutputKind::Spout) };
    let mut f = SetupFlow::new(p, Progress::default(), current);
    assert_eq!(f.view().input, Some(InputChoice::Iphone), "the hardware does not override the setting");
    send(&mut f, &[Event::Next, Event::ChooseCharacter(CharacterChoice::Keep), Event::Next, Event::Skip]);
    assert_eq!(f.view().step, Step::Output);
    assert_eq!(f.view().output, Some(OutputKind::Spout));
    send(&mut f, &[Event::Next]);
    assert_eq!(f.view().step, Step::GoLive, "Continue accepts the current output with no write");
}

#[test]
fn a_resume_after_input_keeps_the_supplied_source() {
    let p = Probes { iphone: IPhone::Paired, ..probes() };
    let resume = Progress { step: Step::Calibrate, ..Progress::default() };
    let current = Current { input: Some(InputChoice::Iphone), output: Some(OutputKind::Ndi) };
    let f = SetupFlow::new(p, resume.clone(), current);
    assert_eq!((f.view().input, f.view().output), (Some(InputChoice::Iphone), Some(OutputKind::Ndi)));
    let g = SetupFlow::new(probes(), Progress { skipped: vec![Step::Input], ..resume }, Current::default());
    assert_eq!(g.view().input, Some(InputChoice::LifeLayerOnly));
}

#[test]
fn a_resume_moved_by_the_probes_is_persisted_by_the_first_event() {
    for step in [Step::Calibrate, Step::Output] {
        let p = Probes { cameras: Cameras::None, recommended_output: None, ..probes() };
        let mut f = SetupFlow::new(p, Progress { step, ..Progress::default() }, Current::default());
        let moved = f.view().step;
        assert_ne!(moved, step);
        let fx = f.handle(Event::Probed(p), 0);
        assert_eq!(fx, [Effect::Persist(f.progress())], "{step:?}");
        assert_eq!(f.progress().step, moved);
    }
}

#[test]
fn a_step_is_persisted_before_it_is_shown() {
    let mut f = flow(probes());
    let fx = send(&mut f, &[Event::Next]);
    assert_eq!(fx.last(), Some(&Effect::Show(Step::Character)));
    assert_eq!(persist_index(&fx), Some(fx.len() - 2));
    let fx = send(&mut f, &[Event::ChooseCharacter(CharacterChoice::Sample)]);
    assert!(
        matches!(fx.as_slice(), [Effect::ApplyCharacter { .. }, Effect::Persist(p), Effect::Show(Step::Input)] if p.step == Step::Input),
        "{fx:?}"
    );
    let fx = send(&mut f, &[Event::Back]);
    assert!(matches!(fx.as_slice(), [Effect::Persist(_), Effect::Show(Step::Character)]));
}

#[test]
fn an_unpaired_iphone_blocks_continue_with_a_note() {
    let mut f = flow(probes());
    to_input(&mut f);
    send(&mut f, &[Event::ChooseInput(InputChoice::Iphone)]);
    assert!(f.view().notes.contains(&Note::IphoneNotPaired));
    assert_eq!(send(&mut f, &[Event::Next]), no_effects());
    assert_eq!(f.view().step, Step::Input);
    f.handle(Event::Probed(Probes { iphone: IPhone::Paired, ..probes() }), 0);
    assert!(!f.view().notes.contains(&Note::IphoneNotPaired));
    send(&mut f, &[Event::Applied { request: 0 }, Event::Next]);
    assert_eq!(f.view().step, Step::Calibrate);
}

#[test]
fn back_to_a_finished_calibration_can_continue() {
    let mut f = SetupFlow::new(probes(), Progress { step: Step::Calibrate, ..Progress::default() }, webcam());
    send(&mut f, &[Event::Next, Event::CalibrationDone { request: LAST }]);
    assert_eq!(f.view().step, Step::Output);
    send(&mut f, &[Event::Back]);
    assert_eq!(f.view().step, Step::Calibrate);
    assert_eq!(f.progress().skipped, Vec::<Step>::new(), "the stored result still counts");
    send(&mut f, &[Event::Next]);
    assert_eq!(f.view().step, Step::Output);
}

#[test]
fn leaving_a_running_calibration_or_changing_source_resets_it() {
    let mut f = SetupFlow::new(probes(), Progress { step: Step::Calibrate, ..Progress::default() }, webcam());
    send(&mut f, &[Event::Next, Event::Back]);
    assert_eq!(f.view().step, Step::Input);
    send(&mut f, &[Event::Next]);
    assert_eq!(
        send(&mut f, &[Event::Next]),
        [Effect::StartCalibration { request: RUN.with(std::cell::Cell::get) }],
        "a new run can start"
    );
    send(&mut f, &[Event::CalibrationDone { request: LAST }, Event::Back, Event::Back]);
    send(&mut f, &[Event::ChooseInput(InputChoice::LifeLayerOnly)]);
    assert_eq!(f.progress().skipped, [Step::Calibrate], "a result for another source is not kept");
}

#[test]
fn losing_the_source_on_calibrate_explains_why_without_moving() {
    let at_calibrate = |p: Probes, current: Current| {
        SetupFlow::new(probes(), Progress { step: Step::Calibrate, ..Progress::default() }, current).tap(p)
    };
    let f = at_calibrate(Probes { cameras: Cameras::None, ..probes() }, webcam());
    assert_eq!((f.view().step, f.view().notes), (Step::Calibrate, vec![Note::NoCamera]));
    let f = at_calibrate(Probes { cameras: Cameras::One(Permission::Denied), ..probes() }, webcam());
    assert_eq!(f.view().notes, [Note::PermissionDenied]);
    let f = at_calibrate(Probes { tracker: TrackerModel::Unavailable, ..probes() }, webcam());
    assert_eq!(f.view().notes, [Note::TrackerUnavailable]);
    let f = at_calibrate(Probes { tracker: TrackerModel::Downloading { percent: 9 }, ..probes() }, webcam());
    assert_eq!(f.view().notes, [Note::TrackerDownloading(9)]);
    let phone = Current { input: Some(InputChoice::Iphone), output: None };
    let paired = Probes { iphone: IPhone::Paired, ..probes() };
    let mut f = SetupFlow::new(paired, Progress { step: Step::Calibrate, ..Progress::default() }, phone);
    f.handle(Event::Probed(probes()), 0);
    assert_eq!((f.view().step, f.view().notes), (Step::Calibrate, vec![Note::IphoneNotPaired]));
}

trait Tap {
    fn tap(self, p: Probes) -> Self;
}

impl Tap for SetupFlow {
    fn tap(mut self, p: Probes) -> Self {
        self.handle(Event::Probed(p), 0);
        self
    }
}

#[test]
fn calibrate_is_skipped_when_no_source_can_be_chosen() {
    let none = Probes { cameras: Cameras::None, ..probes() };
    assert_eq!(flow(none).progress().skipped, [Step::Calibrate]);
    let mut f = flow(none);
    let fx = f.handle(Event::SkipSetup, 0);
    assert!(fx.contains(&Effect::Persist(Progress {
        step: Step::Welcome,
        skipped: vec![Step::Calibrate],
        finished: true
    })));
    // While a source can still be chosen, the person has not given Calibrate up.
    let mut g = SetupFlow::new(probes(), Progress { step: Step::Output, ..Progress::default() }, Current::default());
    assert_eq!(g.handle(Event::SkipSetup, 0).iter().filter(|e| matches!(e, Effect::Persist(_))).count(), 1);
    assert_eq!(g.progress().skipped, Vec::<Step>::new());
}

#[test]
fn store_error_carries_a_reason() {
    let e = StoreError::new("disk full");
    assert_eq!(e.reason(), "disk full");
    assert_eq!(e.to_string(), "setup progress store: disk full");
}

#[test]
fn skipping_input_after_calibrating_discards_the_calibration() {
    let mut f = SetupFlow::new(probes(), Progress { step: Step::Calibrate, ..Progress::default() }, webcam());
    send(&mut f, &[Event::Next, Event::CalibrationDone { request: LAST }, Event::Back, Event::Back]);
    assert_eq!(f.view().step, Step::Input);
    assert_eq!(f.progress().skipped, Vec::<Step>::new());
    // The webcam is applied, so dropping it goes through the host and the step waits for the answer.
    let fx = send_raw(&mut f, &[Event::Skip]);
    assert!(matches!(fx[0], Effect::SelectInput { source: InputChoice::LifeLayerOnly, .. }), "{fx:?}");
    assert_eq!(f.view().step, Step::Input);
    assert!(!f.progress().skipped.contains(&Step::Input), "not skipped until the host applied it");
    let fx = send_raw(&mut f, &[Event::Applied { request: request_of(&fx) }]);
    assert_eq!(f.view().input, Some(InputChoice::LifeLayerOnly));
    assert_eq!(f.progress().skipped, [Step::Input, Step::Calibrate]);
    assert!(fx.contains(&Effect::Persist(f.progress())));
    // Back to the same webcam keeps nothing either: the run belonged to the discarded source.
    send(&mut f, &[Event::Back, Event::ChooseInput(InputChoice::Webcam), Event::Next]);
    assert_eq!(f.view().step, Step::Calibrate);
    let fx = send(&mut f, &[Event::Next]);
    assert!(matches!(fx.as_slice(), [Effect::StartCalibration { .. }]), "{fx:?}");
}

#[test]
fn skipping_calibrate_after_a_result_records_the_skip() {
    let mut f = SetupFlow::new(probes(), Progress { step: Step::Calibrate, ..Progress::default() }, webcam());
    send(&mut f, &[Event::Next, Event::CalibrationDone { request: LAST }, Event::Back, Event::Skip]);
    assert_eq!(f.progress().skipped, [Step::Calibrate]);
    assert_eq!(f.progress().step, Step::Output);
}

fn selects(fx: &[Effect]) -> usize {
    fx.iter().filter(|e| matches!(e, Effect::SelectInput { .. })).count()
}

#[test]
fn continue_on_a_preselected_source_applies_it_and_waits_for_the_host() {
    let mut f = flow(probes());
    to_input(&mut f);
    // Pending: the op may be queued for the person's approval, so the step does not move.
    let fx = send_raw(&mut f, &[Event::Next]);
    assert!(matches!(fx.as_slice(), [Effect::SelectInput { source: InputChoice::Webcam, .. }]), "{fx:?}");
    assert_eq!(f.view().step, Step::Input);
    assert_eq!(f.progress().step, Step::Input);
    assert_eq!(send_raw(&mut f, &[Event::Next]), no_effects(), "asked once, not again");
    let fx = send_raw(&mut f, &[Event::Applied { request: request_of(&fx) }]);
    assert!(
        matches!(fx.as_slice(), [Effect::Persist(p), Effect::Show(Step::Calibrate)] if p.step == Step::Calibrate),
        "{fx:?}"
    );
    send(&mut f, &[Event::Back]);
    assert_eq!(selects(&send(&mut f, &[Event::Next])), 0, "already applied");
    assert_eq!(f.view().step, Step::Calibrate);
}

#[test]
fn a_choice_waits_for_the_host_too_and_applied_alone_does_not_move_the_person() {
    let mut f = flow(probes());
    to_input(&mut f);
    let fx = send_raw(&mut f, &[Event::ChooseInput(InputChoice::Webcam)]);
    assert!(matches!(fx.as_slice(), [Effect::SelectInput { source: InputChoice::Webcam, .. }]), "{fx:?}");
    assert_eq!(send_raw(&mut f, &[Event::ChooseInput(InputChoice::Webcam)]), no_effects());
    assert_eq!(send_raw(&mut f, &[Event::Next]), no_effects(), "not applied yet");
    assert_eq!(f.view().step, Step::Input);
    send_raw(&mut f, &[Event::Applied { request: request_of(&fx) }]);
    assert_eq!(f.view().step, Step::Calibrate, "Continue was pressed, so it moves on");
    let mut g = flow(probes());
    to_input(&mut g);
    let fx = send_raw(&mut g, &[Event::ChooseInput(InputChoice::Webcam)]);
    send_raw(&mut g, &[Event::Applied { request: request_of(&fx) }]);
    assert_eq!(g.view().step, Step::Input, "an answer alone is not a Continue");
    // An answer for a source the person has since left changes nothing.
    let fx = send_raw(&mut g, &[Event::ChooseInput(InputChoice::LifeLayerOnly)]);
    send_raw(&mut g, &[Event::Applied { request: request_of(&fx) }, Event::Next]);
    assert_eq!(g.view().step, Step::Output, "the life layer was applied, so Continue moves on");
}

#[test]
fn a_refused_source_keeps_the_step_with_a_note_and_continue_asks_again() {
    let mut f = flow(probes());
    to_input(&mut f);
    let fx = send_raw(&mut f, &[Event::Next]);
    send_raw(&mut f, &[Event::Refused { request: request_of(&fx) }]);
    assert_eq!(f.view().step, Step::Input);
    assert!(f.view().notes.contains(&Note::InputNotApplied));
    let again = send_raw(&mut f, &[Event::Next]);
    assert!(matches!(again.as_slice(), [Effect::SelectInput { source: InputChoice::Webcam, .. }]), "{again:?}");
    assert_ne!(request_of(&again), request_of(&fx), "a new request");
    assert!(!f.view().notes.contains(&Note::InputNotApplied));
    send_raw(&mut f, &[Event::Applied { request: request_of(&again) }]);
    assert_eq!(f.view().step, Step::Calibrate);
}

#[test]
fn a_source_from_the_settings_is_not_applied_again() {
    let mut f = SetupFlow::new(probes(), Progress::default(), webcam());
    to_input(&mut f);
    assert_eq!(selects(&send(&mut f, &[Event::Next])), 0);
    assert_eq!(f.view().step, Step::Calibrate);
}

#[test]
fn skip_on_input_moves_on_at_once_without_an_op() {
    let mut f = flow(probes());
    to_input(&mut f);
    let fx = send(&mut f, &[Event::Skip]);
    assert_eq!(selects(&fx), 0, "the life layer is the default and writes nothing");
    assert_eq!(f.view().step, Step::Output);
}

#[test]
fn the_camera_wait_stops_when_the_source_changes_or_the_step_moves_on() {
    let mut f = flow(Probes { cameras: Cameras::One(Permission::Undecided), iphone: IPhone::Paired, ..probes() });
    let ok = |f: &mut SetupFlow, e: Event, t: u64| {
        let fx = feed(f, e, t);
        answered(f, fx, t);
    };
    ok(&mut f, Event::Next, 0);
    ok(&mut f, Event::ChooseCharacter(CharacterChoice::Sample), 0);
    ok(&mut f, Event::ChooseInput(InputChoice::Webcam), 0);
    ok(&mut f, Event::ChooseInput(InputChoice::Iphone), 1_000);
    assert_eq!(f.timers().waited_ms(Step::Input), 1_000);
    ok(&mut f, Event::Next, 2_000);
    assert_eq!(f.view().step, Step::Calibrate);
    ok(&mut f, Event::Skip, 12_000);
    ok(&mut f, Event::Next, 22_000);
    for s in [Step::Calibrate, Step::Output, Step::GoLive] {
        assert_eq!(f.timers().waited_ms(s), 0, "{s:?}");
    }
    // The dialog counts only while Input is showing.
    let mut g = flow(Probes { cameras: Cameras::One(Permission::Undecided), ..probes() });
    ok(&mut g, Event::Next, 0);
    ok(&mut g, Event::ChooseCharacter(CharacterChoice::Sample), 0);
    ok(&mut g, Event::Next, 0);
    ok(&mut g, Event::Back, 5_000);
    assert_eq!(g.timers().waited_ms(Step::Input), 5_000, "the dialog is open while Input shows");
    ok(&mut g, Event::Back, 6_000);
    ok(&mut g, Event::Next, 9_000);
    assert_eq!(g.timers().waited_ms(Step::Character), 0);
}

#[test]
fn without_a_camera_continue_stays_on_input() {
    let mut f = flow(Probes { cameras: Cameras::None, ..probes() });
    to_input(&mut f);
    assert_eq!(send(&mut f, &[Event::Next]), no_effects());
    assert_eq!(f.view().step, Step::Input);
}

#[test]
fn a_saved_webcam_with_the_permission_undecided_asks_the_os_on_continue() {
    let p = Probes { cameras: Cameras::One(Permission::Undecided), ..probes() };
    let mut f = SetupFlow::new(p, Progress::default(), webcam());
    to_input(&mut f);
    assert_eq!(send(&mut f, &[Event::Next]), [Effect::RequestCameraPermission], "no SelectInput: it is saved");
    assert_eq!(send(&mut f, &[Event::Next]), no_effects(), "asked once");
    f.handle(Event::Probed(probes()), 0);
    send(&mut f, &[Event::Next]);
    assert_eq!(f.view().step, Step::Calibrate);
}

#[test]
fn skip_on_input_with_nothing_applied_moves_on_at_once() {
    let mut f = flow(probes());
    to_input(&mut f);
    let fx = send(&mut f, &[Event::Skip]);
    assert_eq!(selects(&fx), 0);
    assert_eq!(f.view().step, Step::Output);
    assert_eq!(f.progress().skipped, [Step::Input, Step::Calibrate]);
}

#[test]
fn a_refused_skip_keeps_the_step_and_does_not_record_it() {
    let mut f = SetupFlow::new(probes(), Progress::default(), webcam());
    to_input(&mut f);
    let fx = send_raw(&mut f, &[Event::Skip]);
    send_raw(&mut f, &[Event::Refused { request: request_of(&fx) }]);
    assert_eq!(f.view().step, Step::Input);
    assert!(f.view().notes.contains(&Note::InputNotApplied));
    assert!(!f.progress().skipped.contains(&Step::Input));
}

#[test]
fn answers_are_matched_to_their_request_when_choices_pile_up() {
    // Webcam, then iPhone, then Webcam again, then Continue, with the host answering late.
    let p = Probes { iphone: IPhone::Paired, ..probes() };
    let mut f = flow(p);
    to_input(&mut f);
    let first = send_raw(&mut f, &[Event::ChooseInput(InputChoice::Webcam)]);
    assert!(matches!(first.as_slice(), [Effect::SelectInput { source: InputChoice::Webcam, .. }]), "{first:?}");
    let queued = send_raw(&mut f, &[Event::ChooseInput(InputChoice::Iphone), Event::ChooseInput(InputChoice::Webcam)]);
    assert_eq!(selects(&queued), 0, "one request at a time");
    send_raw(&mut f, &[Event::Next]);
    // A made-up or old id changes nothing.
    assert_eq!(send_raw(&mut f, &[Event::Applied { request: 999 }]), no_effects());
    // The host applies Webcam: that is what the person ends on, so Continue may go on.
    let request = request_of(&first);
    let fx = send_raw(&mut f, &[Event::Applied { request }]);
    assert!(fx.contains(&Effect::Show(Step::Calibrate)), "{fx:?}");
    assert_eq!(send_raw(&mut f, &[Event::Applied { request }]), no_effects(), "answered once");
}

#[test]
fn a_superseded_selection_is_followed_by_the_latest_one() {
    let p = Probes { iphone: IPhone::Paired, ..probes() };
    let mut f = flow(p);
    to_input(&mut f);
    let first = send_raw(&mut f, &[Event::ChooseInput(InputChoice::Webcam)]);
    send_raw(&mut f, &[Event::ChooseInput(InputChoice::Iphone), Event::Next]);
    // Webcam lands, but the person wants the iPhone: it is requested next, and Continue waits for it.
    let fx = send_raw(&mut f, &[Event::Applied { request: request_of(&first) }]);
    assert!(matches!(fx.as_slice(), [Effect::SelectInput { source: InputChoice::Iphone, .. }]), "{fx:?}");
    assert_eq!(f.view().step, Step::Input);
    // Rejecting the last request leaves the step with a note, and nothing else is asked.
    assert_eq!(send_raw(&mut f, &[Event::Refused { request: request_of(&fx) }]), no_effects());
    assert_eq!(f.view().step, Step::Input);
    assert!(f.view().notes.contains(&Note::InputNotApplied));
    // A rejection of a source the person already left asks for the latest one instead.
    let mut g = flow(p);
    to_input(&mut g);
    let first = send_raw(&mut g, &[Event::ChooseInput(InputChoice::Webcam)]);
    send_raw(&mut g, &[Event::ChooseInput(InputChoice::Iphone)]);
    let fx = send_raw(&mut g, &[Event::Refused { request: request_of(&first) }]);
    assert!(matches!(fx.as_slice(), [Effect::SelectInput { source: InputChoice::Iphone, .. }]), "{fx:?}");
    assert!(!g.view().notes.contains(&Note::InputNotApplied));
    send_raw(&mut g, &[Event::Applied { request: request_of(&fx) }, Event::Next]);
    assert_eq!(g.view().step, Step::Calibrate);
}

#[test]
fn clicking_the_saved_webcam_still_asks_the_os() {
    let p = Probes { cameras: Cameras::One(Permission::Undecided), ..probes() };
    let mut f = SetupFlow::new(p, Progress::default(), webcam());
    to_input(&mut f);
    assert_eq!(send(&mut f, &[Event::ChooseInput(InputChoice::Webcam)]), [Effect::RequestCameraPermission]);
    assert_eq!(send(&mut f, &[Event::ChooseInput(InputChoice::Webcam)]), no_effects(), "asked once");
}

fn at_output() -> SetupFlow {
    SetupFlow::new(probes(), Progress { step: Step::Output, ..Progress::default() }, webcam())
}

#[test]
fn an_output_counts_only_after_the_host_added_it() {
    let mut f = at_output();
    // Next before choosing does nothing.
    assert_eq!(send_raw(&mut f, &[Event::Next]), no_effects());
    // Choosing asks the host; while the Confirm is queued the step does not move.
    let fx = send_raw(&mut f, &[Event::ChooseOutput(OutputKind::Spout)]);
    assert!(matches!(fx.as_slice(), [Effect::ShowTestPattern { output: OutputKind::Spout, .. }]), "{fx:?}");
    assert_eq!(f.view().output, Some(OutputKind::Spout), "drawn as selected");
    assert_eq!(send_raw(&mut f, &[Event::Next]), no_effects(), "Next before the answer");
    assert_eq!(f.view().step, Step::Output);
    assert_eq!(send_raw(&mut f, &[Event::ChooseOutput(OutputKind::Spout)]), no_effects(), "asked once");
    // The answer moves on, since Continue was pressed.
    let fx = send_raw(&mut f, &[Event::Applied { request: request_of(&fx) }]);
    assert!(matches!(fx.as_slice(), [Effect::Persist(_), Effect::Show(Step::GoLive)]), "{fx:?}");
}

#[test]
fn next_after_the_answer_moves_on() {
    let mut f = at_output();
    let fx = send_raw(&mut f, &[Event::ChooseOutput(OutputKind::Spout)]);
    send_raw(&mut f, &[Event::Applied { request: request_of(&fx) }]);
    assert_eq!(f.view().step, Step::Output, "an answer alone is not a Continue");
    send_raw(&mut f, &[Event::Next]);
    assert_eq!(f.view().step, Step::GoLive);
}

#[test]
fn a_failed_test_pattern_or_refused_add_stays_on_output_and_can_be_retried() {
    let mut f = at_output();
    let fx = send_raw(&mut f, &[Event::ChooseOutput(OutputKind::Spout), Event::Next]);
    send_raw(&mut f, &[Event::Refused { request: request_of(&fx) }]);
    assert_eq!(f.view().step, Step::Output);
    assert!(f.view().notes.contains(&Note::OutputNotApplied));
    assert_eq!(send_raw(&mut f, &[Event::Applied { request: request_of(&fx) }]), no_effects(), "answered once");
    // Next asks again and waits; a second refusal keeps the step.
    let again = send_raw(&mut f, &[Event::Next]);
    assert!(matches!(again.as_slice(), [Effect::ShowTestPattern { .. }]), "{again:?}");
    assert_ne!(request_of(&again), request_of(&fx));
    send_raw(&mut f, &[Event::Refused { request: request_of(&again) }]);
    assert_eq!(f.view().step, Step::Output);
    // Choosing again, then success.
    let third = send_raw(&mut f, &[Event::ChooseOutput(OutputKind::Spout)]);
    send_raw(&mut f, &[Event::Applied { request: request_of(&third) }, Event::Next]);
    assert_eq!(f.view().step, Step::GoLive);
}

#[test]
fn an_output_from_the_settings_needs_no_request_and_later_is_not_undone_by_an_answer() {
    let current = Current { input: Some(InputChoice::Webcam), output: Some(OutputKind::Spout) };
    let mut f = SetupFlow::new(probes(), Progress { step: Step::Output, ..Progress::default() }, current);
    assert_eq!(send_raw(&mut f, &[Event::Next]), [persist_show_golive(&f)].concat());
    // A late "Later": the answer arriving afterwards still lists the output as added.
    let mut g = at_output();
    let fx = send_raw(&mut g, &[Event::ChooseOutput(OutputKind::Spout), Event::Skip]);
    assert_eq!(g.view().step, Step::GoLive);
    assert!(g.progress().skipped.contains(&Step::Output));
    send_raw(&mut g, &[Event::Applied { request: request_of(&fx) }]);
    assert!(!g.progress().skipped.contains(&Step::Output), "it was added after all");
    assert_eq!(g.view().step, Step::GoLive, "no move from an answer for a left step");
}

fn persist_show_golive(f: &SetupFlow) -> Vec<Effect> {
    let progress = Progress { step: Step::GoLive, ..f.progress() };
    vec![Effect::Persist(progress), Effect::Show(Step::GoLive)]
}

#[test]
fn a_character_choice_waits_for_the_host_and_keep_does_not() {
    let mut f = flow(probes());
    send(&mut f, &[Event::Next]);
    let fx = send_raw(&mut f, &[Event::ChooseCharacter(CharacterChoice::ImportVrm)]);
    assert!(matches!(fx.as_slice(), [Effect::ApplyCharacter { choice: CharacterChoice::ImportVrm, .. }]), "{fx:?}");
    assert_eq!(f.view().step, Step::Character);
    assert_eq!(send_raw(&mut f, &[Event::ChooseCharacter(CharacterChoice::Sample)]), no_effects(), "one at a time");
    send_raw(&mut f, &[Event::Refused { request: request_of(&fx) }]);
    assert_eq!(f.view().step, Step::Character);
    assert!(f.view().notes.contains(&Note::CharacterNotApplied));
    let again = send_raw(&mut f, &[Event::ChooseCharacter(CharacterChoice::Sample)]);
    assert!(!f.view().notes.contains(&Note::CharacterNotApplied));
    let fx = send_raw(&mut f, &[Event::Applied { request: request_of(&again) }]);
    assert!(matches!(fx.as_slice(), [Effect::Persist(_), Effect::Show(Step::Input)]), "{fx:?}");
    // Keep writes nothing.
    let mut g = flow(Probes { project_open: true, ..probes() });
    send(&mut g, &[Event::Next]);
    let fx = send_raw(&mut g, &[Event::ChooseCharacter(CharacterChoice::Keep)]);
    assert!(matches!(fx.as_slice(), [Effect::Persist(_), Effect::Show(Step::Input)]), "{fx:?}");
}

#[test]
fn a_late_character_answer_does_not_move_a_person_who_went_back() {
    let mut f = flow(probes());
    send(&mut f, &[Event::Next]);
    let fx = send_raw(&mut f, &[Event::ChooseCharacter(CharacterChoice::Sample), Event::Back]);
    send_raw(&mut f, &[Event::Applied { request: request_of(&fx) }]);
    assert_eq!(f.view().step, Step::Welcome);
}

#[test]
fn a_calibration_result_belongs_to_its_run_and_a_failed_run_can_be_started_again() {
    let mut f = SetupFlow::new(probes(), Progress { step: Step::Calibrate, ..Progress::default() }, webcam());
    let first = send_raw(&mut f, &[Event::Next]);
    let one = request_of(&first);
    assert_eq!(send_raw(&mut f, &[Event::Next]), no_effects(), "running");
    send_raw(&mut f, &[Event::CalibrationFailed { request: one }]);
    assert_eq!(f.view().step, Step::Calibrate);
    assert!(f.view().notes.contains(&Note::CalibrationNotDone));
    let second = send_raw(&mut f, &[Event::Next]);
    let two = request_of(&second);
    assert_ne!(one, two);
    assert!(!f.view().notes.contains(&Note::CalibrationNotDone));
    // The first run's result arriving now is not the second run's.
    assert_eq!(send_raw(&mut f, &[Event::CalibrationDone { request: one }]), no_effects());
    assert_eq!(f.view().step, Step::Calibrate);
    // Leaving and coming back abandons the run; its late result is ignored too.
    send_raw(&mut f, &[Event::Back, Event::Next]);
    assert_eq!(f.view().step, Step::Calibrate);
    assert_eq!(send_raw(&mut f, &[Event::CalibrationDone { request: two }]), no_effects());
    let third = send_raw(&mut f, &[Event::Next]);
    let fx = send_raw(&mut f, &[Event::CalibrationDone { request: request_of(&third) }]);
    assert!(fx.contains(&Effect::Show(Step::Output)), "{fx:?}");
}
