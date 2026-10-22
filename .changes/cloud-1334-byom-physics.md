# Imported avatars: the end-to-end physics check

- A VRM 1.0 with spring chains, an inside sphere, a plane, cone and hinge limits, and roll, aim and rotation
  constraints keeps all of them through import, performs a head shake on the stage with the sphere holding its chain,
  the hinge holding its plane and the aim on its source, and is exported and read back equal to 1e-6 (#1334).
