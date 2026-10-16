// Evaluate text as code: the `eval` keyword is disabled at the parser.
let name = "ops::fake::set";
eval(`${name}(#{ path: "a", value: 1 })`);
