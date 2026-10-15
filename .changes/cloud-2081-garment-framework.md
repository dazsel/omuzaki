# omuzaki-char: the garment framework and trim library

- A garment is a part kind built from a few calls: its params embed `GarmentCommon` (ease, fabric, cloth), it lofts `Surface`s over the fit inside it, and `GarmentOutput` names, paints and publishes it.
- Garments stack: each publishes an `Envelope` (the fit inside it plus its ease), and layers follow the 10/20/30/40 convention.
- Fabrics: plain, weave, stripes (vertical, horizontal, diagonal) and plaid, painted as seamless repeating tiles.
- Trims on the garment's surface, ported from the prototype: the robe's rolled-edge strip, the vest's piping cords, braid, the gold buttons and rivets, the chain with its pendant cross, the medallion clasp, emblem decals and scalloped lace; metal trims use the `band-highlight` program.
- One conformance suite checks any garment on every template and body shape: clearance, crossings, closure, skin, names, determinism and build time.
