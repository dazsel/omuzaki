// `replace` builds a result as long as the matches times the replacement (20,000 x 20,000 bytes here) before Rhai
// checks its length. The projected length is checked before it builds.
let a = "a";
while a.len < 20000 { a += a; }
a.replace("a", a);
