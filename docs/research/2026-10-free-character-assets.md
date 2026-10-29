---
title: "Free 3D character assets Omuzaki can ship or point people to"
summary: "which free 3D characters, bases and animations are safe for a commercial app, and how they enter Omuzaki"
area: research
---

# Free 3D character assets Omuzaki can ship or point people to

**Question.** Omuzaki's own kit character doesn't yet look good enough to start from. Which free (no money spent) 3D
characters, character bases and animations can Omuzaki ship with, or tell people to bring in, given the licence policy
(`docs/licensing/policy.md`: nothing non-commercial, ever)? Every licence below was read at its primary source on
2026-10-29; each has its row in `docs/licensing/deps/`. This is an engineering reading, not legal advice.

## The answer

| Source | What it is | Licence, as read | Ship it? | How it enters |
|---|---|---|---|---|
| **VRoid Project CC0 samples** (β Ver AvatarSample_1 to 4, Sendagaya Shino, Sakurada Fumiriya, the base and hair samples) | finished anime-style VRM 0.x characters made by pixiv, 27k to 40k triangles, 39 to 56 blend shapes, spring-bone hair | **CC0**: pixiv "has waived all copyright and related or neighboring rights to this model" | **Yes** | starter characters, fetched on demand (`io.starter.*`) |
| **Seed-san** (VRM Consortium sample, VirtualCast) | a VRM 1.0 reference model | VRM Public License 1.0; its settings allow corporate commercial use, modification and redistribution; **credit required** | **Yes, with credit** | starter character |
| VRoid Studio itself (free app) | the easiest free way for a person to make their own character | output is the maker's, commercial use allowed, **but** "You cannot create an application that can generate or output 3D models … consisting of deformed or combined meshes and textures that were created in VRoid Studio" without pixiv's separate licence | **No** for presets we make in it; **yes** for people's own characters | `io.import {as: avatar}` (bring your own model) |
| VRoidPreset_A to Z, AvatarSample_A to C | VRoid Studio's current samples | not CC0; for-profit use allowed, but prohibits "Development of a character creation service using data contained in the sample model" | **No** | a person may still import them to perform |
| VRoid Hub and BOOTH models | thousands of free models by individual creators | each model's own conditions (VRoid Hub licence settings, often the VN3 licence on BOOTH); most allow streaming, few allow redistribution | **No** (we can't redistribute) | the person downloads with their own account and imports |
| Open Source Avatars registry | a curated list of free VRMs | per collection: 100Avatars R1 to R3 and five others CC0, VIPE Heroes CC-BY | possible (CC0 collections) | not yet: their style is low-poly and stylised, below the anime bar |
| MakeHuman / MPFB | free human generator (Blender add-on) | code GPL/AGPL (separate programs only); "All core assets are shared under … CC0" | output yes, tool never bundled | export glTF/VRM, then import |
| Quaternius Universal Animation Library 1 and 2 | 45 + ~90 free humanoid clips (idle, locomotion, emotes) | **CC0** on OpenGameArt | **Yes** | next chunk: retarget to the VRM humanoid as library clips |
| Mixamo | free auto-rigging and animation service | Adobe account needed; the FAQ copy says commercial use is royalty free but "the only thing you can't do is distribute the raw character and animation files"; the primary page refused automated reads | **No** | the person downloads FBX and imports |
| Inochi2D, Live2D samples | 2D puppets | (out of scope: the owner wants a clearly 3D character) | n/a | n/a |
| MetaHuman, FLAME | realistic heads | engine-bound EULA; research-only | **No** | n/a |

**So the free route to a good-looking character today is pixiv's CC0 VRoid samples**: they are the only finished,
anime-style, rigged, expression-ready VTuber characters we found that a commercial character app may ship and let
people edit and re-export. Seed-san adds a VRM 1.0 reference with credit. For our own character, VRoid Studio is the
wrong tool to make it in (the application clause above); the kit stays the way to make original characters, and the
samples are the quality reference it is measured against and the starting point people get on day one.

## Details that matter

**Which file is which.** The OpenGameArt mirror (<https://opengameart.org/content/vroid-studio-cc0-models>, CC0) holds
the alpha-era exports. Their file names predate pixiv's renaming; the VRM meta titles and thumbnails, and the portraits
on each model's VRoid Hub page, match them: `AvatarSample_D` = β Ver AvatarSample_1 (and `AvatarSample_D_Darkness` is
her "Darkness version"), `E` = β 2, `F` = β 3, `G` = β 4. Sendagaya Shino and Sakurada Fumiriya keep their names.

**What the files say about themselves.** β 1 to 4, Shino and Fumiriya carry `licenseName: Other` pointing at a VRoid
Hub licence URL whose query string allows everything (corporate commercial use, modification, redistribution, no
credit). Omuzaki's importer reads a 0.x `Other` conservatively (design `io-api.md`: no redistribution, no modification,
credit required, commercial status unknown), which is right for an unknown file but wrong for these, whose CC0 waiver
pixiv publishes. That is why starters enter through a catalogue that carries the verified record, not through a plain
import: the catalogue stores the original under CC0 and hands back the avatar's source row with the CC0 terms. The
path is `io.starter.fetch`, then `io.inspect` and `io.import` as an avatar, then `licence.set_source` with that row,
`licence.derive` and `licence.set`. With the record set, `io.check_export` and `io.export` use the record's terms in
place of the file's own (held against the rows, so a stale record is refused), and the character publishes as CC0.

**Trademarks.** "VRoid" is pixiv's mark. We say where a starter came from and credit the VRoid Project, and never imply
pixiv made or endorses Omuzaki.

**The bar.** The reference bar (#2393, the talking-head short T1) asks for a mature anime face with an anime symbol vocabulary (`><`
eyes, blank shock eyes, anger marks, gloom shading), heterochromia, rim-lit hair and 12 or more distinct expressions.
The VRoid samples bring a clean anime base (β 3 is even heterochromatic) with 5 to 7 emotion expressions, visemes and
blinks; the anime symbols are not in the files, and the renderer does not yet draw MToon's matcap, rim and shade
textures (LK-07). The gaps are listed in the pull request that adds the starters, with renders.

## Sources

- pixiv, "Do VRoid Studio's sample models come with conditions of use?": <https://vroid.pixiv.help/hc/en-us/articles/4402614652569>
- β Ver AvatarSample_1 to 4: articles `360012381793`, `360014900273`, `360014900113`, `360014900233`; Sendagaya Shino `360013482714`; Sakurada Fumiriya `360014788554` (all under <https://vroid.pixiv.help/hc/en-us/articles/>)
- VRoidPreset_A to Z conditions: <https://vroid.pixiv.help/hc/en-us/articles/4402394424089>
- VRoid Studio output for commercial use: <https://vroid.pixiv.help/hc/en-us/articles/4405813333657>
- Seed-san: <https://github.com/vrm-c/vrm-specification/tree/master/samples/Seed-san>; VRM Public License 1.0: <https://vrm.dev/en/licenses/1.0/>
- Open Source Avatars: <https://github.com/ToxSam/open-source-avatars> (`data/projects.json`)
- MakeHuman and MPFB licence: <https://static.makehumancommunity.org/about/license.html>; MPFB FAQ on closed-source use
- Quaternius UAL: <https://opengameart.org/content/universal-animation-library>, <https://opengameart.org/content/universal-animation-library-2>
- Mixamo FAQ (community copy): <https://community.adobe.com/questions-696/mixamo-faq-licensing-royalties-ownership-eula-and-tos-589400>
