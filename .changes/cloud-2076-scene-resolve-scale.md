# omuzaki-stage: scenes apply to the renderer's scene without allocating, and resolve refuses two more bad inputs

- `scene::apply` fills the renderer's `Scene` from a `ResolvedScene` (active camera, lights, background, post look, outline light) with no allocation, tells the caller when the ground disc or the skin-floor override changed, and hands resolved scenes from another thread to the render thread through a one-slot newest-wins `Mailbox` (#274, #131).
- `ResolvedScene::resolve_into` refuses a point or spot light whose range resolves to 0 or below (a tiny range on a tiny figure), instead of handing the renderer a light it rejects (#2020).
- It also refuses a figure root lifted in Y: a cast member's root is `(x, 0, z)` (#2020).
- `Landmarks::measure` reads skinned vertices and joints in the pose the stage draws at rest (the T-pose), not the bind pose, where a template's arms hang down. The hands preset now frames both hands instead of the hips (#1978). The bust box leaves out forearms and hands, and the body box now includes the outstretched arms.
- The framing contact sheet also writes a 1:1 sheet.
