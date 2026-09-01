//! OpenSubsonic 1.16.1 client and provider-neutral music models.

pub mod client;
pub mod models;
mod wire;

pub use crate::auth::{Credentials, ProfileId};
pub use client::{ApiClient, ApiError, AudioStream, NetActivity, OpenSubsonicClient};
pub use models::*;
