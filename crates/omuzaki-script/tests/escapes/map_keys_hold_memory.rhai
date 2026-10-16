// Rhai counts the values in a map but not its keys, so 2,000 half-megabyte keys would hold a gigabyte unseen. The
// memory budget counts what the run's thread holds and stops it.
let k = "x";
while k.len < 500000 { k += k; }
let m = #{};
for i in 0..2000 {
    m[k + i] = 1;
}
