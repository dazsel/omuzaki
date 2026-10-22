//! `publish` latency (#777: at most 2 µs at p99 on the dev machine): chat events published into a running show with
//! no listeners while the engine thread drains the inbox as it would live. Two loads, each printed as p50, p99 and
//! p99.9:
//!
//! - **paced**: one event every 20 µs (50,000 a second, far past any live chat), each one finding the engine between
//!   steps;
//! - **burst**: 1,000 back to back, then a 5 ms gap, so producers and the engine thread contend for the queue (and the
//!   allocator) on every call;
//! - **the floor**: the same paced calls with no show running, which is `publish`'s checks alone.
//!
//! `cargo bench -p omuzaki-live --bench inbox`

#![allow(clippy::unwrap_used, clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use std::time::{Duration, Instant};

use omuzaki_live::Automation;
use omuzaki_live_api::event::{EventSink, LiveEvent};
use omuzaki_live_api::ids::ShowId;
use omuzaki_live_api::show::ShowDef;
use omuzaki_live_mock::fixtures;

const CALLS: usize = 100_000;
const BURST: usize = 1_000;
const PACE: Duration = Duration::from_micros(20);

fn events(tag: &str) -> Vec<LiveEvent> {
    // Built ahead, so the timing covers `publish` alone; distinct texts, so dedup keeps each one.
    (0..CALLS).map(|i| fixtures::chat("kiri", &format!("{tag}{i}"))).collect()
}

fn report(load: &str, mut times: Vec<Duration>) {
    times.sort_unstable();
    let at = |q: f64| times[((times.len() as f64 - 1.0) * q).round() as usize];
    println!("publish, {load}, {} calls: p50 {:?}, p99 {:?}, p99.9 {:?}", times.len(), at(0.5), at(0.99), at(0.999));
}

fn main() {
    omuzaki_live::link();
    let automation = Automation::new();
    automation.start_def(&ShowId::new("bench").unwrap(), &ShowDef::new("Bench")).unwrap();

    let mut times = Vec::with_capacity(CALLS);
    let mut next = Instant::now();
    for event in events("paced") {
        while Instant::now() < next {
            std::hint::spin_loop();
        }
        next += PACE;
        let started = Instant::now();
        let published = automation.publish(event);
        times.push(started.elapsed());
        published.unwrap();
    }
    report("paced every 20 µs", times);

    let mut times = Vec::with_capacity(CALLS);
    for (i, event) in events("burst").into_iter().enumerate() {
        if i % BURST == 0 {
            std::thread::sleep(Duration::from_millis(5));
        }
        let started = Instant::now();
        let published = automation.publish(event);
        times.push(started.elapsed());
        published.unwrap();
    }
    report("bursts of 1,000", times);

    let idle = Automation::new();
    let mut times = Vec::with_capacity(CALLS);
    let mut next = Instant::now();
    for event in events("floor") {
        while Instant::now() < next {
            std::hint::spin_loop();
        }
        next += PACE;
        let started = Instant::now();
        let published = idle.publish(event);
        times.push(started.elapsed());
        published.unwrap();
    }
    report("no show running (validation only, the floor)", times);
}
