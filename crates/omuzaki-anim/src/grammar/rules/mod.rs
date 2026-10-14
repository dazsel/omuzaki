//! The grammar rules, one module each. A rule registers itself with
//! [`register_grammar_rule!`](crate::register_grammar_rule), so adding one is a new file and a `mod` line here.

mod common;
mod grin_fangs;
mod laugh_squint;
mod pout_puff;
mod smile_blink;

pub use grin_fangs::GrinFangs;
pub use laugh_squint::LaughSquint;
pub use pout_puff::PoutCheekPuff;
pub use smile_blink::SmileBlinkArc;
