//! Layered art: the way an illustration becomes a puppet. An artist's layers (exported from a painting tool as PNGs, or
//! a PSD) are listed in a manifest with their place on the canvas and, where the name doesn't already say, what each
//! one is; [`crate::autorig`] turns them into a rigged puppet.
//!
//! The manifest is TOML (`*.layers.toml`), back to front:
//!
//! ```toml
//! name = "lumi"
//! canvas = [2048, 4096]
//!
//! [[layer]]
//! name = "hair back"          # the role is read from the name when `role` is absent
//! file = "hair_back.png"      # beside the manifest; full-canvas, or cropped with `offset`
//!
//! [[layer]]
//! name = "eyes squeeze"
//! file = "eyes_squeeze.png"
//! role = "expression"
//! show_when = "eyesSqueeze"   # a channel: shown as it rises (and `hide_when` hides a layer)
//! ```

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// What a layer is, which decides how the auto-rig moves it.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Role {
    /// The torso and clothes: sways with the body and breathes.
    Body,
    /// The neck: between the body and the head.
    Neck,
    /// The face's skin: the head's base surface.
    Face,
    /// An ear (left or right of the picture).
    Ear,
    /// Hair behind the head.
    HairBack,
    /// Hair at the sides, in front of the ears.
    HairSide,
    /// Bangs and anything in front of the face.
    HairFront,
    /// A brow.
    Brow,
    /// The white of an eye: also the mask for its iris.
    EyeWhite,
    /// An iris and pupil: follows the gaze, clipped by the eye white.
    Iris,
    /// The upper lash line and lid: slides down on a blink.
    EyeLine,
    /// The lower lash line.
    EyeLower,
    /// A closed eye, shown as the eye shuts.
    EyeClosed,
    /// The closed mouth.
    Mouth,
    /// The open mouth (inside, teeth, tongue), shown as the jaw opens.
    MouthOpen,
    /// The nose.
    Nose,
    /// Blush, shadows and marks painted on the face.
    FaceDetail,
    /// A layer an expression channel shows or hides.
    Expression,
    /// Anything else on the head (hats, horns, hair ornaments).
    HeadAccessory,
    /// Anything else on the body (necklaces, collars).
    BodyAccessory,
    /// A layer that only sits there.
    Static,
}

impl Role {
    /// The role a layer's name suggests, by the words painters use.
    #[must_use]
    pub fn from_name(name: &str) -> Self {
        let n = name.to_ascii_lowercase().replace(['_', '-', '.'], " ");
        let has = |words: &[&str]| words.iter().any(|w| n.split_whitespace().any(|t| t == *w) || n.contains(w));
        if has(&["closed eye", "eye closed", "eyeclosed", "blink"]) {
            Self::EyeClosed
        } else if has(&["iris", "pupil", "eyeball"]) {
            Self::Iris
        } else if has(&["eye white", "eyewhite", "sclera"]) {
            Self::EyeWhite
        } else if has(&["lower eye", "eye lower", "lower lash", "lower lid"]) {
            Self::EyeLower
        } else if has(&["lash", "eyeline", "eye line", "upper lid", "lid"]) {
            Self::EyeLine
        } else if has(&["brow"]) {
            Self::Brow
        } else if has(&["mouth open", "open mouth", "teeth", "tongue", "mouth inner"]) {
            Self::MouthOpen
        } else if has(&["mouth", "lip"]) {
            Self::Mouth
        } else if has(&["nose"]) {
            Self::Nose
        } else if has(&["blush", "cheek", "shadow", "mark"]) {
            Self::FaceDetail
        } else if has(&["back hair", "hair back", "backhair"]) {
            Self::HairBack
        } else if has(&["side hair", "hair side", "sidehair"]) {
            Self::HairSide
        } else if has(&["bang", "fringe", "front hair", "hair front", "fronthair", "hair"]) {
            Self::HairFront
        } else if has(&["ear"]) {
            Self::Ear
        } else if has(&["face", "head", "skin"]) {
            Self::Face
        } else if has(&["neck"]) {
            Self::Neck
        } else if has(&["body", "torso", "chest", "shirt", "dress", "cloth", "arm", "shoulder"]) {
            Self::Body
        } else {
            Self::Static
        }
    }

    /// Whether the auto-rig hangs this layer on the head.
    #[must_use]
    pub const fn on_head(self) -> bool {
        !matches!(self, Self::Body | Self::Neck | Self::BodyAccessory | Self::Static)
    }

    /// How far in front of the face's surface the layer sits, in head radii (negative is behind), for the head turn.
    #[must_use]
    pub const fn depth(self) -> f32 {
        match self {
            Self::HairBack => -0.4,
            Self::Ear => -0.35,
            Self::HairSide => -0.1,
            Self::Face
            | Self::FaceDetail
            | Self::Expression
            | Self::Static
            | Self::Body
            | Self::Neck
            | Self::BodyAccessory => 0.0,
            Self::EyeWhite | Self::Iris | Self::EyeLower | Self::EyeClosed => 0.02,
            Self::EyeLine | Self::Brow | Self::Mouth | Self::MouthOpen => 0.05,
            Self::Nose => 0.15,
            Self::HeadAccessory => 0.2,
            Self::HairFront => 0.25,
        }
    }
}

/// One layer as the manifest lists it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayerDef {
    /// Its name.
    pub name: String,
    /// Its image, relative to the manifest.
    pub file: String,
    /// Where the image's top-left corner sits on the canvas, pixels.
    #[serde(default)]
    pub offset: [i32; 2],
    /// What it is; read from the name when absent.
    #[serde(default)]
    pub role: Option<Role>,
    /// Which side of the picture it is on, for paired features; read from the name or its position when absent.
    #[serde(default)]
    pub side: Option<Side>,
    /// A depth override, in head radii (see [`Role::depth`]).
    #[serde(default)]
    pub depth: Option<f32>,
    /// How it blends.
    #[serde(default)]
    pub blend: omuzaki_puppet_api::BlendMode,
    /// Its opacity.
    #[serde(default = "one")]
    pub opacity: f32,
    /// A channel that shows it as it rises (`"eyesSqueeze"`, `"mouthSmileLeft"`).
    #[serde(default)]
    pub show_when: Option<String>,
    /// A channel that hides it as it rises.
    #[serde(default)]
    pub hide_when: Option<String>,
    /// The features an expression layer stands in for while it shows (`"eyes"`, `"mouth"`, `"brows"`): they hide.
    /// When absent, a layer shown by an eye-shape channel (`eyes*`, `eyeArc*`) replaces the eyes.
    #[serde(default)]
    pub replaces: Option<Vec<String>>,
}

impl LayerDef {
    /// The features this layer stands in for while its `show_when` channel is up.
    #[must_use]
    pub fn replaced(&self) -> Vec<String> {
        if let Some(r) = &self.replaces {
            return r.clone();
        }
        match &self.show_when {
            Some(c) if c.starts_with("eyes") || c.starts_with("eyeArc") => vec!["eyes".into()],
            _ => Vec::new(),
        }
    }
}

const fn one() -> f32 {
    1.0
}

/// Left or right of the picture (the character's right is the picture's left).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    /// The picture's left: the character's right.
    Left,
    /// The picture's right: the character's left.
    Right,
}

/// The manifest.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayersManifest {
    /// The puppet's name.
    pub name: String,
    /// The canvas in pixels.
    pub canvas: [u32; 2],
    /// The artist, if known.
    #[serde(default)]
    pub artist: Option<String>,
    /// The layers, back to front.
    #[serde(rename = "layer")]
    pub layers: Vec<LayerDef>,
}

/// A layer with its pixels.
#[derive(Clone, Debug)]
pub struct Layer {
    /// What the manifest says about it.
    pub def: LayerDef,
    /// Its role, decided.
    pub role: Role,
    /// Its image's size.
    pub size: [u32; 2],
    /// Straight RGBA8, row-major.
    pub rgba: Vec<u8>,
}

/// Layered art, loaded.
#[derive(Clone, Debug)]
pub struct LayeredArt {
    /// The manifest.
    pub manifest: LayersManifest,
    /// The layers, back to front.
    pub layers: Vec<Layer>,
}

/// Why layered art didn't load.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LayersError {
    /// The manifest couldn't be read or parsed.
    #[error("the layers manifest {path}: {detail}")]
    Manifest {
        /// The manifest.
        path: PathBuf,
        /// What went wrong.
        detail: String,
    },
    /// A layer's image couldn't be read.
    #[error("layer {name:?} ({path}): {detail}")]
    Image {
        /// The layer.
        name: String,
        /// Its file.
        path: PathBuf,
        /// What went wrong.
        detail: String,
    },
    /// The art breaks a limit.
    #[error("{0}")]
    Limit(String),
}

/// The largest canvas side, pixels.
pub const MAX_SIDE: u32 = 16_384;
/// The most layers.
pub const MAX_LAYERS: usize = 1024;

impl LayeredArt {
    /// Loads the manifest at `path` and every layer it names (relative to its folder).
    ///
    /// # Errors
    /// [`LayersError`] for an unreadable manifest or image, or art past [`MAX_SIDE`] or [`MAX_LAYERS`].
    pub fn load(path: &Path) -> Result<Self, LayersError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| LayersError::Manifest { path: path.to_owned(), detail: e.to_string() })?;
        let manifest: LayersManifest = toml::from_str(&text)
            .map_err(|e| LayersError::Manifest { path: path.to_owned(), detail: e.to_string() })?;
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        Self::from_manifest(manifest, |file| {
            let p = dir.join(file);
            let img = image::ImageReader::open(&p)
                .map_err(|e| e.to_string())
                .and_then(|r| r.with_guessed_format().map_err(|e| e.to_string()))
                .and_then(|r| r.decode().map_err(|e| e.to_string()))?
                .into_rgba8();
            Ok(([img.width(), img.height()], img.into_raw()))
        })
    }

    /// Parses the manifest `text` and reads every layer it names through `read`, which gives the bytes of an image
    /// file by its path relative to the manifest (PNG, or any format the `image` crate reads). So a host reads the
    /// files through its own file access.
    ///
    /// # Errors
    /// As [`LayeredArt::load`].
    pub fn from_toml_with(
        text: &str,
        mut read: impl FnMut(&str) -> Result<Vec<u8>, String>,
    ) -> Result<Self, LayersError> {
        let manifest: LayersManifest = toml::from_str(text)
            .map_err(|e| LayersError::Manifest { path: PathBuf::from("layers.toml"), detail: e.to_string() })?;
        Self::from_manifest(manifest, |file| {
            if file.contains("..") || Path::new(file).is_absolute() {
                return Err("the layer's file leaves the manifest's folder".to_owned());
            }
            let img = image::load_from_memory(&read(file)?).map_err(|e| e.to_string())?.into_rgba8();
            Ok(([img.width(), img.height()], img.into_raw()))
        })
    }

    /// Builds layered art from a manifest, reading each layer's straight RGBA8 pixels with `read` (given the layer's
    /// `file`).
    ///
    /// # Errors
    /// As [`LayeredArt::load`].
    pub fn from_manifest(
        manifest: LayersManifest,
        mut read: impl FnMut(&str) -> Result<([u32; 2], Vec<u8>), String>,
    ) -> Result<Self, LayersError> {
        let [cw, ch] = manifest.canvas;
        if cw == 0 || ch == 0 || cw > MAX_SIDE || ch > MAX_SIDE {
            return Err(LayersError::Limit(format!("a canvas is 1 to {MAX_SIDE} pixels each way")));
        }
        if manifest.layers.len() > MAX_LAYERS {
            return Err(LayersError::Limit(format!("at most {MAX_LAYERS} layers")));
        }
        let mut layers = Vec::with_capacity(manifest.layers.len());
        for def in &manifest.layers {
            let (size, rgba) = read(&def.file).map_err(|detail| LayersError::Image {
                name: def.name.clone(),
                path: PathBuf::from(&def.file),
                detail,
            })?;
            if size[0] > MAX_SIDE || size[1] > MAX_SIDE || rgba.len() != (size[0] as usize) * (size[1] as usize) * 4 {
                return Err(LayersError::Limit(format!("layer {:?}: a bad or oversized image", def.name)));
            }
            let role = def.role.unwrap_or_else(|| Role::from_name(&def.name));
            layers.push(Layer { def: def.clone(), role, size, rgba });
        }
        Ok(Self { manifest, layers })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_say_what_layers_are() {
        for (name, role) in [
            ("Hair Back", Role::HairBack),
            ("front_hair_2", Role::HairFront),
            ("bangs", Role::HairFront),
            ("Eye White L", Role::EyeWhite),
            ("iris R", Role::Iris),
            ("upper lash L", Role::EyeLine),
            ("lower lash L", Role::EyeLower),
            ("eye closed L", Role::EyeClosed),
            ("brow left", Role::Brow),
            ("mouth open", Role::MouthOpen),
            ("teeth", Role::MouthOpen),
            ("mouth", Role::Mouth),
            ("blush", Role::FaceDetail),
            ("face", Role::Face),
            ("neck", Role::Neck),
            ("body", Role::Body),
            ("background", Role::Static),
        ] {
            assert_eq!(Role::from_name(name), role, "{name}");
        }
    }

    #[test]
    fn a_manifest_parses_and_checks_its_images() {
        let text = r#"
            name = "t"
            canvas = [4, 4]
            [[layer]]
            name = "face"
            file = "face.png"
            [[layer]]
            name = "squeeze"
            file = "sq.png"
            role = "expression"
            show_when = "eyesSqueeze"
        "#;
        let manifest: LayersManifest = toml::from_str(text).unwrap_or_else(|e| panic!("{e}"));
        let art = LayeredArt::from_manifest(manifest.clone(), |_| Ok(([2, 2], vec![255; 16])));
        let roles: Vec<Role> = art.map(|a| a.layers.iter().map(|l| l.role).collect()).unwrap_or_default();
        assert_eq!(roles, vec![Role::Face, Role::Expression]);
        let short = LayeredArt::from_manifest(manifest, |_| Ok(([2, 2], vec![255; 15])));
        assert!(matches!(short, Err(LayersError::Limit(_))));
    }
}
