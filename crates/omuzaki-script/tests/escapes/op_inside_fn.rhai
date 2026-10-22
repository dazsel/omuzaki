// An op named inside a function that a closure calls: `check` must still list it, and the run refuses it.
fn start() {
    ops::script::run(#{ source: "1 + 1" })
}
[1].map(|x| start());
