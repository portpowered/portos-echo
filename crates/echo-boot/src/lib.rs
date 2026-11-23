//! Boot supervision policy. All time, processes and filesystem access are supplied by the host.

pub mod layout;
mod selection;
mod supervisor;

pub use selection::{Digest, Selection, Slot};
pub use supervisor::{ChildStatus, HostOps, Log, Stop, supervise};

pub type Result<T> = std::result::Result<T, &'static str>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    Device,
    Core,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Identity {
    pub uid: u32,
    pub real_gid: u32,
    pub effective_gid: u32,
    pub groups: &'static [u32],
    pub domain: Option<&'static str>,
}

impl Role {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Device => "device",
            Self::Core => "core",
        }
    }

    #[must_use]
    pub const fn identity(self) -> Identity {
        match self {
            Self::Device => Identity {
                uid: 1000,
                real_gid: 3990,
                effective_gid: 1000,
                groups: &[1000, 1005, 3003, 1004, 1002],
                domain: Some("u:r:hassmic:s0"),
            },
            Self::Core => Identity {
                uid: 9999,
                real_gid: 3990,
                effective_gid: 3990,
                groups: &[3990, 3003, 1004],
                domain: None,
            },
        }
    }
}
