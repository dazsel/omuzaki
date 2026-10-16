# omuzaki-face: a contour drawn twice no longer doubles its edge

- A path that repeats a same-winding contour now covers its partly covered edge pixels as the contour drawn once does, as Chrome's canvas does. The CPU rasteriser sweeps each pixel row in four quarter-row bands to tell a layered edge from a single one.
