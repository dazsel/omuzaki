# A scene's skin floor reaches the skin materials

- Setting, changing or clearing a scene's `skin_floor` now updates every toon skin material of the staged character in place when the stage pays the scene (#274): `Stage::pay_scene` no longer leaves the floor owed. No mesh is made and no other material is touched, and clearing the override gives each material its own floor back.
- The floor stays on the materials through a look edit, a palette change and a variant swap, because every material the stage sends to the renderer carries it. `StageCharacter::skin_floor` says which floor is in force.
