// A closure that captures a variable is curried with it; kept in a variable, the capture is held where no limit counts.
let big = "x";
while big.len < 500000 { big += big; }
let pointer = |a| a + big;
let again = pointer;
