// A closure stored in a map holds what it captured out of sight of the map size limit.
let k = "x";
while k.len < 500000 { k += k; }
let held = #{};
for i in 0..2000 {
    let s = k + i;
    held.f = || s;
}
