# omuzaki-char: braid crown

- The braid generator makes the crown: interleaved strands per side that follow a path round the back of the head (port of `hair.js` section 4, #1675).
- Strand width narrows to 0.4 of `strand_width` over the last 8 % of the length; the root does not yet halve as in the prototype (#842).
- The generator draws no random numbers, so a part's random stream is unchanged.
