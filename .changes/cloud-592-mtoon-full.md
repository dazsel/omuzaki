# Imported models draw with MToon 1.0's own lighting

- An imported model's MToon materials are lit the way three-vrm lights them, instead of through the kit's toon
  ramp (#592, LK-07). That covers:
  - the shade colour and its texture;
  - the shading shift and its texture, and the toony factor;
  - GI equalisation, as UniVRM defines it;
  - the parametric rim, with its fresnel power, lift, texture and lighting mix;
  - the matcap;
  - the emissive and normal textures;
  - UV scroll and rotation with their mask.
- Blended MToon surfaces that ask for depth writes (`transparentWithZWrite`) get them. The render-queue offset orders
  blended meshes as three-vrm does.
- The renderer reports every MToon group as drawn, so imports no longer list them under `look.mtoon_not_drawn`.
- Expression colour binds for the shade, rim and matcap colours now move the MToon colours themselves.
