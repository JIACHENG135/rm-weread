//! rm-weread: a native WeRead (微信读书) client for reMarkable tablets.
//! See docs/design.md for the architecture and phased plan.

pub mod content;
pub mod cookie;
pub mod layout;
pub mod login;
pub mod paginate;
pub mod pdfgen;
pub mod pipeline;
pub mod reader;
pub mod reader_state;
pub mod session;
pub mod shelf;
pub mod skill_gateway;
pub mod underlines;
pub mod weread_sign;
pub mod xhtml;
pub mod xochitl_doc;
