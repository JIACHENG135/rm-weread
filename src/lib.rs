//! rm-weread: a native WeRead (微信读书) client for reMarkable tablets.
//! See docs/design.md for the architecture and phased plan.

pub mod content;
pub mod cookie;
pub mod login;
pub mod session;
pub mod paginate;
pub mod reader;
pub mod reader_state;
pub mod shelf;
pub mod skill_gateway;
pub mod weread_sign;
pub mod xhtml;
