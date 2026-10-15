//! The `VRChat` performance rank (VF-07): every stat at each of its limits on both platforms, the overall rank,
//! Unsupported stats, bounds per axis, the texture estimate, and the counts of a gathered mock character.

// Test arithmetic on pixel and megabyte counts: the values are small enough that the casts are exact.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use std::collections::BTreeMap;
use std::io::Cursor;

use image::{DynamicImage, ImageFormat, RgbImage, RgbaImage};
use omuzaki_char_api::{BuildOptions, BuildServices, CharId, CharacterBuilder, RegistryBuilder};
use omuzaki_char_mock::mock_character;
use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_io_api::{Blob, EncodedImage, ImageMime, IoScene, Mesh, MeshData, Node, Primitive, Skin};
use omuzaki_io_ops::gather::avatar::add_avatar;
use omuzaki_io_ops::gather::{GatherOptions, GatherPurpose, gather};
use omuzaki_io_ops::vrchat_rank::{AvatarStats, Platform, Rank, rank, stats_from_ioscene, texture_mib};
use toml::Value;

const PC_LIMITS: &str = include_str!("../data/limits/vrchat-pc.toml");
const ANDROID_LIMITS: &str = include_str!("../data/limits/vrchat-quest.toml");
const MIB: f64 = 1024.0 * 1024.0;

/// The stats the page disables on Android. They are Unsupported there and never count.
const ANDROID_UNSUPPORTED: [&str; 6] =
    ["lights", "cloths", "cloth_vertices", "physics_colliders", "physics_rigidbodies", "audio_sources"];

/// The rows of one platform's table, keyed by stat name: the supported rows only, with the three bounds axes put back
/// together as one `bounds` row of `[x, y, z]` per tier.
fn table(platform: &str) -> BTreeMap<String, Value> {
    let text = if platform == "pc" { PC_LIMITS } else { ANDROID_LIMITS };
    let file: Value = toml::from_str(text).unwrap();
    let mut rows: BTreeMap<String, Value> = file["stats"]
        .as_table()
        .unwrap()
        .iter()
        .filter(|(_, v)| v.is_array())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let axes = ["bounds_x", "bounds_y", "bounds_z"].map(|key| rows.remove(key).unwrap());
    let per_tier = (0..4).map(|tier| Value::Array(axes.iter().map(|axis| axis[tier].clone()).collect())).collect();
    rows.insert("bounds".to_string(), Value::Array(per_tier));
    rows
}

/// A scalar row as four integers, or `None` for bounds (which are per axis).
fn scalar_row(value: &Value) -> Option<[i64; 4]> {
    let items = value.as_array()?;
    let ints: Vec<i64> = items.iter().map(Value::as_integer).collect::<Option<_>>()?;
    <[i64; 4]>::try_from(ints).ok()
}

fn rank_of(ranking: &omuzaki_io_ops::vrchat_rank::Ranking, name: &str) -> Option<Rank> {
    ranking.stats.iter().find(|s| s.name == name).unwrap().rank
}

/// The band a value lands in, written out from the page's rule: the first limit it fits under.
fn expected_rank(value: f64, limits: [i64; 4]) -> Rank {
    let bands = [Rank::Excellent, Rank::Good, Rank::Medium, Rank::Poor];
    bands.into_iter().zip(limits).find(|(_, limit)| value <= *limit as f64).map_or(Rank::VeryPoor, |(rank, _)| rank)
}

/// Sets one count-valued stat by its data-file name.
fn set(stats: &mut AvatarStats, name: &str, value: u64) {
    let field = match name {
        "triangles" => &mut stats.triangles,
        "skinned_meshes" => &mut stats.skinned_meshes,
        "basic_meshes" => &mut stats.basic_meshes,
        "material_slots" => &mut stats.material_slots,
        "physbones_components" => &mut stats.physbones_components,
        "physbones_transforms" => &mut stats.physbones_transforms,
        "physbones_colliders" => &mut stats.physbones_colliders,
        "physbones_collision_checks" => &mut stats.physbones_collision_checks,
        "contacts" => &mut stats.contacts,
        "constraints" => &mut stats.constraints,
        "constraint_depth" => &mut stats.constraint_depth,
        "animators" => &mut stats.animators,
        "bones" => &mut stats.bones,
        "lights" => &mut stats.lights,
        "particle_systems" => &mut stats.particle_systems,
        "total_particles" => &mut stats.total_particles,
        "mesh_particle_polys" => &mut stats.mesh_particle_polys,
        "particle_trails" => &mut stats.particle_trails,
        "particle_collisions" => &mut stats.particle_collisions,
        "trail_renderers" => &mut stats.trail_renderers,
        "line_renderers" => &mut stats.line_renderers,
        "raycasts" => &mut stats.raycasts,
        "cloths" => &mut stats.cloths,
        "cloth_vertices" => &mut stats.cloth_vertices,
        "physics_colliders" => &mut stats.physics_colliders,
        "physics_rigidbodies" => &mut stats.physics_rigidbodies,
        "audio_sources" => &mut stats.audio_sources,
        other => panic!("no count field for stat '{other}'"),
    };
    *field = value;
}

/// Sets the opaque texture pixels so the platform's estimate comes to `mib`.
fn set_texture(stats: &mut AvatarStats, platform: Platform, mib: f64) {
    let bytes_per_px = match platform {
        Platform::Pc => 0.5,
        Platform::Android => 3.56 / 8.0,
    };
    stats.texture_px_opaque = (mib * MIB / (bytes_per_px * 4.0 / 3.0)).round() as u64;
}

fn platform_key(platform: Platform) -> &'static str {
    match platform {
        Platform::Pc => "pc",
        Platform::Android => "android",
    }
}

#[test]
fn the_limits_file_parses_with_every_stat_on_pc_and_the_android_unsupported_set() {
    let pc = table("pc");
    let android = table("android");
    assert_eq!(pc.len(), 29, "PC: 28 scalar stats and bounds");
    assert_eq!(android.len(), pc.len() - ANDROID_UNSUPPORTED.len());
    for name in ANDROID_UNSUPPORTED {
        assert!(!android.contains_key(name), "{name} is unsupported on Android");
        assert!(pc.contains_key(name), "{name} has PC limits");
    }
    // A stat missing from the data file would read as Unsupported without a word, so check every stat is ranked on PC
    // and that Android leaves out exactly the page's disabled set.
    let pc_ranking = rank(&AvatarStats::default(), Platform::Pc).unwrap();
    assert_eq!(pc_ranking.stats.len(), pc.len());
    assert!(pc_ranking.stats.iter().all(|s| s.rank.is_some()), "every PC stat has limits");
    let android_ranking = rank(&AvatarStats::default(), Platform::Android).unwrap();
    let unsupported: Vec<&str> = android_ranking.stats.iter().filter(|s| s.rank.is_none()).map(|s| s.name).collect();
    assert_eq!(unsupported, ANDROID_UNSUPPORTED);
    // Spot-check transcriptions against the page's table, so a typo here fails loudly.
    assert_eq!(scalar_row(&pc["triangles"]), Some([32000, 70000, 70000, 70000]));
    assert_eq!(scalar_row(&pc["skinned_meshes"]), Some([1, 2, 8, 16]));
    assert_eq!(scalar_row(&android["triangles"]), Some([7500, 10000, 15000, 20000]));
    assert_eq!(scalar_row(&android["bones"]), Some([75, 90, 150, 150]));
    assert_eq!(scalar_row(&android["constraints"]), Some([30, 60, 120, 150]));
}

#[test]
fn every_stat_ranks_at_each_limit_and_one_past_it_on_both_platforms() {
    for platform in [Platform::Pc, Platform::Android] {
        let rows = table(platform_key(platform));
        for (name, row) in &rows {
            let Some(limits) = scalar_row(row) else {
                continue;
            };
            let mut values: Vec<i64> = limits.iter().flat_map(|&t| [t - 1, t, t + 1]).filter(|v| *v >= 0).collect();
            values.sort_unstable();
            values.dedup();
            for value in values {
                let mut stats = AvatarStats::default();
                let (measured, expected) = if name == "texture_mib" {
                    set_texture(&mut stats, platform, value as f64);
                    let measured = texture_mib(&stats, platform);
                    assert!((measured - value as f64).abs() < 1e-6, "texture estimate {measured} for {value} MiB");
                    (measured, expected_rank(measured, limits))
                } else {
                    set(&mut stats, name, value as u64);
                    (value as f64, expected_rank(value as f64, limits))
                };
                let ranking = rank(&stats, platform).unwrap();
                let ctx = format!("{} {name} at {measured}", platform_key(platform));
                assert_eq!(rank_of(&ranking, name), Some(expected), "{ctx}");
                // Every other stat is 0, which is Excellent everywhere, so the overall rank is this stat's rank.
                assert_eq!(ranking.rank, expected, "overall, {ctx}");
            }
        }
    }
}

#[test]
fn bounds_are_compared_per_axis_and_the_worst_axis_sets_the_rank() {
    let cases: [([f64; 3], Rank); 7] = [
        ([0.0, 0.0, 0.0], Rank::Excellent),
        ([2.5, 2.5, 2.5], Rank::Excellent),
        ([2.5, 2.6, 2.5], Rank::Good),
        ([4.0, 4.0, 4.0], Rank::Good),
        ([4.0, 4.0, 4.5], Rank::Medium),
        ([5.0, 6.0, 5.0], Rank::Medium),
        ([5.0, 6.0, 5.1], Rank::VeryPoor),
    ];
    for platform in [Platform::Pc, Platform::Android] {
        for (bounds, expected) in cases {
            let stats = AvatarStats { bounds, ..AvatarStats::default() };
            let ranking = rank(&stats, platform).unwrap();
            assert_eq!(rank_of(&ranking, "bounds"), Some(expected), "{} bounds {bounds:?}", platform_key(platform));
        }
    }
}

#[test]
fn stats_the_page_disables_on_android_are_unsupported_and_do_not_count() {
    let stats = AvatarStats {
        lights: 9,
        cloths: 9,
        cloth_vertices: 9999,
        physics_colliders: 9,
        physics_rigidbodies: 9,
        audio_sources: 99,
        ..AvatarStats::default()
    };
    let android = rank(&stats, Platform::Android).unwrap();
    for name in ANDROID_UNSUPPORTED {
        assert_eq!(rank_of(&android, name), None, "{name} is Unsupported on Android");
    }
    assert_eq!(android.rank, Rank::Excellent, "Unsupported stats do not raise the Android rank");
    let pc = rank(&stats, Platform::Pc).unwrap();
    assert_eq!(rank_of(&pc, "lights"), Some(Rank::VeryPoor), "the same counts are ranked on PC");
    assert_eq!(pc.rank, Rank::VeryPoor);
}

#[test]
fn the_overall_rank_is_the_worst_supported_stat() {
    let stats = AvatarStats { skinned_meshes: 2, bones: 120, animators: 3, ..AvatarStats::default() };
    // PC: skinned meshes 2 is Good; bones 120 is Good; animators 3 is Good.
    assert_eq!(rank(&stats, Platform::Pc).unwrap().rank, Rank::Good);
    // Android: skinned meshes 2 is Medium (limit 2), bones 120 is Medium (limit 150), animators 3 is Very Poor (limit 2).
    assert_eq!(rank(&stats, Platform::Android).unwrap().rank, Rank::VeryPoor);
}

#[test]
fn the_texture_estimate_uses_the_compressed_rates_and_a_mip_chain() {
    let stats = AvatarStats { texture_px_opaque: 1_000_000, texture_px_alpha: 1_000_000, ..AvatarStats::default() };
    // PC: 0.5 B/px opaque plus 1 B/px with alpha, times 4/3 for mips.
    let pc = (1_000_000.0 * 0.5 + 1_000_000.0 * 1.0) * 4.0 / 3.0 / MIB;
    assert!((texture_mib(&stats, Platform::Pc) - pc).abs() < 1e-9);
    // Android: ASTC 6x6 at 3.56 bits per pixel for both, times 4/3 for mips.
    let android = 2_000_000.0 * (3.56 / 8.0) * 4.0 / 3.0 / MIB;
    assert!((texture_mib(&stats, Platform::Android) - android).abs() < 1e-9);
}

fn png(width: u32, height: u32, alpha: bool) -> Vec<u8> {
    let image = if alpha {
        DynamicImage::ImageRgba8(RgbaImage::new(width, height))
    } else {
        DynamicImage::ImageRgb8(RgbImage::new(width, height))
    };
    let mut bytes = Vec::new();
    image.write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png).unwrap();
    bytes
}

#[test]
fn an_export_scene_counts_its_textures_from_the_image_headers() {
    let mut scene = IoScene::new();
    scene.add_image(EncodedImage::new("opaque", ImageMime::Png, Blob::new(png(10, 20, false))));
    scene.add_image(EncodedImage::new("masked", ImageMime::Png, Blob::new(png(4, 5, true))));
    let stats = stats_from_ioscene(&scene).unwrap();
    assert_eq!((stats.texture_px_opaque, stats.texture_px_alpha), (200, 20));
    assert_eq!(stats.triangles, 0, "an empty scene has no triangles");
}

#[test]
fn the_gathered_mock_character_counts_its_rig_and_meshes() {
    let def = mock_character().unwrap();
    let built = RegistryBuilder
        .build(&CharId::new("mira").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .unwrap();
    let mut gathered = gather(&built, &GatherOptions { target_height: None, purpose: GatherPurpose::Vrm }).unwrap();
    add_avatar(&mut gathered, &built).unwrap();
    let stats = stats_from_ioscene(&gathered.scene).unwrap();
    // The mock: five parts (head, hair, body, shirt, vest), four skinned and one rigid mesh node, one spring chain of
    // three hair bones over the head-and-neck group's two colliders, 17 humanoid bones and three hair bones. It has no
    // textures, and an export carries none of the particle, light, cloth or audio counts.
    assert_eq!(stats.triangles, 1060);
    assert_eq!((stats.skinned_meshes, stats.basic_meshes), (4, 1));
    assert_eq!(stats.material_slots, 5);
    assert_eq!(stats.bones, 20);
    assert_eq!(stats.animators, 1);
    assert_eq!((stats.physbones_components, stats.physbones_transforms), (1, 3));
    assert_eq!(stats.physbones_colliders, 2);
    assert_eq!(stats.physbones_collision_checks, 3 * 2, "three joints against the chain's two colliders");
    assert_eq!((stats.texture_px_opaque, stats.texture_px_alpha), (0, 0));
    assert!(stats.bounds.iter().all(|extent| *extent > 0.0), "a mock with meshes has a size on every axis");
    // Skinned 4 is Medium on PC (limit 8) and Very Poor on Android (limit 2); the rest sit in the better ranks.
    assert_eq!(rank(&stats, Platform::Pc).unwrap().rank, Rank::Medium);
    assert_eq!(rank(&stats, Platform::Android).unwrap().rank, Rank::VeryPoor);
}

/// A scene with one mesh of `triangles` triangles spanning `extent` metres along X, and nothing else.
fn mesh_scene(triangles: usize, extent: f32) -> (IoScene, omuzaki_io_api::MeshIx) {
    let mut scene = IoScene::new();
    let data = MeshData {
        positions: vec![Vec3::ZERO, Vec3::new(extent, 0.0, 0.0), Vec3::new(extent, 0.0, 0.0)],
        indices: [0, 1, 2].repeat(triangles),
        ..MeshData::default()
    };
    let mut mesh = Mesh::new("body");
    mesh.primitives.push(Primitive::new(data));
    let ix = scene.add_mesh(mesh);
    (scene, ix)
}

#[test]
fn a_mesh_shared_by_two_nodes_counts_its_triangles_twice() {
    // Each node is one instance: the page ranks the triangles the avatar draws, and a shared mesh is drawn twice.
    let (mut scene, mesh) = mesh_scene(20_000, 1.0);
    for name in ["left", "right"] {
        let mut node = Node::new(name);
        node.mesh = Some(mesh);
        let ix = scene.add_node(node);
        scene.roots.push(ix);
    }
    let stats = stats_from_ioscene(&scene).unwrap();
    assert_eq!(stats.triangles, 40_000);
    assert_eq!(stats.material_slots, 2, "one primitive per mesh node");
}

#[test]
fn bounds_follow_each_nodes_world_transform_down_its_parent_chain() {
    // A 3 m mesh under a parent scaled by 2 is 6 m in the world.
    let (mut scene, mesh) = mesh_scene(1, 3.0);
    let mut parent = Node::new("parent");
    parent.transform.scale = Vec3::splat(2.0);
    let parent = scene.add_node(parent);
    let mut child = Node::new("child");
    child.mesh = Some(mesh);
    let child = scene.add_node(child);
    scene.attach(parent, child);
    scene.roots.push(parent);
    let stats = stats_from_ioscene(&scene).unwrap();
    assert_eq!(stats.bounds, [6.0, 0.0, 0.0]);
}

/// A rig with one joint under the root, and a mesh node skinned to it. The mesh is 3 m along X, fully weighted to the
/// joint. The mesh node itself carries a transform, which a skinned mesh must ignore.
fn skinned_scene(joint_scale: f32, inverse_bind: Vec<Mat4>) -> IoScene {
    let mut scene = IoScene::new();
    let mut joint = Node::new("hips");
    joint.transform.scale = Vec3::splat(joint_scale);
    let joint = scene.add_node(joint);
    scene.roots.push(joint);
    let skin = scene.add_skin(Skin::new("rig", vec![joint], inverse_bind));
    let data = MeshData {
        positions: vec![Vec3::ZERO, Vec3::new(3.0, 0.0, 0.0), Vec3::new(3.0, 0.0, 0.0)],
        indices: [0, 1, 2].to_vec(),
        joints: vec![[0; 4]; 3],
        weights: vec![[1.0, 0.0, 0.0, 0.0]; 3],
        ..MeshData::default()
    };
    let mut mesh = Mesh::new("body");
    mesh.primitives.push(Primitive::new(data));
    let mesh = scene.add_mesh(mesh);
    let mut node = Node::new("body");
    node.mesh = Some(mesh);
    node.skin = Some(skin);
    node.transform.scale = Vec3::splat(10.0);
    let node = scene.add_node(node);
    scene.roots.push(node);
    scene
}

#[test]
fn a_skinned_mesh_is_placed_by_its_joint_not_its_node_transform() {
    // Joint scaled by 2 with no inverse bind matrices (identity, per the spec): the 3 m mesh is 6 m. The mesh node's own
    // scale of 10 is ignored, as glTF says a skinned mesh's node transform is.
    let scene = skinned_scene(2.0, Vec::new());
    assert_eq!(stats_from_ioscene(&scene).unwrap().bounds, [6.0, 0.0, 0.0]);
}

#[test]
fn a_bind_pose_different_from_the_rest_pose_places_the_vertices_by_the_bind_pose() {
    // The rest pose is the identity. The inverse bind matrix is for a bind pose scaled by 2, so the mesh (authored at the
    // bind pose) is shown at half size in the rest pose: 3 m becomes 1.5 m.
    let scene = skinned_scene(1.0, vec![Mat4::from_scale(Vec3::splat(0.5))]);
    assert_eq!(stats_from_ioscene(&scene).unwrap().bounds, [1.5, 0.0, 0.0]);
}
