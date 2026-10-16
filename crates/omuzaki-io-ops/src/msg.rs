//! The crate's message table: every sentence `omuzaki-io-ops` says, declared once with `omuzaki_l10n_api::messages!`
//! (`docs/design/l10n-ops.md`, sections 3.2 and 5).
//!
//! The table starts with the import report. Every rule an importer or the preparation steps can raise has a line,
//! `io-ops-<rule>`, and a fix, `io-ops-<rule>-hint`, with the rule id in kebab case (dots and underscores become
//! hyphens). [`crate::report_sections`] decides which of the four report sections a rule belongs to. Both are keyed by
//! the rule id, so the screen never parses text.
//!
//! The English is plain language for a `VTuber` and one whole sentence. A message takes an argument only when the
//! finding names something a person needs to see (a channel, a bone, an expression); the argument is an `Id`, shown as
//! it is, never a fragment joined into the sentence. The crate's other messages (op errors and reasons) join this table
//! as they are written.

omuzaki_l10n_api::messages! {
    /// The report line for the `humanoid.inferred` rule: what the import found.
    pub humanoid_inferred = "io-ops-humanoid-inferred" ()
        => "Some of the body bones were guessed from their names and shape.";
    /// The fix for the `humanoid.inferred` rule, shown under its report line.
    pub humanoid_inferred_hint = "io-ops-humanoid-inferred-hint" ()
        => "Check the result, or set the bones yourself in the humanoid overrides.";
    /// The report line for the `humanoid.ambiguous` rule: what the import found. `bone` is the name of the thing it is about, shown as it is.
    pub humanoid_ambiguous = "io-ops-humanoid-ambiguous" (bone: Id)
        => "More than one bone could be { $bone }, so one was picked.";
    /// The fix for the `humanoid.ambiguous` rule, shown under its report line.
    pub humanoid_ambiguous_hint = "io-ops-humanoid-ambiguous-hint" ()
        => "Check the pick, or choose the right bone in the humanoid overrides.";
    /// The report line for the `humanoid.required_missing` rule: what the import found. `bone` is the name of the thing it is about, shown as it is.
    pub humanoid_required_missing = "io-ops-humanoid-required-missing" (bone: Id)
        => "This model has no bone that can be { $bone }, and the body cannot move without it.";
    /// The fix for the `humanoid.required_missing` rule, shown under its report line. `bone` is the name of the thing it is about, shown as it is.
    pub humanoid_required_missing_hint = "io-ops-humanoid-required-missing-hint" (bone: Id)
        => "Pick a bone for { $bone } in the humanoid overrides, or fix the skeleton in your modelling tool.";
    /// The report line for the `humanoid.override_invalid` rule: what the import found. `bone` is the name of the thing it is about, shown as it is.
    pub humanoid_override_invalid = "io-ops-humanoid-override-invalid" (bone: Id)
        => "The bone you chose for { $bone } cannot be used.";
    /// The fix for the `humanoid.override_invalid` rule, shown under its report line. `bone` is the name of the thing it is about, shown as it is.
    pub humanoid_override_invalid_hint = "io-ops-humanoid-override-invalid-hint" (bone: Id)
        => "Choose another bone for { $bone }, or clear the override so the app can find one.";
    /// The report line for the `humanoid.override_refound` rule: what the import found.
    pub humanoid_override_refound = "io-ops-humanoid-override-refound" ()
        => "A bone you chose was replaced by the one the app found, because they agree.";
    /// The fix for the `humanoid.override_refound` rule, shown under its report line.
    pub humanoid_override_refound_hint = "io-ops-humanoid-override-refound-hint" ()
        => "Nothing to do; clear the override if you no longer need it.";
    /// The report line for the `humanoid.other_skeleton` rule: what the import found.
    pub humanoid_other_skeleton = "io-ops-humanoid-other-skeleton" ()
        => "This skeleton does not follow the usual human layout, so the app mapped it as best it could.";
    /// The fix for the `humanoid.other_skeleton` rule, shown under its report line.
    pub humanoid_other_skeleton_hint = "io-ops-humanoid-other-skeleton-hint" ()
        => "Check the humanoid mapping, and set overrides for any bone that is wrong.";
    /// The report line for the `humanoid.scale_invalid` rule: what the import found. `bone` is the name of the thing it is about, shown as it is.
    pub humanoid_scale_invalid = "io-ops-humanoid-scale-invalid" (bone: Id)
        => "The rest size of { $bone } is zero or flipped, so it cannot be mapped.";
    /// The fix for the `humanoid.scale_invalid` rule, shown under its report line. `bone` is the name of the thing it is about, shown as it is.
    pub humanoid_scale_invalid_hint = "io-ops-humanoid-scale-invalid-hint" (bone: Id)
        => "Fix the scale of { $bone } in your modelling tool and import again.";
    /// The report line for the `humanoid.sides_overlap` rule: what the import found.
    pub humanoid_sides_overlap = "io-ops-humanoid-sides-overlap" ()
        => "The left and right sides of the body overlap, so the app cannot tell them apart.";
    /// The fix for the `humanoid.sides_overlap` rule, shown under its report line.
    pub humanoid_sides_overlap_hint = "io-ops-humanoid-sides-overlap-hint" ()
        => "Check the bone names, or set overrides for the left and right bones.";
    /// The report line for the `avatar.expression_custom` rule: what the import found.
    pub avatar_expression_custom = "io-ops-avatar-expression-custom" ()
        => "This model has a custom expression that you can trigger from your own controls.";
    /// The fix for the `avatar.expression_custom` rule, shown under its report line.
    pub avatar_expression_custom_hint = "io-ops-avatar-expression-custom-hint" ()
        => "Nothing to do; bind the expression to a key or a hotkey if you want it.";
    /// The report line for the `avatar.thumb_off_axis` rule: what the import found. `bone` is the name of the thing it is about, shown as it is.
    pub avatar_thumb_off_axis = "io-ops-avatar-thumb-off-axis" (bone: Id)
        => "A thumb on { $bone } points away from the usual direction, so finger poses may look bent.";
    /// The fix for the `avatar.thumb_off_axis` rule, shown under its report line.
    pub avatar_thumb_off_axis_hint = "io-ops-avatar-thumb-off-axis-hint" ()
        => "Check the hand poses; fix the thumb in your modelling tool if they look wrong.";
    /// The report line for the `vrm.perfect_sync_complete` rule: what the import found.
    pub vrm_perfect_sync_complete = "io-ops-vrm-perfect-sync-complete" ()
        => "This model has the full set of face-tracking shapes, so every tracked expression has a target.";
    /// The fix for the `vrm.perfect_sync_complete` rule, shown under its report line.
    pub vrm_perfect_sync_complete_hint = "io-ops-vrm-perfect-sync-complete-hint" ()
        => "Nothing to do.";
    /// The report line for the `avatar.rest_normalised` rule: what the import found.
    pub avatar_rest_normalised = "io-ops-avatar-rest-normalised" ()
        => "The resting pose was straightened into a T-pose so the body maps cleanly.";
    /// The fix for the `avatar.rest_normalised` rule, shown under its report line.
    pub avatar_rest_normalised_hint = "io-ops-avatar-rest-normalised-hint" ()
        => "Nothing to do; check the pose in the preview, or fix the rest pose in your modelling tool.";
    /// The report line for the `avatar.weights_normalised` rule: what the import found.
    pub avatar_weights_normalised = "io-ops-avatar-weights-normalised" ()
        => "The skin weights of some vertices did not add up to one, so they were evened out.";
    /// The fix for the `avatar.weights_normalised` rule, shown under its report line.
    pub avatar_weights_normalised_hint = "io-ops-avatar-weights-normalised-hint" ()
        => "Nothing to do; if the mesh looks wrong, re-paint those weights in your modelling tool.";
    /// The report line for the `cleanup.welded` rule: what the import found.
    pub cleanup_welded = "io-ops-cleanup-welded" ()
        => "Vertices that sat on top of each other were joined.";
    /// The fix for the `cleanup.welded` rule, shown under its report line.
    pub cleanup_welded_hint = "io-ops-cleanup-welded-hint" ()
        => "Nothing to do; the mesh looks the same, with less to draw.";
    /// The report line for the `cleanup.merged` rule: what the import found.
    pub cleanup_merged = "io-ops-cleanup-merged" ()
        => "Meshes that share a material were merged into one.";
    /// The fix for the `cleanup.merged` rule, shown under its report line.
    pub cleanup_merged_hint = "io-ops-cleanup-merged-hint" ()
        => "Nothing to do; the model looks the same, drawn in fewer steps.";
    /// The report line for the `cleanup.dropped_unused` rule: what the import found.
    pub cleanup_dropped_unused = "io-ops-cleanup-dropped-unused" ()
        => "Parts that nothing used, such as spare materials or textures, were left out.";
    /// The fix for the `cleanup.dropped_unused` rule, shown under its report line.
    pub cleanup_dropped_unused_hint = "io-ops-cleanup-dropped-unused-hint" ()
        => "Nothing to do; they were not needed by the model.";
    /// The report line for the `arkit.synthesised` rule: what the import found. `channel` is the name of the thing it is about, shown as it is.
    pub arkit_synthesised = "io-ops-arkit-synthesised" (channel: Id)
        => "The shape for { $channel } was built from other shapes the model already has.";
    /// The fix for the `arkit.synthesised` rule, shown under its report line.
    pub arkit_synthesised_hint = "io-ops-arkit-synthesised-hint" ()
        => "Check it in the preview, or replace it with a shape you made yourself.";
    /// The report line for the `arkit.generated` rule: what the import found. `channel` is the name of the thing it is about, shown as it is.
    pub arkit_generated = "io-ops-arkit-generated" (channel: Id)
        => "A new shape for { $channel } was generated so face tracking can drive it.";
    /// The fix for the `arkit.generated` rule, shown under its report line.
    pub arkit_generated_hint = "io-ops-arkit-generated-hint" ()
        => "Check it in the preview, or replace it with a shape you made yourself.";
    /// The report line for the `vrm.collider_normal_normalised` rule: what the import found.
    pub vrm_collider_normal_normalised = "io-ops-vrm-collider-normal-normalised" ()
        => "A collider direction was rescaled to a length of one.";
    /// The fix for the `vrm.collider_normal_normalised` rule, shown under its report line.
    pub vrm_collider_normal_normalised_hint = "io-ops-vrm-collider-normal-normalised-hint" ()
        => "Nothing to do.";
    /// The report line for the `vrm.limit_clamped` rule: what the import found.
    pub vrm_limit_clamped = "io-ops-vrm-limit-clamped" ()
        => "A joint limit angle was past the largest value allowed and was reduced.";
    /// The fix for the `vrm.limit_clamped` rule, shown under its report line.
    pub vrm_limit_clamped_hint = "io-ops-vrm-limit-clamped-hint" ()
        => "Nothing to do; check the joint in the preview.";
    /// The report line for the `vrm.bind_weight_clamped` rule: what the import found.
    pub vrm_bind_weight_clamped = "io-ops-vrm-bind-weight-clamped" ()
        => "An expression binding weight was outside zero to one and was pulled back in.";
    /// The fix for the `vrm.bind_weight_clamped` rule, shown under its report line.
    pub vrm_bind_weight_clamped_hint = "io-ops-vrm-bind-weight-clamped-hint" ()
        => "Nothing to do; check the expression in the preview.";
    /// The report line for the `vrm.mtoon_queue_clamped` rule: what the import found.
    pub vrm_mtoon_queue_clamped = "io-ops-vrm-mtoon-queue-clamped" ()
        => "A draw-order value on a toon material was outside the allowed range and was pulled back in.";
    /// The fix for the `vrm.mtoon_queue_clamped` rule, shown under its report line.
    pub vrm_mtoon_queue_clamped_hint = "io-ops-vrm-mtoon-queue-clamped-hint" ()
        => "Nothing to do; check the material in the preview.";
    /// The report line for the `arkit.missing` rule: what the import found. `channel` is the name of the thing it is about, shown as it is.
    pub arkit_missing = "io-ops-arkit-missing" (channel: Id)
        => "This model has no shape for { $channel }, so face tracking cannot move it.";
    /// The fix for the `arkit.missing` rule, shown under its report line. `channel` is the name of the thing it is about, shown as it is.
    pub arkit_missing_hint = "io-ops-arkit-missing-hint" (channel: Id)
        => "Generate the shape, or map another expression to { $channel }.";
    /// The report line for the `arkit.gaze_inert` rule: what the import found.
    pub arkit_gaze_inert = "io-ops-arkit-gaze-inert" ()
        => "The eyes cannot follow your gaze, because the model has no eye bones or gaze shapes.";
    /// The fix for the `arkit.gaze_inert` rule, shown under its report line.
    pub arkit_gaze_inert_hint = "io-ops-arkit-gaze-inert-hint" ()
        => "Add eye bones or look shapes in your modelling tool, or map the gaze to another expression.";
    /// The report line for the `arkit.gaze_partial` rule: what the import found.
    pub arkit_gaze_partial = "io-ops-arkit-gaze-partial" ()
        => "The eyes follow your gaze in some directions only, because some look shapes are missing.";
    /// The fix for the `arkit.gaze_partial` rule, shown under its report line.
    pub arkit_gaze_partial_hint = "io-ops-arkit-gaze-partial-hint" ()
        => "Add the missing look shapes, or map them to other expressions.";
    /// The report line for the `avatar.expression_undriven` rule: what the import found. `expression` is the name of the thing it is about, shown as it is.
    pub avatar_expression_undriven = "io-ops-avatar-expression-undriven" (expression: Id)
        => "The expression { $expression } is never driven, so it will stay still.";
    /// The fix for the `avatar.expression_undriven` rule, shown under its report line. `expression` is the name of the thing it is about, shown as it is.
    pub avatar_expression_undriven_hint = "io-ops-avatar-expression-undriven-hint" (expression: Id)
        => "Map a tracking channel to { $expression }, or leave it for manual triggers.";
    /// The report line for the `look.mtoon_not_drawn` rule: what the import found.
    pub look_mtoon_not_drawn = "io-ops-look-mtoon-not-drawn" ()
        => "Some toon-shading settings on this material are not drawn by Omuzaki, so it looks a little different.";
    /// The fix for the `look.mtoon_not_drawn` rule, shown under its report line.
    pub look_mtoon_not_drawn_hint = "io-ops-look-mtoon-not-drawn-hint" ()
        => "Compare it in the preview, and adjust the material colours if it looks off.";
    /// The report line for the `material.not_drawn` rule: what the import found.
    pub material_not_drawn = "io-ops-material-not-drawn" ()
        => "Some material settings are not drawn by Omuzaki, so the material looks different.";
    /// The fix for the `material.not_drawn` rule, shown under its report line.
    pub material_not_drawn_hint = "io-ops-material-not-drawn-hint" ()
        => "Compare it in the preview, and adjust the material by hand if it looks off.";
    /// The report line for the `material.approximated` rule: what the import found.
    pub material_approximated = "io-ops-material-approximated" ()
        => "A material setting was approximated, because Omuzaki has no exact match.";
    /// The fix for the `material.approximated` rule, shown under its report line.
    pub material_approximated_hint = "io-ops-material-approximated-hint" ()
        => "Compare it in the preview, and adjust the material by hand if it looks off.";
    /// The report line for the `material.texture_missing` rule: what the import found.
    pub material_texture_missing = "io-ops-material-texture-missing" ()
        => "A material points to a texture that is not in the file, so it is drawn without it.";
    /// The fix for the `material.texture_missing` rule, shown under its report line.
    pub material_texture_missing_hint = "io-ops-material-texture-missing-hint" ()
        => "Put the texture back in your modelling tool and import again.";
    /// The report line for the `material.not_exported` rule: what the import found.
    pub material_not_exported = "io-ops-material-not-exported" ()
        => "A material setting has no place in the exported file, so it was left out.";
    /// The fix for the `material.not_exported` rule, shown under its report line.
    pub material_not_exported_hint = "io-ops-material-not-exported-hint" ()
        => "Export as VRM 1 to keep more of the material, or accept the plainer look.";
    /// The report line for the `material.outline_dropped` rule: what the import found.
    pub material_outline_dropped = "io-ops-material-outline-dropped" ()
        => "The outline of a material was left out.";
    /// The fix for the `material.outline_dropped` rule, shown under its report line.
    pub material_outline_dropped_hint = "io-ops-material-outline-dropped-hint" ()
        => "Add the outline again as a separate outline in the app, if you want one.";
    /// The report line for the `material.program_dropped` rule: what the import found.
    pub material_program_dropped = "io-ops-material-program-dropped" ()
        => "A custom shader on a material was left out, because it cannot run here.";
    /// The fix for the `material.program_dropped` rule, shown under its report line.
    pub material_program_dropped_hint = "io-ops-material-program-dropped-hint" ()
        => "Replace it with a standard or toon material.";
    /// The report line for the `material.skin_dropped` rule: what the import found.
    pub material_skin_dropped = "io-ops-material-skin-dropped" ()
        => "A skin-shading setting on a material was left out.";
    /// The fix for the `material.skin_dropped` rule, shown under its report line.
    pub material_skin_dropped_hint = "io-ops-material-skin-dropped-hint" ()
        => "Adjust the material colours by hand if the skin looks flat.";
    /// The report line for the `material.texture_colour_space` rule: what the import found.
    pub material_texture_colour_space = "io-ops-material-texture-colour-space" ()
        => "A texture was read in a different colour mode than its file says, so its colours may shift.";
    /// The fix for the `material.texture_colour_space` rule, shown under its report line.
    pub material_texture_colour_space_hint = "io-ops-material-texture-colour-space-hint" ()
        => "Check the colours in the preview, and re-save the texture with the right colour mode if they are off.";
    /// The report line for the `material.vertex_colours_ignored` rule: what the import found.
    pub material_vertex_colours_ignored = "io-ops-material-vertex-colours-ignored" ()
        => "The colours painted on the mesh vertices are ignored.";
    /// The fix for the `material.vertex_colours_ignored` rule, shown under its report line.
    pub material_vertex_colours_ignored_hint = "io-ops-material-vertex-colours-ignored-hint" ()
        => "Bake the colours into a texture in your modelling tool.";
    /// The report line for the `texture.unreadable` rule: what the import found.
    pub texture_unreadable = "io-ops-texture-unreadable" ()
        => "A texture could not be read, so the material is drawn without it.";
    /// The fix for the `texture.unreadable` rule, shown under its report line.
    pub texture_unreadable_hint = "io-ops-texture-unreadable-hint" ()
        => "Re-save the texture as a standard PNG or JPEG and import again.";
    /// The report line for the `texture.depth_reduced` rule: what the import found.
    pub texture_depth_reduced = "io-ops-texture-depth-reduced" ()
        => "A texture had more colour depth than the app uses, so it was reduced to 8 bits.";
    /// The fix for the `texture.depth_reduced` rule, shown under its report line.
    pub texture_depth_reduced_hint = "io-ops-texture-depth-reduced-hint" ()
        => "Nothing to do; the difference is rarely visible.";
    /// The report line for the `texture.anisotropy_dropped` rule: what the import found.
    pub texture_anisotropy_dropped = "io-ops-texture-anisotropy-dropped" ()
        => "A texture's sharpening at steep angles was left out.";
    /// The fix for the `texture.anisotropy_dropped` rule, shown under its report line.
    pub texture_anisotropy_dropped_hint = "io-ops-texture-anisotropy-dropped-hint" ()
        => "Nothing to do; textures look slightly softer at steep angles.";
    /// The report line for the `texture.sampler_approximated` rule: what the import found.
    pub texture_sampler_approximated = "io-ops-texture-sampler-approximated" ()
        => "A texture's wrapping or filtering mode was swapped for the nearest one the app has.";
    /// The fix for the `texture.sampler_approximated` rule, shown under its report line.
    pub texture_sampler_approximated_hint = "io-ops-texture-sampler-approximated-hint" ()
        => "Check the texture in the preview; fix tiling in your modelling tool if it looks wrong.";
    /// The report line for the `texture.unpremultiplied` rule: what the import found.
    pub texture_unpremultiplied = "io-ops-texture-unpremultiplied" ()
        => "A texture's transparency was stored in a way the app does not use, so it was converted.";
    /// The fix for the `texture.unpremultiplied` rule, shown under its report line.
    pub texture_unpremultiplied_hint = "io-ops-texture-unpremultiplied-hint" ()
        => "Check soft edges in the preview; re-save the texture if they look wrong.";
    /// The report line for the `avatar.bind_dropped` rule: what the import found. `expression` is the name of the thing it is about, shown as it is.
    pub avatar_bind_dropped = "io-ops-avatar-bind-dropped" (expression: Id)
        => "An expression had a link to { $expression } that could not be kept.";
    /// The fix for the `avatar.bind_dropped` rule, shown under its report line.
    pub avatar_bind_dropped_hint = "io-ops-avatar-bind-dropped-hint" ()
        => "Re-create the link in the expression editor.";
    /// The report line for the `avatar.spring_chain_unused` rule: what the import found.
    pub avatar_spring_chain_unused = "io-ops-avatar-spring-chain-unused" ()
        => "A physics chain that sways hair or cloth does not move anything, so it is not used.";
    /// The fix for the `avatar.spring_chain_unused` rule, shown under its report line.
    pub avatar_spring_chain_unused_hint = "io-ops-avatar-spring-chain-unused-hint" ()
        => "Check that the chain's bones exist, or remove the chain.";
    /// The report line for the `avatar.constraint_unused` rule: what the import found.
    pub avatar_constraint_unused = "io-ops-avatar-constraint-unused" ()
        => "A bone constraint points at nothing, so it is not used.";
    /// The fix for the `avatar.constraint_unused` rule, shown under its report line.
    pub avatar_constraint_unused_hint = "io-ops-avatar-constraint-unused-hint" ()
        => "Check the constraint's target bone, or remove the constraint.";
    /// The report line for the `avatar.morphs_dropped` rule: what the import found.
    pub avatar_morphs_dropped = "io-ops-avatar-morphs-dropped" ()
        => "Some blend shapes on a mesh were left out.";
    /// The fix for the `avatar.morphs_dropped` rule, shown under its report line.
    pub avatar_morphs_dropped_hint = "io-ops-avatar-morphs-dropped-hint" ()
        => "Keep the shapes you need on a mesh that the app can read, or accept the loss.";
    /// The report line for the `avatar.collider_unknown` rule: what the import found.
    pub avatar_collider_unknown = "io-ops-avatar-collider-unknown" ()
        => "A physics collider points at a bone that does not exist, so it is not used.";
    /// The fix for the `avatar.collider_unknown` rule, shown under its report line.
    pub avatar_collider_unknown_hint = "io-ops-avatar-collider-unknown-hint" ()
        => "Point the collider at an existing bone, or remove it.";
    /// The report line for the `avatar.material_edit_unknown` rule: what the import found.
    pub avatar_material_edit_unknown = "io-ops-avatar-material-edit-unknown" ()
        => "A material change you asked for names a material this model does not have.";
    /// The fix for the `avatar.material_edit_unknown` rule, shown under its report line.
    pub avatar_material_edit_unknown_hint = "io-ops-avatar-material-edit-unknown-hint" ()
        => "Check the material name in your import options.";
    /// The report line for the `avatar.channel_override_invalid` rule: what the import found. `channel` is the name of the thing it is about, shown as it is.
    pub avatar_channel_override_invalid = "io-ops-avatar-channel-override-invalid" (channel: Id)
        => "The channel you chose for { $channel } cannot be used.";
    /// The fix for the `avatar.channel_override_invalid` rule, shown under its report line.
    pub avatar_channel_override_invalid_hint = "io-ops-avatar-channel-override-invalid-hint" ()
        => "Choose another channel, or clear the override.";
    /// The report line for the `avatar.too_many_bones` rule: what the import found.
    pub avatar_too_many_bones = "io-ops-avatar-too-many-bones" ()
        => "The model has more bones than the app can use.";
    /// The fix for the `avatar.too_many_bones` rule, shown under its report line.
    pub avatar_too_many_bones_hint = "io-ops-avatar-too-many-bones-hint" ()
        => "Reduce the bones in your modelling tool, or use the bone reducer when you export.";
    /// The report line for the `avatar.too_many_meshes` rule: what the import found.
    pub avatar_too_many_meshes = "io-ops-avatar-too-many-meshes" ()
        => "The model has more meshes than the app can use.";
    /// The fix for the `avatar.too_many_meshes` rule, shown under its report line.
    pub avatar_too_many_meshes_hint = "io-ops-avatar-too-many-meshes-hint" ()
        => "Merge meshes in your modelling tool and import again.";
    /// The report line for the `avatar.skeleton_invalid` rule: what the import found.
    pub avatar_skeleton_invalid = "io-ops-avatar-skeleton-invalid" ()
        => "The skeleton is broken, for example a bone that loops back to itself, so it cannot be used.";
    /// The fix for the `avatar.skeleton_invalid` rule, shown under its report line.
    pub avatar_skeleton_invalid_hint = "io-ops-avatar-skeleton-invalid-hint" ()
        => "Fix the skeleton in your modelling tool and import again.";
    /// The report line for the `avatar.skin_invalid` rule: what the import found.
    pub avatar_skin_invalid = "io-ops-avatar-skin-invalid" ()
        => "A mesh's skin refers to bones that do not exist, so it cannot be used.";
    /// The fix for the `avatar.skin_invalid` rule, shown under its report line.
    pub avatar_skin_invalid_hint = "io-ops-avatar-skin-invalid-hint" ()
        => "Re-bind the mesh to its skeleton in your modelling tool.";
    /// The report line for the `avatar.material_missing` rule: what the import found.
    pub avatar_material_missing = "io-ops-avatar-material-missing" ()
        => "A part of the model is drawn with a material the file doesn't have, so it cannot be shown.";
    /// The fix for the `avatar.material_missing` rule, shown under its report line.
    pub avatar_material_missing_hint = "io-ops-avatar-material-missing-hint" ()
        => "Give that part a material in your modelling tool and export the model again.";
    /// The report line for the `avatar.rest_unread` rule: what the import found.
    pub avatar_rest_unread = "io-ops-avatar-rest-unread" ()
        => "The pose the model was sculpted in could not be read back, so it cannot be shown as authored.";
    /// The fix for the `avatar.rest_unread` rule, shown under its report line.
    pub avatar_rest_unread_hint = "io-ops-avatar-rest-unread-hint" ()
        => "This is a bug in Omuzaki; report it with the model file.";
    /// The report line for the `avatar.invalid` rule: what the import found.
    pub avatar_invalid = "io-ops-avatar-invalid" ()
        => "The avatar settings are not valid, so the import cannot go on.";
    /// The fix for the `avatar.invalid` rule, shown under its report line.
    pub avatar_invalid_hint = "io-ops-avatar-invalid-hint" ()
        => "Fix the settings named in the report, or start from the defaults.";
    /// The report line for the `avatar.schema_unknown` rule: what the import found.
    pub avatar_schema_unknown = "io-ops-avatar-schema-unknown" ()
        => "The avatar settings were written by a version of the app that this one does not know.";
    /// The fix for the `avatar.schema_unknown` rule, shown under its report line.
    pub avatar_schema_unknown_hint = "io-ops-avatar-schema-unknown-hint" ()
        => "Update the app, or re-create the settings with this version.";
    /// The report line for the `gltf.avatar_dropped` rule: what the import found.
    pub gltf_avatar_dropped = "io-ops-gltf-avatar-dropped" ()
        => "Avatar details such as expressions and physics cannot be stored in a plain glTF file, so they were left out.";
    /// The fix for the `gltf.avatar_dropped` rule, shown under its report line.
    pub gltf_avatar_dropped_hint = "io-ops-gltf-avatar-dropped-hint" ()
        => "Export as VRM to keep them.";
    /// The report line for the `gltf.toon_dropped` rule: what the import found.
    pub gltf_toon_dropped = "io-ops-gltf-toon-dropped" ()
        => "Toon shading cannot be stored in a plain glTF file, so it was left out.";
    /// The fix for the `gltf.toon_dropped` rule, shown under its report line.
    pub gltf_toon_dropped_hint = "io-ops-gltf-toon-dropped-hint" ()
        => "Export as VRM 1 to keep toon shading.";
    /// The report line for the `gltf.layout_unsupported` rule: what the import found.
    pub gltf_layout_unsupported = "io-ops-gltf-layout-unsupported" ()
        => "A VRM must be a single binary file, and this one is split into several files.";
    /// The fix for the `gltf.layout_unsupported` rule, shown under its report line.
    pub gltf_layout_unsupported_hint = "io-ops-gltf-layout-unsupported-hint" ()
        => "Pack the model into one .vrm file and import again.";
    /// The report line for the `vrm.annotation_dropped` rule: what the import found.
    pub vrm_annotation_dropped = "io-ops-vrm-annotation-dropped" ()
        => "A first-person annotation was left out.";
    /// The fix for the `vrm.annotation_dropped` rule, shown under its report line.
    pub vrm_annotation_dropped_hint = "io-ops-vrm-annotation-dropped-hint" ()
        => "Set the first-person view again in the app.";
    /// The report line for the `vrm.bind_dropped` rule: what the import found.
    pub vrm_bind_dropped = "io-ops-vrm-bind-dropped" ()
        => "Some expression links point at things that do not exist, so they were left out.";
    /// The fix for the `vrm.bind_dropped` rule, shown under its report line.
    pub vrm_bind_dropped_hint = "io-ops-vrm-bind-dropped-hint" ()
        => "Re-create the expressions you need in the expression editor.";
    /// The report line for the `vrm.collider_dropped` rule: what the import found.
    pub vrm_collider_dropped = "io-ops-vrm-collider-dropped" ()
        => "Some physics colliders were left out, because their shape or bone is not usable.";
    /// The fix for the `vrm.collider_dropped` rule, shown under its report line.
    pub vrm_collider_dropped_hint = "io-ops-vrm-collider-dropped-hint" ()
        => "Check the physics in the preview, and add colliders again if hair or cloth passes through the body.";
    /// The report line for the `vrm.collider_shapeless` rule: what the import found.
    pub vrm_collider_shapeless = "io-ops-vrm-collider-shapeless" ()
        => "A physics collider has no shape, so it is not used.";
    /// The fix for the `vrm.collider_shapeless` rule, shown under its report line.
    pub vrm_collider_shapeless_hint = "io-ops-vrm-collider-shapeless-hint" ()
        => "Give the collider a shape in your modelling tool, or remove it.";
    /// The report line for the `vrm.constraint_dropped` rule: what the import found.
    pub vrm_constraint_dropped = "io-ops-vrm-constraint-dropped" ()
        => "Some bone constraints were left out, because their target is not usable.";
    /// The fix for the `vrm.constraint_dropped` rule, shown under its report line.
    pub vrm_constraint_dropped_hint = "io-ops-vrm-constraint-dropped-hint" ()
        => "Check the joints in the preview, and fix the constraints in your modelling tool.";
    /// The report line for the `vrm.constraint_duplicate` rule: what the import found.
    pub vrm_constraint_duplicate = "io-ops-vrm-constraint-duplicate" ()
        => "Two constraints drive the same bone, so one was left out.";
    /// The fix for the `vrm.constraint_duplicate` rule, shown under its report line.
    pub vrm_constraint_duplicate_hint = "io-ops-vrm-constraint-duplicate-hint" ()
        => "Keep only one constraint per bone in your modelling tool.";
    /// The report line for the `vrm.constraint_invalid` rule: what the import found.
    pub vrm_constraint_invalid = "io-ops-vrm-constraint-invalid" ()
        => "A bone constraint is not valid, so it was left out.";
    /// The fix for the `vrm.constraint_invalid` rule, shown under its report line.
    pub vrm_constraint_invalid_hint = "io-ops-vrm-constraint-invalid-hint" ()
        => "Fix the constraint in your modelling tool and import again.";
    /// The report line for the `vrm.expression_name_invalid` rule: what the import found.
    pub vrm_expression_name_invalid = "io-ops-vrm-expression-name-invalid" ()
        => "An expression has a name that cannot be used, so it was left out.";
    /// The fix for the `vrm.expression_name_invalid` rule, shown under its report line.
    pub vrm_expression_name_invalid_hint = "io-ops-vrm-expression-name-invalid-hint" ()
        => "Rename the expression in your modelling tool and import again.";
    /// The report line for the `vrm.humanoid_unknown_bone` rule: what the import found.
    pub vrm_humanoid_unknown_bone = "io-ops-vrm-humanoid-unknown-bone" ()
        => "The file names a humanoid bone that the standard does not have, so it was left out.";
    /// The fix for the `vrm.humanoid_unknown_bone` rule, shown under its report line.
    pub vrm_humanoid_unknown_bone_hint = "io-ops-vrm-humanoid-unknown-bone-hint" ()
        => "Map the bone to a standard one in the humanoid overrides.";
    /// The report line for the `vrm.limit_invalid` rule: what the import found.
    pub vrm_limit_invalid = "io-ops-vrm-limit-invalid" ()
        => "A joint limit is not valid, so it was left out.";
    /// The fix for the `vrm.limit_invalid` rule, shown under its report line.
    pub vrm_limit_invalid_hint = "io-ops-vrm-limit-invalid-hint" ()
        => "Fix the limit in your modelling tool.";
    /// The report line for the `vrm.limit_on_tail_ignored` rule: what the import found.
    pub vrm_limit_on_tail_ignored = "io-ops-vrm-limit-on-tail-ignored" ()
        => "A joint limit on the last bone of a chain has nothing to limit, so it is ignored.";
    /// The fix for the `vrm.limit_on_tail_ignored` rule, shown under its report line.
    pub vrm_limit_on_tail_ignored_hint = "io-ops-vrm-limit-on-tail-ignored-hint" ()
        => "Nothing to do.";
    /// The report line for the `vrm.meta_licence_condition` rule: what the import found.
    pub vrm_meta_licence_condition = "io-ops-vrm-meta-licence-condition" ()
        => "A licence condition could not be written the way the file format allows, so it was saved as plain text.";
    /// The fix for the `vrm.meta_licence_condition` rule, shown under its report line.
    pub vrm_meta_licence_condition_hint = "io-ops-vrm-meta-licence-condition-hint" ()
        => "Read the licence text in the exported file, and check it still says what you mean.";
    /// The report line for the `vrm.meta_unknown_keys` rule: what the import found.
    pub vrm_meta_unknown_keys = "io-ops-vrm-meta-unknown-keys" ()
        => "The model's licence block has fields this app does not know, so they were kept but not used.";
    /// The fix for the `vrm.meta_unknown_keys` rule, shown under its report line.
    pub vrm_meta_unknown_keys_hint = "io-ops-vrm-meta-unknown-keys-hint" ()
        => "Read the licence of the model yourself before you perform with it.";
    /// The report line for the `vrm.mtoon_invalid` rule: what the import found.
    pub vrm_mtoon_invalid = "io-ops-vrm-mtoon-invalid" ()
        => "A toon material has settings that are not valid, so defaults were used.";
    /// The fix for the `vrm.mtoon_invalid` rule, shown under its report line.
    pub vrm_mtoon_invalid_hint = "io-ops-vrm-mtoon-invalid-hint" ()
        => "Check the material in the preview, and fix it in your modelling tool.";
    /// The report line for the `vrm.perfect_sync_partial` rule: what the import found.
    pub vrm_perfect_sync_partial = "io-ops-vrm-perfect-sync-partial" ()
        => "This model has only some of the face-tracking shapes, so the rest of the face will not move.";
    /// The fix for the `vrm.perfect_sync_partial` rule, shown under its report line.
    pub vrm_perfect_sync_partial_hint = "io-ops-vrm-perfect-sync-partial-hint" ()
        => "Add the missing shapes, or generate them.";
    /// The report line for the `vrm.spring_chain_dropped` rule: what the import found.
    pub vrm_spring_chain_dropped = "io-ops-vrm-spring-chain-dropped" ()
        => "Some hair or cloth physics chains were left out, because their bones are missing or out of order.";
    /// The fix for the `vrm.spring_chain_dropped` rule, shown under its report line.
    pub vrm_spring_chain_dropped_hint = "io-ops-vrm-spring-chain-dropped-hint" ()
        => "Check the physics in the preview, and fix the chains in your modelling tool.";
    /// The report line for the `vrm.spring_center_dropped` rule: what the import found.
    pub vrm_spring_center_dropped = "io-ops-vrm-spring-center-dropped" ()
        => "The centre of some physics chains was left out, because it is not the first bone or its parent.";
    /// The fix for the `vrm.spring_center_dropped` rule, shown under its report line.
    pub vrm_spring_center_dropped_hint = "io-ops-vrm-spring-center-dropped-hint" ()
        => "Set the centre to the first bone of the chain or its parent in your modelling tool.";
    /// The report line for the `vrm.tpose` rule: what the import found.
    pub vrm_tpose = "io-ops-vrm-tpose" ()
        => "The arms or legs are off the T-pose, so some apps may pose the body wrongly.";
    /// The fix for the `vrm.tpose` rule, shown under its report line.
    pub vrm_tpose_hint = "io-ops-vrm-tpose-hint" ()
        => "Put the model in a T-pose in your modelling tool before you export.";
    /// The report line for the `rights.avatar_permission` rule: what the import found.
    pub rights_avatar_permission = "io-ops-rights-avatar-permission" ()
        => "The model's licence limits who may use this avatar.";
    /// The fix for the `rights.avatar_permission` rule, shown under its report line.
    pub rights_avatar_permission_hint = "io-ops-rights-avatar-permission-hint" ()
        => "Read the licence, and ask the author if you are not sure you may perform with it.";
    /// The report line for the `rights.perform_non_profit` rule: what the import found.
    pub rights_perform_non_profit = "io-ops-rights-perform-non-profit" ()
        => "The model's licence allows performing only without making money.";
    /// The fix for the `rights.perform_non_profit` rule, shown under its report line.
    pub rights_perform_non_profit_hint = "io-ops-rights-perform-non-profit-hint" ()
        => "Read the licence before you monetise a stream with this model.";
    /// The report line for the `rights.credit_required` rule: what the import found.
    pub rights_credit_required = "io-ops-rights-credit-required" ()
        => "The model's licence requires you to credit its author.";
    /// The fix for the `rights.credit_required` rule, shown under its report line.
    pub rights_credit_required_hint = "io-ops-rights-credit-required-hint" ()
        => "Put the credit line shown in the report in your stream titles.";
    /// The report line for the `rights.content_restricted` rule: what the import found.
    pub rights_content_restricted = "io-ops-rights-content-restricted" ()
        => "The model's licence limits some kinds of content.";
    /// The fix for the `rights.content_restricted` rule, shown under its report line.
    pub rights_content_restricted_hint = "io-ops-rights-content-restricted-hint" ()
        => "Read the licence before you show violent, sexual or political content with this model.";
    /// The report line for the `rights.unrecognised_license_url` rule: what the import found.
    pub rights_unrecognised_license_url = "io-ops-rights-unrecognised-license-url" ()
        => "The model points to a licence this app does not recognise.";
    /// The fix for the `rights.unrecognised_license_url` rule, shown under its report line.
    pub rights_unrecognised_license_url_hint = "io-ops-rights-unrecognised-license-url-hint" ()
        => "Open the link and read it before you use the model.";
    /// The report line for the `rights.import_restricted` rule: what the import found.
    pub rights_import_restricted = "io-ops-rights-import-restricted" ()
        => "The model's licence forbids sharing or changing it.";
    /// The fix for the `rights.import_restricted` rule, shown under its report line.
    pub rights_import_restricted_hint = "io-ops-rights-import-restricted-hint" ()
        => "Do not export or share this model; keep it for your own use.";
    /// The report line for the `rights.other_license` rule: what the import found.
    pub rights_other_license = "io-ops-rights-other-license" ()
        => "The model points to more licence terms outside the file.";
    /// The fix for the `rights.other_license` rule, shown under its report line.
    pub rights_other_license_hint = "io-ops-rights-other-license-hint" ()
        => "Read those terms before you use the model.";
    /// The report line for the `rights.no_license_info` rule: what the import found.
    pub rights_no_license_info = "io-ops-rights-no-license-info" ()
        => "The model carries no licence information.";
    /// The fix for the `rights.no_license_info` rule, shown under its report line.
    pub rights_no_license_info_hint = "io-ops-rights-no-license-info-hint" ()
        => "Ask the author what you may do with it before you use it.";
}
