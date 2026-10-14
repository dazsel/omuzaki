//! Camera recipes from the golden manifest: the framing, lens and projection of a case, read from its `spec.camera`.

use omuzaki_core::glam::Vec3;
use omuzaki_render_api::{Camera, Projection};
use serde::Deserialize;

use super::RecipeError;

/// A golden case's `spec.camera`: a perspective lens looking from `position` at `target`, with three.js's up (+Y).
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
pub struct CameraSpec {
    /// Vertical field of view, degrees (three.js `PerspectiveCamera.fov`).
    pub fov: f32,
    /// Near plane, metres.
    pub near: f32,
    /// Far plane, metres.
    pub far: f32,
    /// Where the camera is, metres.
    pub position: [f32; 3],
    /// What it looks at, metres.
    pub target: [f32; 3],
}

impl CameraSpec {
    /// The render-api camera for this lens. Its aspect ratio comes from the render target, as every camera's does.
    ///
    /// # Errors
    /// [`RecipeError::Invalid`] if the lens is degenerate (a field of view outside `(0, 180)`, `near >= far`, ...).
    pub fn to_camera(&self) -> Result<Camera, RecipeError> {
        let projection = Projection::Perspective { fov_y_deg: self.fov, near: self.near, far: self.far };
        let camera = Camera::look_at(Vec3::from(self.position), Vec3::from(self.target), Vec3::Y, projection);
        camera.validate().map_err(|e| RecipeError::invalid("camera", e.to_string()))?;
        Ok(camera)
    }
}
