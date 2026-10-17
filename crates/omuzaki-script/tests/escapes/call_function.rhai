// The `call` keyword invokes a pointer by name or value: disabled at the parser.
let pointer = |args| args;
call(pointer, #{ path: "a", value: 1 });
