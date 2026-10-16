//! The latency picture of a mixer (design section 5.5): what every route from a source to an output or a tap adds, in
//! samples, summed from the nodes' declared latencies. A disabled node keeps its latency, so it counts.

use omuzaki_audio_api::graph::SourceRef;
use omuzaki_audio_api::ids::DeviceRole;
use omuzaki_audio_api::report::{BusLatency, GraphReport, RouteEnd, RouteLatency};
use omuzaki_audio_api::tap::TapPoint;
use omuzaki_core::time::Seconds;

use crate::mixer::Mixer;

impl Mixer {
    /// The sum of the declared latencies of bus `at`'s chain.
    fn chain_latency(&self, at: usize) -> u32 {
        self.buses[at].chain.iter().map(|l| l.node.latency()).sum()
    }

    /// Every route from a source through the buses to an output role or to one of `taps` (resolved points), and each
    /// bus's chain latency. Bridges and devices add nothing in the mock.
    pub(crate) fn report(&self, taps: &[TapPoint]) -> GraphReport {
        let mut routes: Vec<RouteLatency> = Vec::new();
        let mut push = |from: &SourceRef, to: RouteEnd, graph_samples: u32| {
            let route = RouteLatency {
                from: from.clone(),
                to,
                graph_samples,
                bridge_in: Seconds(0.0),
                bridge_out: Seconds(0.0),
                device: Seconds(0.0),
                measured: None,
            };
            if !routes.contains(&route) {
                routes.push(route);
            }
        };
        for (start, bus) in self.buses.iter().enumerate() {
            for source in &bus.sources {
                if let SourceRef::Device { role } = source
                    && taps.contains(&TapPoint::Input(role.clone()))
                {
                    push(source, RouteEnd::Tap { point: TapPoint::Input(role.clone()) }, 0);
                }
                self.walk(start, 0, &mut |at, latency_in, latency_out| {
                    let id = &self.buses[at].id;
                    for role in &self.buses[at].outputs {
                        push(source, RouteEnd::Output { role: DeviceRole::clone(role) }, latency_out);
                    }
                    for (point, latency) in [
                        (TapPoint::BusPreChain(id.clone()), latency_in),
                        (TapPoint::BusPostChain(id.clone()), latency_out),
                        (TapPoint::BusPostFader(id.clone()), latency_out),
                    ] {
                        if taps.contains(&point) {
                            push(source, RouteEnd::Tap { point }, latency);
                        }
                    }
                });
            }
        }
        let buses = (0..self.buses.len())
            .map(|at| BusLatency { bus: self.buses[at].id.clone(), chain_samples: self.chain_latency(at) })
            .collect();
        GraphReport { routes, buses }
    }

    /// Visits bus `at` and every bus downstream of it along the sends, with the latency on arrival (`latency_in`) and
    /// after the chain (`latency_out`). A bus reached by two paths is visited once per path.
    fn walk(&self, at: usize, latency_in: u32, visit: &mut impl FnMut(usize, u32, u32)) {
        let latency_out = latency_in + self.chain_latency(at);
        visit(at, latency_in, latency_out);
        for send in &self.buses[at].sends {
            self.walk(send.to, latency_out, visit);
        }
    }
}
