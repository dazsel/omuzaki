# Staircases, framed art from your library and a crystal window head

- New prop kind `staircase`: a straight flight with nosings, an optional carpet runner and a balustrade on either side
  or both (`balusters`, art-deco `panels` or a `solid` wall) under a sloped handrail between newel posts.
- `picture` takes `image` (a PNG or JPEG from the asset store, by its content hash) and `fit` (`cover` crops it to
  fill the canvas, `contain` shows all of it on the canvas's ground). Until it loads, or if the store lacks it, the
  painted canvas shows. Any prop kind can show an image on one of its pieces the same way.
- `window` takes `starburst`: a transom over the glass with a starburst of crystal shards round a gem.
- Prop kinds can turn solids on a lathe (balusters, posts, candlesticks).
