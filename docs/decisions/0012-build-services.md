---
title: "0012: Build services are injected into the character builder (2026-10-12)"
summary: "Build services are injected into the character builder"
area: decisions
---

# 0012: Build services are injected into the character builder (2026-10-12)

**Status:** accepted. **Context:** `docs/design/char-face-api.md`; CONTRIBUTING (a breaking API change needs a decision record).

## Context

Garments draw procedural textures (plaid, tie stripes, cloth weave, a robe's celtic bands) and the head reads a traced radius map from the asset store. `BuildCtx` offered neither a rasteriser nor the store. The layering rule (an implementation crate depends on API crates only) means `omuzaki-char` cannot call the rasteriser in `omuzaki-face` and cannot open `omuzaki-store`.

## Decision

1. The app injects both as API types. `omuzaki-char-api` gains `AssetSource` (read an asset by `ContentHash`), `RasterizerFactory` (`Arc<dyn Fn() -> Box<dyn FaceRasterizer> + Send + Sync>`) and `BuildServices { assets: Option<..>, rasterizer: Option<..> }`.
2. **Breaking:** `CharacterBuilder::build` takes a fourth argument, `services: &BuildServices`. `BuildCtx` gains a `services` field, and parts use `BuildCtx::asset(&hash)` and `BuildCtx::paint_texture(size_px, rect, &DrawList, desc)`.
3. Services are **not** part of `BuildOptions`, which stays `Eq`, and not part of a cache key. Assets are content-addressed and the rasteriser is a deterministic function of its draw list, so the services change where inputs come from, never what the same inputs build.
4. A missing service is a named error, never a wrong build: `BuildError::Missing { needs: "a 2D rasteriser" }` for textures, `AssetError::Unavailable` for assets.
5. `FaceLayerSpec::for_rect(rect, layer, px_per_m)` in `omuzaki-face-api` describes a texture that is not a face layer, so a garment needs no fake `FaceProjection`.

## Consequences

- Every caller of `CharacterBuilder::build` adds an argument (`&BuildServices::default()` for none). In this repo that is the tests of `omuzaki-char-api` and `omuzaki-char-mock`; the stage and the real builder come later and are written against the new signature.
- `omuzaki-char` stays free of `omuzaki-face` and `omuzaki-store`; the app (`omuzaki-stage`, the CLI) builds the services from the real rasteriser and store.
- A factory (not one shared rasteriser) means a parallel builder never shares mutable rasteriser state.
- Rejected: putting services in `BuildOptions` (breaks `Eq` and muddles the cache key); a global registry for the rasteriser (hidden state, not testable); letting `omuzaki-char` depend on `omuzaki-face` (breaks the layering rule that keeps crates buildable and testable alone).
