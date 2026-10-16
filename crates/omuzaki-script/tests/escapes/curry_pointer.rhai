// Curry a pointer to fix its arguments: `curry` is disabled at the parser.
let pointer = |a, b| a;
let fixed = pointer.curry(#{ path: "a", value: 1 });
fixed.call(2);
