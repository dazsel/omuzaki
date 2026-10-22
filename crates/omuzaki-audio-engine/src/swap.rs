//! Structural swaps on the audio thread (design section 3.3, path 2): the running [`Mix`] and the one waiting to
//! replace it.
//!
//! A new graph is offered at a block start. The buses it changes or drops fade out in the running graph over
//! [`FADE`](crate::mix::FADE) samples; at the sample the last of them is silent, the new graph adopts the old one's
//! nodes and state and runs the rest of the block, its changed buses fading in. The old graph is handed back to be
//! freed off the audio thread. Nothing here allocates or frees.

use omuzaki_audio_api::graph::MicMode;

use crate::mix::{Inputs, Mix, Points};

/// The graph the audio thread runs, and the one waiting to take over.
pub(crate) struct Running {
    pub graph: Box<Mix>,
    pending: Option<Box<Mix>>,
}

impl Running {
    pub(crate) fn new(graph: Box<Mix>) -> Self {
        Self { graph, pending: None }
    }

    /// Offers `next` to replace the running graph. Returns a graph to free: one that was waiting and is now
    /// superseded before it ran.
    pub(crate) fn offer(&mut self, next: Box<Mix>) -> Option<Box<Mix>> {
        self.graph.leave_for(&next);
        self.pending.replace(next)
    }

    /// The graph a parameter message for `generation` is for: the waiting one if it is that generation, else the
    /// running one if it is.
    pub(crate) fn generation(&mut self, generation: u64) -> Option<&mut Mix> {
        match &mut self.pending {
            Some(next) if next.generation == generation => Some(next),
            _ => (self.graph.generation == generation).then_some(&mut *self.graph),
        }
    }

    /// Sets the mic mode on both graphs.
    pub(crate) fn set_mic_mode(&mut self, mode: MicMode) {
        self.graph.set_mic_mode(mode);
        if let Some(next) = &mut self.pending {
            next.set_mic_mode(mode);
        }
    }

    /// Runs one block of `n` frames, swapping in the waiting graph at the sample its fade-outs finish. Returns the
    /// replaced graph, to be freed off the audio thread.
    pub(crate) fn process(
        &mut self,
        n: usize,
        first: u64,
        inputs: &impl Inputs,
        points: &mut impl Points,
    ) -> Option<Box<Mix>> {
        if self.pending.is_none() {
            self.graph.process(n, first, inputs, points);
            return None;
        }
        let split = self.graph.fade_out_left().min(n);
        if split > 0 {
            self.graph.process_range(0, split, first, inputs, points);
        }
        if split == n && self.graph.fade_out_left() > 0 {
            return None;
        }
        let mut next = self.pending.take()?;
        next.adopt(&mut self.graph, split);
        let old = std::mem::replace(&mut self.graph, next);
        if split < n {
            self.graph.process_range(split, n, first, inputs, points);
        }
        Some(old)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::cast_precision_loss, clippy::cast_possible_truncation)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::mpsc::{TryRecvError, sync_channel};

    use omuzaki_audio_api::graph::{
        BusSpec, Channels, DuckRule, DuckTrigger, GraphSpec, MicMode, NodeSpec, SendSpec, SourceRef,
    };
    use omuzaki_audio_api::ids::{BusId, DeviceRole, NodeId, NodeKindId};
    use omuzaki_audio_api::node::{
        BlockMut, ChannelSupport, ConfigValue, LatencyRange, MAX_BLOCK, Node, NodeConfig, NodeError, NodeKind,
        NodeKindInfo, ParamIndex,
    };
    use omuzaki_audio_mock::conformance::allocations_during;
    use omuzaki_core::rng::Rng;

    use super::*;
    use crate::mix::{FADE, Stage};

    /// A node that writes how many samples it has seen into every sample, so a lost state shows as a restart.
    #[derive(Debug)]
    struct CountKind;

    omuzaki_audio_api::register_node_kind!(CountKind);

    struct Count(u64);

    impl NodeKind for CountKind {
        fn info(&self) -> NodeKindInfo {
            NodeKindInfo {
                id: "test.count",
                summary: "writes its running sample count (a test node)",
                channels: ChannelSupport::Any,
                params: &[],
                latency: LatencyRange { min: 0, max: 0 },
                sidechain: false,
            }
        }

        fn create(&self, _: &NodeConfig, _: Channels) -> Result<Box<dyn Node>, NodeError> {
            Ok(Box::new(Count(0)))
        }
    }

    impl Node for Count {
        fn latency(&self) -> u32 {
            0
        }

        fn set_param(&mut self, _: ParamIndex, _: f32) {}

        fn process(&mut self, block: &mut BlockMut<'_>) {
            let start = self.0;
            block.for_each_channel(|samples| {
                for (i, s) in samples.iter_mut().enumerate() {
                    *s = (start + i as u64 + 1) as f32;
                }
            });
            self.0 += block.frames() as u64;
        }

        fn reset(&mut self) {
            self.0 = 0;
        }
    }

    /// A seeded noise for every source.
    struct Noise(Vec<f32>);

    impl Noise {
        fn new(seed: u64) -> Self {
            let mut rng = Rng::new(seed);
            Self((0..MAX_BLOCK).map(|_| rng.range_f32(-0.5, 0.5)).collect())
        }
    }

    impl Inputs for Noise {
        fn source(&self, _: &SourceRef, n: usize) -> Option<(usize, [&[f32]; 2])> {
            Some((1, [&self.0[..n], &self.0[..n]]))
        }
    }

    /// Records each bus's post-fader signal, first channel.
    #[derive(Default)]
    struct Capture(BTreeMap<String, Vec<f32>>);

    impl Points for Capture {
        fn reached(&mut self, stage: Stage, bus: &BusId, _: usize, block: [&[f32]; 2]) {
            if stage == Stage::PostFader {
                self.0.entry(bus.as_str().to_owned()).or_default().extend_from_slice(block[0]);
            }
        }
    }

    struct Nowhere;

    impl Points for Nowhere {
        fn reached(&mut self, _: Stage, _: &BusId, _: usize, _: [&[f32]; 2]) {}
    }

    fn node(id: &str, kind: &str, latency: i64) -> NodeSpec {
        NodeSpec {
            id: NodeId::new(id).unwrap(),
            kind: NodeKindId::new(kind).unwrap(),
            config: NodeConfig(BTreeMap::from([("latency".to_owned(), ConfigValue::Int(latency))])),
            params: BTreeMap::new(),
            enabled: true,
            sidechain: None,
        }
    }

    fn bus(id: &str, chain: Vec<NodeSpec>) -> BusSpec {
        BusSpec {
            id: BusId::new(id).unwrap(),
            channels: Channels::Mono,
            sources: vec![SourceRef::Device { role: DeviceRole::new("line_in").unwrap() }],
            chain,
            gain_db: 0.0,
            muted: false,
            sends: Vec::new(),
            outputs: vec![DeviceRole::new("main_out").unwrap()],
        }
    }

    fn compile(spec: &GraphSpec, generation: u64, before: Option<&GraphSpec>) -> Box<Mix> {
        Box::new(Mix::compile(spec, generation, MicMode::Live, before).unwrap().0)
    }

    /// Runs `blocks` blocks of `n`, offering `swaps[k]` at the start of block `k` where given.
    fn run(first: &GraphSpec, swaps: &BTreeMap<usize, GraphSpec>, blocks: usize, n: usize) -> Capture {
        omuzaki_audio_mock::link();
        let noise = Noise::new(3);
        let mut running = Running::new(compile(first, 0, None));
        let mut capture = Capture::default();
        let mut before = first.clone();
        for k in 0..blocks {
            if let Some(next) = swaps.get(&k) {
                running.offer(compile(next, k as u64 + 1, Some(&before)));
                before = next.clone();
            }
            running.process(n, (k * n) as u64, &noise, &mut capture);
        }
        capture
    }

    #[test]
    fn an_untouched_bus_is_bit_identical_and_a_changed_one_shows_exactly_the_fade() {
        let a = GraphSpec {
            buses: vec![
                bus("voice", vec![node("eq", "mock.null", 64)]),
                bus("music", vec![node("comp", "mock.null", 32)]),
            ],
            ..GraphSpec::default()
        };
        // The voice gains a node that passes audio unchanged: only the fade can differ from the run without swaps.
        let mut b = a.clone();
        b.buses[0].chain.push(node("gate", "mock.null", 0));
        for n in [37, 480, 1_024] {
            let swaps = BTreeMap::from([(5, b.clone()), (9, a.clone()), (10, b.clone())]);
            let still = run(&a, &BTreeMap::new(), 16, n);
            let swapped = run(&a, &swaps, 16, n);
            let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
            assert_eq!(bits(&swapped.0["music"]), bits(&still.0["music"]), "blocks of {n}: the music was touched");
            // The fades: out over 64 samples from each offer, then in over 64. The offer at block 10 arrives while the
            // fade-in from block 9's swap may still run when blocks are short, and turns round where it is.
            let mut expected = still.0["voice"].clone();
            let offers = [5 * n, 9 * n, 10 * n];
            let (mut level, mut out) = (FADE, false);
            let factor: Vec<f32> = (0..expected.len())
                .map(|i| {
                    out |= offers.contains(&i);
                    if out {
                        level -= 1;
                        out = level > 0;
                    } else if level < FADE {
                        level += 1;
                    }
                    level as f32 / FADE as f32
                })
                .collect();
            for (e, f) in expected.iter_mut().zip(&factor) {
                *e *= f;
            }
            assert_eq!(bits(&swapped.0["voice"]), bits(&expected), "blocks of {n}: the voice's fade");
        }
    }

    #[test]
    fn a_counting_node_keeps_counting_across_a_hundred_swaps() {
        let with = |extra: bool| {
            let mut chain = vec![node("count", "test.count", 0)];
            if extra {
                chain.push(node("pad", "mock.null", 0));
            }
            GraphSpec { buses: vec![bus("voice", chain)], ..GraphSpec::default() }
        };
        let swaps: BTreeMap<usize, GraphSpec> = (0..100).map(|k| (2 * k + 1, with(k % 2 == 0))).collect();
        let n = 480;
        let blocks = 203;
        let out = run(&with(false), &swaps, blocks, n);
        let voice = &out.0["voice"];
        assert_eq!(voice.len(), blocks * n);
        // Outside the fades every sample is its own index plus one: the count never restarted.
        assert_eq!(*voice.last().unwrap(), (blocks * n) as f32);
        for k in 0..blocks {
            let at = k * n + 200;
            assert_eq!(voice[at], (at + 1) as f32, "block {k}");
        }
    }

    /// A random graph from a small family, so successive graphs share buses and nodes.
    fn random_graph(rng: &mut Rng) -> GraphSpec {
        let names = ["a", "b", "c", "d"];
        let mut buses = Vec::new();
        for name in names {
            if rng.next_f32() < 0.75 || (name == "d" && buses.is_empty()) {
                let mut chain = Vec::new();
                for id in ["n1", "n2", "n3"] {
                    if rng.next_f32() < 0.5 {
                        chain.push(node(id, "mock.null", [0, 32, 64][(rng.next_u64() % 3) as usize]));
                    }
                }
                let mut b = bus(name, chain);
                if rng.next_f32() < 0.5 {
                    b.channels = Channels::Stereo;
                }
                b.gain_db = rng.range_f32(-12.0, 0.0);
                buses.push(b);
            }
        }
        for i in 0..buses.len() {
            for j in (i + 1)..buses.len() {
                if rng.next_f32() < 0.4 {
                    let to = buses[j].id.clone();
                    buses[i].sends.push(SendSpec { to, gain_db: -6.0, pre_fader: rng.next_f32() < 0.3 });
                }
            }
        }
        let mut ducks = Vec::new();
        if buses.len() >= 2 && rng.next_f32() < 0.6 {
            ducks.push(DuckRule {
                trigger: DuckTrigger::Bus { id: buses[0].id.clone() },
                target: buses[buses.len() - 1].id.clone(),
                depth_db: 12.0,
                threshold_db: -40.0,
                attack_ms: 5.0,
                hold_ms: 20.0,
                release_ms: 50.0,
            });
        }
        GraphSpec { buses, ducks, ..GraphSpec::default() }
    }

    /// 10,000 random structural edits against a graph running on its own thread: the audio thread allocates and
    /// frees nothing, every old graph comes back to the control thread, and every edit takes over.
    #[test]
    fn ten_thousand_structural_edits_allocate_and_free_nothing_on_the_audio_thread() {
        const EDITS: u64 = 10_000;
        omuzaki_audio_mock::link();
        let (commands, inbox) = sync_channel::<Box<Mix>>(8);
        let (garbage, returned) = sync_channel::<Box<Mix>>(8);
        let mut rng = Rng::new(42);
        let first = random_graph(&mut rng);
        let start = compile(&first, 0, None);
        let audio = std::thread::spawn(move || {
            let noise = Noise::new(7);
            let mut running = Running::new(start);
            let mut held: Vec<Box<Mix>> = Vec::with_capacity(64);
            let mut sample = 0u64;
            let mut generation = 0u64;
            let mut k = 0usize;
            let count = allocations_during(&mut || {
                loop {
                    while let Some(old) = held.pop() {
                        if let Err(std::sync::mpsc::TrySendError::Full(old)) = garbage.try_send(old) {
                            held.push(old);
                            break;
                        }
                    }
                    let mut closed = false;
                    if held.len() < 32 {
                        match inbox.try_recv() {
                            Ok(next) => {
                                if let Some(superseded) = running.offer(next) {
                                    held.push(superseded);
                                }
                            }
                            Err(TryRecvError::Empty) => {}
                            Err(TryRecvError::Disconnected) => closed = true,
                        }
                    }
                    let n = [37usize, 480, 1, 1_024, 64][k % 5];
                    k += 1;
                    if let Some(old) = running.process(n, sample, &noise, &mut Nowhere) {
                        held.push(old);
                    }
                    sample += n as u64;
                    generation = running.graph.generation;
                    if closed && held.is_empty() && running.graph.fade_out_left() == 0 {
                        break;
                    }
                }
            });
            (count, generation)
        });
        let mut before = first;
        let mut freed = 0u64;
        for generation in 1..=EDITS {
            let next = random_graph(&mut rng);
            let mut mix = compile(&next, generation, Some(&before));
            before = next;
            loop {
                match commands.try_send(mix) {
                    Ok(()) => break,
                    Err(std::sync::mpsc::TrySendError::Full(back)) => {
                        mix = back;
                        if returned.try_recv().is_ok() {
                            freed += 1;
                        } else {
                            std::thread::yield_now();
                        }
                    }
                    Err(std::sync::mpsc::TrySendError::Disconnected(_)) => panic!("the audio thread stopped"),
                }
            }
            while returned.try_recv().is_ok() {
                freed += 1;
            }
        }
        drop(commands);
        while let Ok(old) = returned.recv() {
            drop(old);
            freed += 1;
        }
        let (count, generation) = audio.join().unwrap();
        assert_eq!(count, 0, "allocator calls on the audio thread");
        assert_eq!(generation, EDITS, "the last edit took over");
        assert!(freed <= EDITS, "{freed}");
        println!("swap soak: {EDITS} edits, {freed} graphs freed on the control thread, 0 allocator calls on audio");
    }

    /// Steady levels: the mic at 0.5, everything else at 1.
    struct Levels([f32; MAX_BLOCK], [f32; MAX_BLOCK]);

    impl Inputs for Levels {
        fn source(&self, source: &SourceRef, n: usize) -> Option<(usize, [&[f32]; 2])> {
            let level = match source {
                SourceRef::Device { role } if role.as_str() == "mic_in" => &self.0,
                _ => &self.1,
            };
            Some((1, [&level[..n], &level[..n]]))
        }
    }

    #[test]
    fn a_rule_removed_mid_duck_releases_instead_of_jumping() {
        omuzaki_audio_mock::link();
        let mut voice = bus("voice", Vec::new());
        voice.sources = vec![SourceRef::Device { role: DeviceRole::new("mic_in").unwrap() }];
        let with = GraphSpec {
            buses: vec![voice, bus("bed", Vec::new())],
            ducks: vec![DuckRule {
                trigger: DuckTrigger::Bus { id: BusId::new("voice").unwrap() },
                target: BusId::new("bed").unwrap(),
                depth_db: 12.0,
                threshold_db: -30.0,
                attack_ms: 10.0,
                hold_ms: 100.0,
                release_ms: 200.0,
            }],
            ..GraphSpec::default()
        };
        let without = GraphSpec { ducks: Vec::new(), ..with.clone() };
        let levels = Levels([0.5; MAX_BLOCK], [1.0; MAX_BLOCK]);
        let n = 480;
        let mut running = Running::new(compile(&with, 0, None));
        let mut capture = Capture::default();
        for k in 0..10 {
            running.process(n, (k * n) as u64, &levels, &mut capture);
        }
        running.offer(compile(&without, 1, Some(&with)));
        for k in 10..50 {
            running.process(n, (k * n) as u64, &levels, &mut capture);
        }
        let bed = &capture.0["bed"];
        let depth = omuzaki_audio_api::semantics::db_to_gain(-12.0);
        assert!((bed[10 * n - 1] - depth).abs() < 1e-6, "ducked when the rule goes: {}", bed[10 * n - 1]);
        // It holds for 100 ms, then rises over 200 ms: never a step bigger than one sample of the release.
        let steepest = bed.windows(2).skip(10 * n - 1).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
        assert!(steepest < 1e-3, "a jump of {steepest}");
        assert!((bed[10 * n + 4_799] - depth).abs() < 1e-6, "still held");
        assert_eq!(bed[10 * n + 4_800 + 9_600 + 10].to_bits(), 1.0f32.to_bits(), "released");
    }
}
