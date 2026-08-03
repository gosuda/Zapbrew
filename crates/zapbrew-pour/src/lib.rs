//! Unpack, relocation (text + ELF + Mach-O), and link/unlink for Homebrew bottles.

mod archive;
mod error;
mod link;
mod relocate;
mod types;

pub use archive::unpack;
pub use error::PourError;
pub use link::{link, plan_unlink, unlink};
pub use relocate::relocate;
pub use types::{LinkOptions, LinkReport, RelocationReport, UnlinkReport};
