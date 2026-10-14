//! The resting face ops (FE-06): `face.set_rest` sets one field of the resting face in paint terms
//! (`face/mapping/rest`), and `face.set_rest_channels` sets the `rest` bus layer that the painter reads on top of the
//! neutral bus. Both validate the whole face and return the resting paint before and after.

mod channels;
mod set;

pub use channels::SetRestChannels;
pub use set::SetRest;
