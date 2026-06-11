//! NiFi repository storage layout: which repositories exist and how they map to volumes.

use stackable_operator::k8s_openapi::api::core::v1::VolumeMount;
use strum::{Display, EnumIter};

use crate::crd::constants::NIFI_PVC_STORAGE_DIRECTORY;

#[derive(Debug, Display, EnumIter)]
pub enum NifiRepository {
    #[strum(serialize = "filebased")]
    Filebased,
    #[strum(serialize = "flowfile")]
    Flowfile,
    #[strum(serialize = "database")]
    Database,
    #[strum(serialize = "content")]
    Content,
    #[strum(serialize = "provenance")]
    Provenance,
    #[strum(serialize = "state")]
    State,
}

/// The repositories that are backed by a [`PersistentVolume`] and therefore need a volume mount
/// in both the prepare and the nifi container.
///
/// [`NifiRepository::Filebased`] is intentionally excluded: it is only mounted conditionally for
/// file-based authorization (see [`crate::security::authorization`]).
///
/// [`PersistentVolume`]: stackable_operator::k8s_openapi::api::core::v1::PersistentVolume
pub const PERSISTENT_REPOSITORIES: [NifiRepository; 5] = [
    NifiRepository::Flowfile,
    NifiRepository::Database,
    NifiRepository::Content,
    NifiRepository::Provenance,
    NifiRepository::State,
];

impl NifiRepository {
    pub fn repository(&self) -> String {
        format!("{}-repository", self)
    }

    pub fn mount_path(&self) -> String {
        format!("{NIFI_PVC_STORAGE_DIRECTORY}/{}", self)
    }

    /// The [`VolumeMount`] mounting this repository's volume into a container.
    pub fn volume_mount(&self) -> VolumeMount {
        VolumeMount {
            name: self.repository(),
            mount_path: self.mount_path(),
            ..VolumeMount::default()
        }
    }
}
