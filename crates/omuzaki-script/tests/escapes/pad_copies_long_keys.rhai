// `pad` clones its item once per added element, and Rhai doesn't count a map's keys: 100 copies of ten half-megabyte
// keys is 500 MiB in one call. The projected size is checked before it copies.
let k = "x";
while k.len < 500000 { k += k; }
let m = #{};
for i in 0..10 {
    m[k + i] = 1;
}
let a = [];
a.pad(100, m);
