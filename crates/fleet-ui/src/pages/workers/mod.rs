//! Thin re-export so `use pages::workers::Workers` still compiles.

pub use crate::pages::runners::self_hosted::SelfHosted as Workers;
