// The same through maps kept in an array: every map is small and the entries add up to less than the map limit, so
// no data limit applies. The memory budget stops the run.
let k = "x";
while k.len < 500000 { k += k; }
let held = [];
for n in 0..200 {
    let m = #{};
    for i in 0..10 {
        m[k + i] = 1;
    }
    held.push(m);
}
