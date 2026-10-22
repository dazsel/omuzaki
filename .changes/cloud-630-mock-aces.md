# The mock renderer tone maps like the GPU

- `omuzaki-render-mock` now uses three.js's ACES curve (the `1 / 0.6` pre-scale, the input and output matrices, and the RRT and ODT fit), the curve the GPU renderer and the prototype use (#630). The mock is within 0.5 of a level of three.js on 55 colours at two exposures, where it was off by up to 204 levels on saturated colours. Goldens that use the default tone map now agree with the GPU, and the cube golden is blessed again.
