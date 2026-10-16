// Take input: `params` is the map the caller passed in (the console's parameter box, `--params @file.json`,
// or an agent's `params`). Check a key with `in` before you use it, so a missing one gets a default.
let blend = if "blend" in params { params.blend } else { 0.5 };
ops::face::set_profile(#{ character: params.character, profile: "egg-oval", blend: blend })
