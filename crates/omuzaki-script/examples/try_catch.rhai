// Handle an error with Rhai's own try/catch. The caught value is the same map `try_op` returns as `error`:
// code, message, field and hint, plus the op, line and column of the call that failed.
try {
    ops::face::set_profile(#{ character: "nobody", profile: "egg-oval" });
} catch (error) {
    print(`${error.op} failed (${error.code}): ${error.message}`);
    if error.hint != () {
        print(`hint: ${error.hint}`);
    }
}
