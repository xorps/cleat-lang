//! The `alloc` items every module uses (the core is `no_std`, so `std`'s prelude isn't there).

pub use alloc::borrow::ToOwned;
pub use alloc::boxed::Box;
pub use alloc::collections::{BTreeMap, BTreeSet};
pub use alloc::rc::Rc;
pub use alloc::string::{String, ToString};
pub use alloc::vec::Vec;
pub use alloc::{format, vec};
