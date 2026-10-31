//! The shades one named colour becomes on each thing it colours: a hair gradient, an iris, a cloth with its shadow
//! and line. Mixed in linear light, so a shade darkens the way paint under less light does.

use omuzaki_core::colour::Colour;
use omuzaki_core::glam::Vec3;

/// `a` moved toward `b` by `t` (0 is `a`), in linear light.
pub fn mix(a: Colour, b: Colour, t: f32) -> Colour {
    Colour::from_linear(a.to_linear().lerp(b.to_linear(), t.clamp(0.0, 1.0)), 1.0)
}

/// `c` in less light: darker, and a little toward cool violet, the way toon shadows read.
pub fn shade(c: Colour, amount: f32) -> Colour {
    let cool = Vec3::new(0.30, 0.27, 0.42);
    let lit = c.to_linear();
    let dark = lit * (1.0 - amount) + lit.lerp(cool * lit.length().max(0.02), 0.25) * amount * 0.25;
    Colour::from_linear(dark.max(Vec3::ZERO), 1.0)
}

/// The hair's root, middle and tip for `colours` (one colour, or a gradient from the first to the last).
pub fn hair(colours: &[Colour], named: Option<[Colour; 3]>) -> [Colour; 3] {
    match (colours, named) {
        ([_], Some(named)) => named,
        ([one], None) => [shade(*one, 0.35), *one, mix(*one, Colour::WHITE, 0.18)],
        ([first, .., last], _) => [shade(*first, 0.2), mix(*first, *last, 0.45), *last],
        ([], _) => [Colour::BLACK; 3],
    }
}

/// The iris's top, middle, lit crescent and ring for `colours`.
pub fn iris(colours: &[Colour]) -> [Colour; 4] {
    let (top, low) = match colours {
        [one] => (*one, *one),
        [first, .., last] => (*first, *last),
        [] => (Colour::BLACK, Colour::BLACK),
    };
    [shade(top, 0.55), mix(top, low, 0.5), mix(low, Colour::WHITE, 0.3), shade(top, 0.75)]
}

/// The cloth's colour, its shadow and its line for `colour`.
pub fn cloth(colour: Colour) -> [Colour; 3] {
    [colour, shade(colour, 0.3), shade(colour, 0.6)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luma(c: Colour) -> f32 {
        c.to_linear().dot(Vec3::new(0.2126, 0.7152, 0.0722))
    }

    #[test]
    fn a_shade_is_darker_and_a_hair_tip_lighter_than_the_middle() {
        for hex in [0x00c0_c8d8, 0x003a_2a4a, 0x00f0_d070, 0x00e0_5a8a, 0x0010_1010] {
            let c = Colour::hex(hex);
            assert!(luma(shade(c, 0.35)) < luma(c), "{c}");
            let [root, mid, tip] = hair(&[c], None);
            assert!(luma(root) < luma(mid) && luma(mid) < luma(tip), "{c}: {root} {mid} {tip}");
        }
    }

    #[test]
    fn a_gradient_runs_from_the_first_colour_to_the_last() {
        let (blonde, pink) = (Colour::hex(0x00f2_d58a), Colour::hex(0x00f0_9ab8));
        let [_, _, tip] = hair(&[blonde, pink], None);
        assert_eq!(tip, pink);
        let [top, _, low, _] = iris(&[blonde, pink]);
        assert!(luma(top) < luma(low));
    }
}
