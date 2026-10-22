# omuzaki-face: the overlap union is exact for stacked, sloped and crossing edges

- A path that stacks many copies of a contour, or whose same-winding edges slope through the same pixel or cross inside a quarter row, now draws the union of its layers at partly covered pixels, as Chrome's canvas does.
- The check for layers that share a pixel costs about nothing on a face drawing.
