# Import: .gltf files with external buffers and images

- `io.import` now imports a `.gltf` that names external `.bin` and image files, when it is given as a `path`. The files
  are read from the model's own folder and nowhere else (a file outside it is refused, as `io.inspect` refuses it), and
  the input size limit counts them with the model. A model passed as `bytes` still has no folder to read from.
