// The third hook for a program that does not define `program_alpha`, and for a draw with no program: the alpha is left
// alone. Appended by `opaque_source`. Only the Transparent pass calls it; the Opaque pass never does.
fn program_alpha(p: ProgramIn, alpha: f32) -> f32 {
    return alpha;
}
