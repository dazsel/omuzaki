//! What the host learned about the machine (design section 5). The host re-reads these when a device or model event
//! arrives and sends [`Event::Probed`](crate::Event::Probed); a probe that does not answer is passed as its most
//! cautious value, so the flow never waits on one.

/// Whether the OS lets the app use a camera.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Permission {
    /// The person allowed camera access.
    Granted,
    /// The person refused it (the fix is in the OS settings).
    Denied,
    /// The OS has not been asked yet.
    Undecided,
}

/// The cameras found and the permission state.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Cameras {
    /// No camera at all.
    None,
    /// Exactly one camera, so there is nothing to pick.
    One(Permission),
    /// This many cameras; permission is not per camera, so listing them implies it was given.
    Several(u8),
}

impl Cameras {
    /// How many cameras there are.
    pub const fn count(self) -> u8 {
        match self {
            Cameras::None => 0,
            Cameras::One(_) => 1,
            Cameras::Several(n) => n,
        }
    }

    /// The permission state, `None` when there is no camera to ask about.
    pub const fn permission(self) -> Option<Permission> {
        match self {
            Cameras::One(p) => Some(p),
            Cameras::Several(0) | Cameras::None => None,
            Cameras::Several(_) => Some(Permission::Granted),
        }
    }

    /// True when the person has more than one camera to choose between.
    pub const fn needs_picker(self) -> bool {
        self.count() >= 2
    }

    /// True when the Webcam source could work now or after the OS answers.
    pub const fn webcam_possible(self) -> bool {
        matches!(self.permission(), Some(Permission::Granted | Permission::Undecided))
    }
}

/// Readiness of the webcam tracker model (`face_tracker@1`); it matters only to the Webcam source.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum TrackerModel {
    /// Installed and verified.
    Ready,
    /// Being fetched; the flow continues with the life layer.
    Downloading {
        /// Progress, 0 to 100.
        percent: u8,
    },
    /// Offline or failed verification.
    Unavailable,
}

/// Whether an iPhone is paired (its blendshapes need no webcam model).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum IPhone {
    /// Not paired.
    NotPaired,
    /// Paired and sending.
    Paired,
}

/// A way to send the picture to another program.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum OutputKind {
    /// Spout (Windows).
    Spout,
    /// Syphon (macOS).
    Syphon,
    /// NDI.
    Ndi,
    /// A virtual camera.
    VirtualCamera,
    /// Capturing the app's window.
    WindowCapture,
}

/// Whether to offer a language the OS did not pick.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum LanguageOffer {
    /// Follow the OS silently.
    None,
    /// Offer Simplified Chinese on a Traditional Chinese system.
    SimplifiedChineseOnTraditionalSystem,
}

/// Everything the flow reads from the machine.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Probes {
    /// Cameras and permission.
    pub cameras: Cameras,
    /// The webcam tracker model.
    pub tracker: TrackerModel,
    /// iPhone pairing.
    pub iphone: IPhone,
    /// The output the OS recommends; `None` leaves the Output step out.
    pub recommended_output: Option<OutputKind>,
    /// An assistant is configured.
    pub assistant: bool,
    /// The language offer.
    pub language: LanguageOffer,
    /// A project is open (a rerun), so Character offers Keep.
    pub project_open: bool,
    /// The privacy claim on Welcome is true (design section 9); read by the panel's copy, not by the flow.
    pub privacy_claim_true: bool,
}
