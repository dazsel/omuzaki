# JPEG textures resize, and a cut-short JPEG is refused instead of decoded with zeros

- `fit_texture::resize` and `downscale_to` take JPEG input (#2009), baseline and progressive, and return PNG. The
  frame header's size is checked against the limits before any decoder is built, so a header that declares an
  oversized image is `IoError::Limit` without decoding.
- Before a JPEG is decoded, every scan's entropy-coded data is walked to its end against the frame header (#1144):
  Huffman tables, byte stuffing, restart markers and the MCU count of each scan, for spectral selection and successive
  approximation too. A scan whose data stops early, a missing or out-of-order restart marker, or a file without its
  end-of-image marker is `Malformed`, naming the scan and how many MCUs were complete. JPEG decoders fill such a scan
  with zeros, which made a damaged texture instead of an error. Lossless, hierarchical, arithmetic-coded and 12-bit
  JPEGs are `Unsupported`; more than 256 scans is a `Limit`.
- `io.import` and `io.inspect` name FBX among the formats they read (#981).
