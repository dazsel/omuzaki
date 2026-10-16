// A function that returns a closure hands out a pointer that outlives its call.
fn make(s) {
    || s
}
let k = "x";
while k.len < 500000 { k += k; }
let all = [];
for i in 0..2000 {
    all.push(make(k + i));
}
