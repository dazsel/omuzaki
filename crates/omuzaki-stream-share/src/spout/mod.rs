//! Spout2 (Windows): the picture as a shared D3D11 texture that OBS's Spout2 Capture source and other apps open by name.
//!
//! The sender side of the protocol is written here from its documented layout (Spout2 2.007, BSD-2-Clause, whose
//! notice we keep), with no SDK code compiled in; `spikes/broadcast-out` proved that a receiver written the same way
//! reads what the SDK's sender publishes, and that OBS opens exactly this kind of handle. A sender is:
//!
//! | Object | Name | Contents |
//! |---|---|---|
//! | file mapping | `SpoutSenderNames` | every sender's name, one 256-byte slot each, ended by an empty slot |
//! | file mapping | `ActiveSenderName` | the name a receiver picks when it is given none |
//! | file mapping | the sender's name | its [`info_block`]: share handle, size, DXGI format (280 bytes) |
//! | mutex | `<mapping>_mutex` | guards each mapping above |
//! | mutex | `<name>_SpoutAccessMutex` | held while the texture is written, so a receiver that takes it never reads half a frame |
//! | semaphore | `<name>_Count_Semaphore` | released once per frame, so a receiver can tell a new frame from a repeat |
//! | texture | (the share handle) | BGRA8, created with `D3D11_RESOURCE_MISC_SHARED`, opened with `OpenSharedResource` |
//!
//! The frame is uploaded into a private texture first and then copied into the shared one on the GPU, and the sender
//! waits for that copy before it lets go of the access mutex and counts the frame: the spike measured torn frames
//! without the wait, and OBS, which reads on its own clock without the mutex, sees only the copy's microseconds.

#[cfg(windows)]
mod win;

use std::sync::Arc;

use omuzaki_core::glam::UVec2;
use omuzaki_stream_api::error::StreamError;
use omuzaki_stream_api::output::{AlphaNeed, OutputConfig};

use crate::share::{Opener, Publisher, Share};

/// The registered output kind.
pub const KIND: &str = "spout";
/// The mapping that lists every sender.
pub const SENDER_NAMES: &str = "SpoutSenderNames";
/// The mapping that names the active sender.
pub const ACTIVE_SENDER: &str = "ActiveSenderName";
/// One name slot, terminator included: the longest name is one byte shorter.
pub const NAME_BYTES: usize = 256;
/// How many senders the list holds when this sender creates it (Spout 2.007's default).
pub const MAX_SENDERS: usize = 64;
/// The size of a sender's info block (`SharedTextureInfo`).
pub const INFO_BYTES: usize = 280;
/// `DXGI_FORMAT_B8G8R8A8_UNORM`, the format OBS's Spout2 source and the spike read.
pub const FORMAT_BGRA8: u32 = 87;

/// The idle `spout` output: premultiplied frames, published by a Spout sender.
pub fn output() -> Share {
    Share::new(KIND, AlphaNeed::Premultiplied, Arc::new(open) as Opener)
}

/// Whether this computer can run a Spout sender: any Windows; elsewhere a hint to the route that works there.
///
/// # Errors
/// [`StreamError::Unavailable`] on any OS but Windows.
pub fn available() -> Result<(), StreamError> {
    if cfg!(windows) {
        Ok(())
    } else {
        Err(StreamError::Unavailable {
            what: "Spout".into(),
            hint: "Spout is for Windows; on a Mac use the Syphon output, elsewhere the virtual camera or the output \
                   window"
                .into(),
        })
    }
}

/// Opens a Spout sender for `cfg`: its name, as receivers will list it, and its size.
fn open(cfg: &OutputConfig) -> Result<Box<dyn Publisher>, StreamError> {
    available()?;
    check_name(&cfg.name)?;
    #[cfg(windows)]
    {
        Ok(Box::new(win::Sender::open(&cfg.name, cfg.size)?))
    }
    #[cfg(not(windows))]
    {
        Err(StreamError::Unavailable { what: "Spout".into(), hint: "Spout is for Windows".into() })
    }
}

/// Checks that `name` can be a Spout sender's name: printable ASCII (receivers read the slots as ANSI text and open
/// the mapping by it), no backslash (a kernel object name's namespace separator), and short enough for a slot.
///
/// # Errors
/// [`StreamError::Config`] saying what to change.
pub fn check_name(name: &str) -> Result<(), StreamError> {
    if name.is_empty() || name.len() >= NAME_BYTES {
        return Err(StreamError::Config(format!("a Spout name is 1 to {} characters", NAME_BYTES - 1)));
    }
    if let Some(bad) = name.chars().find(|c| !(c.is_ascii_graphic() || *c == ' ') || *c == '\\') {
        return Err(StreamError::Config(format!(
            "a Spout name is plain letters, digits, spaces and punctuation other than a backslash; {bad:?} is not"
        )));
    }
    Ok(())
}

/// The names in a `SpoutSenderNames` list: each slot up to its first zero, until the first empty slot.
pub fn read_names(list: &[u8]) -> Vec<String> {
    list.as_chunks::<NAME_BYTES>()
        .0
        .iter()
        .map(|slot| &slot[..slot.iter().position(|&b| b == 0).unwrap_or(slot.len())])
        .take_while(|name| !name.is_empty())
        .map(|name| String::from_utf8_lossy(name).into_owned())
        .collect()
}

/// Writes `names` into a `SpoutSenderNames` list, one zero-padded slot each, and empties the rest.
///
/// # Errors
/// [`StreamError::Config`] when the list has no slot left for the names and the empty slot that ends them.
pub fn write_names(list: &mut [u8], names: &[String]) -> Result<(), StreamError> {
    let slots = list.len() / NAME_BYTES;
    if names.len() >= slots {
        return Err(StreamError::Config(format!(
            "Spout already lists {} senders, its most; stop one in another app first",
            slots.saturating_sub(1)
        )));
    }
    list.fill(0);
    for (slot, name) in list.as_chunks_mut::<NAME_BYTES>().0.iter_mut().zip(names) {
        let bytes = &name.as_bytes()[..name.len().min(NAME_BYTES - 1)];
        slot[..bytes.len()].copy_from_slice(bytes);
    }
    Ok(())
}

/// Writes `name` as the `ActiveSenderName` (one zero-padded slot; an empty name clears it).
pub fn write_active(slot: &mut [u8], name: &str) {
    slot.fill(0);
    let len = name.len().min(slot.len().saturating_sub(1));
    slot[..len].copy_from_slice(&name.as_bytes()[..len]);
}

/// A sender's `SharedTextureInfo`: the 32-bit share handle, width, height and DXGI format, then `usage` (unused,
/// zero), a 256-byte description (the sending program's path, as Spout 2.007 writes it) and `partnerId` (zero: a
/// texture sender, not Spout's CPU-memory mode). Little-endian, as on every Windows machine.
pub fn info_block(handle: u32, size: UVec2, host: &str) -> [u8; INFO_BYTES] {
    let mut info = [0u8; INFO_BYTES];
    for (at, word) in [(0, handle), (4, size.x), (8, size.y), (12, FORMAT_BGRA8), (16, 0)] {
        info[at..at + 4].copy_from_slice(&word.to_le_bytes());
    }
    let host: Vec<u8> = host
        .chars()
        .map(|c| u8::try_from(c).ok().filter(|b| b.is_ascii() && !b.is_ascii_control()).unwrap_or(b'?'))
        .collect();
    let len = host.len().min(NAME_BYTES - 1);
    info[20..20 + len].copy_from_slice(&host[..len]);
    info
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn names_round_trip_through_the_list() {
        let mut list = vec![0xAA; NAME_BYTES * 4];
        write_names(&mut list, &names(&["OBS Test", "Omuzaki"])).expect("they fit");
        assert_eq!(read_names(&list), names(&["OBS Test", "Omuzaki"]));
        assert_eq!(&list[..8], b"OBS Test");
        assert!(list[8..NAME_BYTES].iter().all(|&b| b == 0), "a slot is zero-padded");
        assert!(list[NAME_BYTES * 2..].iter().all(|&b| b == 0), "the rest is emptied");
    }

    #[test]
    fn reading_stops_at_the_first_empty_slot() {
        let mut list = vec![0u8; NAME_BYTES * 3];
        list[..3].copy_from_slice(b"One");
        list[NAME_BYTES * 2..NAME_BYTES * 2 + 5].copy_from_slice(b"Stale");
        assert_eq!(read_names(&list), names(&["One"]));
    }

    #[test]
    fn a_full_list_refuses_another_name() {
        let mut list = vec![0u8; NAME_BYTES * 2];
        assert!(write_names(&mut list, &names(&["One"])).is_ok());
        let err = write_names(&mut list, &names(&["One", "Two"])).expect_err("no slot for the terminator");
        assert!(matches!(err, StreamError::Config(_)), "{err:?}");
    }

    #[test]
    fn the_active_name_fills_one_slot_and_can_be_cleared() {
        let mut slot = [0xAAu8; NAME_BYTES];
        write_active(&mut slot, "Omuzaki");
        assert_eq!(read_names(&slot), names(&["Omuzaki"]));
        write_active(&mut slot, "");
        assert!(slot.iter().all(|&b| b == 0));
    }

    #[test]
    fn the_info_block_is_spouts_layout() {
        let info = info_block(0x4000_2C82, UVec2::new(1920, 1080), "C:\\Omuzaki\\omuzaki.exe");
        assert_eq!(info.len(), 280);
        let word = |at: usize| u32::from_le_bytes([info[at], info[at + 1], info[at + 2], info[at + 3]]);
        assert_eq!((word(0), word(4), word(8), word(12), word(16)), (0x4000_2C82, 1920, 1080, 87, 0));
        assert_eq!(&info[20..42], b"C:\\Omuzaki\\omuzaki.exe");
        assert_eq!(word(276), 0, "partnerId: a texture sender");
    }

    #[test]
    fn a_non_ascii_host_path_is_kept_readable() {
        let info = info_block(1, UVec2::ONE, "C:\\Ü\\o.exe");
        assert_eq!(&info[20..30], b"C:\\?\\o.exe");
    }

    #[test]
    fn names_receivers_can_open_pass_and_others_say_why_not() {
        assert!(check_name("Omuzaki").is_ok());
        assert!(check_name("Omuzaki (clean feed) #2").is_ok());
        for bad in ["", "a\\b", "Omuzaki\u{e9}", "tab\there", &"x".repeat(256)] {
            assert!(matches!(check_name(bad), Err(StreamError::Config(_))), "{bad:?}");
        }
        assert!(check_name(&"x".repeat(255)).is_ok());
    }

    #[test]
    fn off_windows_the_kind_points_at_the_route_that_works() {
        if cfg!(windows) {
            assert!(available().is_ok());
        } else {
            let Err(StreamError::Unavailable { hint, .. }) = available() else { panic!("Spout is Windows-only") };
            assert!(hint.contains("Syphon"), "{hint}");
        }
    }
}
