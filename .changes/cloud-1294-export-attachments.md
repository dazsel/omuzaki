# Imported avatars: attachments in the exported file

- `io.export` and `io.check_export` of an imported character build its attachments as the stage does and write them
  after everything the file had: a part on one bone is a node under that bone, its vertices in the bone's frame, and a
  part spread over several bones is a skinned node at the root. Their materials and textures are written from the
  kit's specs.
- The attach code moved from the stage to `omuzaki_io_ops::ops::avatar::attach`, so the stage and export share it.
