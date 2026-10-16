// Build a function pointer from a string and call it: `Fn` is disabled, so no op can be reached by a computed name.
let f = Fn("ops::fake::set");
f.call(#{ path: "a", value: 1 });
