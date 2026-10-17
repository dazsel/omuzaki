// Give every character the same face profile, and skip the ones that can't take it.
// `try_op` runs one call and answers #{ ok, value, error }, so one failure doesn't end the run.
let done = 0;
for name in ["aki", "ren", "nobody"] {
    let r = try_op(|| ops::face::set_profile(#{ character: name, profile: "egg-oval" }));
    if r.ok {
        done += 1;
    } else {
        log(`skipped ${name}: ${r.error.message}`);
    }
}
print(`updated ${done} characters`);
done
