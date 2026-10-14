//! What a character can look at, and how the stage turns it into a world position.

use omuzaki_core::glam::Vec3;

/// Identifies an entity in the stage's scene (another character, a prop). The stage owns the numbering.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct EntityId(pub u64);

/// What the eyes aim at.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum GazeTarget {
    /// The viewer: the active camera.
    Camera,
    /// A fixed point, world space, metres.
    Point(Vec3),
    /// Another entity (an object or character), wherever the scene has it this frame.
    Entity(EntityId),
    /// No gaze target: the solver leaves the eye channels alone.
    Off,
}

/// How the eyes should chase a target, which sets how fast they ease.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum GazeKind {
    /// Ambient following (the camera): a relaxed ease.
    Follow,
    /// A deliberate look (a point or an entity): a quick, saccade-like ease.
    Driven,
}

/// A target resolved to a world position.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Resolved {
    /// Where to look, world space, metres.
    pub position: Vec3,
    /// How fast to chase it.
    pub kind: GazeKind,
}

/// The part of the stage the gaze needs: where things are this frame.
pub trait GazeScene {
    /// The active camera's world position, if there is one.
    fn camera_position(&self) -> Option<Vec3>;
    /// The world position of `id`, if it exists.
    fn entity_position(&self, id: EntityId) -> Option<Vec3>;
}

impl GazeTarget {
    /// The world position to look at, or `None` for [`GazeTarget::Off`] and for a camera or entity the scene doesn't
    /// have. [`GazeSolver::step`](super::GazeSolver::step) treats `None` as no gaze: it leaves the bus alone and
    /// restarts its easing from centre.
    pub fn resolve(&self, scene: &impl GazeScene) -> Option<Resolved> {
        let (position, kind) = match *self {
            GazeTarget::Camera => (scene.camera_position()?, GazeKind::Follow),
            GazeTarget::Point(p) => (p, GazeKind::Driven),
            GazeTarget::Entity(id) => (scene.entity_position(id)?, GazeKind::Driven),
            GazeTarget::Off => return None,
        };
        Some(Resolved { position, kind })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scene;
    impl GazeScene for Scene {
        fn camera_position(&self) -> Option<Vec3> {
            Some(Vec3::new(0.0, 1.0, 3.0))
        }
        fn entity_position(&self, id: EntityId) -> Option<Vec3> {
            (id == EntityId(7)).then_some(Vec3::X)
        }
    }

    #[test]
    fn each_target_resolves_to_its_position_and_kind() {
        let camera = GazeTarget::Camera.resolve(&Scene).map(|r| (r.position, r.kind));
        assert_eq!(camera, Some((Vec3::new(0.0, 1.0, 3.0), GazeKind::Follow)));
        let point = GazeTarget::Point(Vec3::Y).resolve(&Scene).map(|r| (r.position, r.kind));
        assert_eq!(point, Some((Vec3::Y, GazeKind::Driven)));
        let entity = GazeTarget::Entity(EntityId(7)).resolve(&Scene).map(|r| r.position);
        assert_eq!(entity, Some(Vec3::X));
    }

    #[test]
    fn off_and_missing_entities_resolve_to_nothing() {
        assert!(GazeTarget::Off.resolve(&Scene).is_none());
        assert!(GazeTarget::Entity(EntityId(8)).resolve(&Scene).is_none());
    }
}
