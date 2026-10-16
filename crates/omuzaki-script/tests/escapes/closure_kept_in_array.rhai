// Rhai counts the values in arrays and maps but not what a closure captures, so keeping closures would hold 2,000
// megabyte strings unseen. A closure may only be passed straight to `try_op` or an array function.
let k = "x";
while k.len < 500000 { k += k; }
let fs = [];
for i in 0..2000 {
    let s = k + i;
    fs.push(|| s);
}
