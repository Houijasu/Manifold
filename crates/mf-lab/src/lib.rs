//! Research experiments, ablations, and reproducibility tooling.

#[cfg(feature = "corrhist-regression")]
pub mod corpus;
#[cfg(feature = "corrhist-regression")]
pub mod corrhist;
#[cfg(feature = "corrhist-regression")]
pub mod regression;
#[cfg(feature = "corrhist-regression")]
pub mod report;
#[cfg(feature = "corrhist-regression")]
pub mod reservoir;
