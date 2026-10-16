// Read before you write: `get(path)` reads the document and answers `()` when there is nothing at the path.
let face = get("characters/aki/face");
if face == () {
    print("aki has no face yet");
} else {
    print(`aki's profile is ${face.profile} at blend ${face.blend}`);
}
face
