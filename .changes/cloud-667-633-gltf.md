# glTF: hyphenated attributes round trip, and nested arrays are counted

- Custom vertex attributes with a hyphen in their name (`_HAIR-UV`, `_HAIR-INFO`, which the kit's hair writes) now read back on import (#667). A gathered character keeps its hair data through export, import and re-export, byte for byte.
- The importer counts the arrays nested in meshes, scenes, nodes and skins before it parses the document (#633): the primitives, morph targets, scene roots, node children and skin joints summed over the file. A file with more than the limits allow is refused by count with almost nothing allocated, and a scene root that names no node is malformed at once instead of costing one finding per root.
