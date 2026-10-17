// A script starting another script run: scripts do not nest.
ops::script::run(#{ source: "ops::fake::set(#{ path: \"a\", value: 1 })" });
