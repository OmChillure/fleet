//! Thin re-export so `use pages::workers::Workers as SelfHosted` still compiles.

#[allow(unused_imports)]
pub use crate::pages::runners::self_hosted::{SelfHosted as Workers, *};
