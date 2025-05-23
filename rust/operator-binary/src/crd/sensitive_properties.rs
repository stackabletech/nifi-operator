use serde::{Deserialize, Serialize};
use snafu::Snafu;
use stackable_operator::schemars::{self, JsonSchema};

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display(
        "The sensitive properties algorithm '{algorithm}' is not supported in NiFi 2.X.X. Please see https://nifi.apache.org/docs/nifi-docs/html/administration-guide.html#updating-the-sensitive-properties-algorithm on how to upgrade the algorithm."
    ))]
    UnsupportedSensitivePropertiesAlgorithm { algorithm: String },
}

/// These settings configure the encryption of sensitive properties in NiFi processors.
/// NiFi supports encrypting sensitive properties in processors as they are written to disk.
/// You can configure the encryption algorithm and the key to use.
/// You can also let the operator generate an encryption key for you.
#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NifiSensitivePropertiesConfig {
    /// A reference to a Secret. The Secret needs to contain a key `nifiSensitivePropsKey`.
    /// If `autoGenerate` is false and this object is missing, the Operator will raise an error.
    /// The encryption key needs to be at least 12 characters long.
    pub key_secret: String,

    /// Whether to generate the `keySecret` if it is missing.
    /// Defaults to `false`.
    #[serde(default)]
    pub auto_generate: bool,

    /// This is setting the `nifi.sensitive.props.algorithm` property in NiFi.
    /// This setting configures the encryption algorithm to use to encrypt sensitive properties.
    /// Valid values are:
    ///
    /// `nifiPbkdf2AesGcm256` (the default value),
    /// `nifiArgon2AesGcm256`,
    ///
    /// The following algorithms are deprecated and will be removed in future versions:
    ///
    /// `nifiArgon2AesGcm128`,
    /// `nifiBcryptAesGcm128`,
    /// `nifiBcryptAesGcm256`,
    /// `nifiPbkdf2AesGcm128`,
    /// `nifiScryptAesGcm128`,
    /// `nifiScryptAesGcm256`.
    ///
    /// Learn more about the specifics of the algorithm parameters in the
    /// [NiFi documentation](https://nifi.apache.org/docs/nifi-docs/html/administration-guide.html#property-encryption-algorithms).
    pub algorithm: Option<NifiSensitiveKeyAlgorithm>,
}

#[derive(strum::Display, Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NifiSensitiveKeyAlgorithm {
    // supported in v2
    #[strum(serialize = "NIFI_PBKDF2_AES_GCM_256")]
    NifiPbkdf2AesGcm256,
    // supported in v2
    #[strum(serialize = "NIFI_ARGON2_AES_GCM_256")]
    NifiArgon2AesGcm256,
    // Deprecated in v1 -> can be removed when 1.x.x is no longer supported
    #[strum(serialize = "NIFI_BCRYPT_AES_GCM_128")]
    NifiBcryptAesGcm128,
    // Deprecated in v1 -> can be removed when 1.x.x is no longer supported
    #[strum(serialize = "NIFI_BCRYPT_AES_GCM_256")]
    NifiBcryptAesGcm256,
    // Deprecated in v1 -> can be removed when 1.x.x is no longer supported
    #[strum(serialize = "NIFI_PBKDF2_AES_GCM_128")]
    NifiPbkdf2AesGcm128,
    // Deprecated in v1 -> can be removed when 1.x.x is no longer supported
    #[strum(serialize = "NIFI_ARGON2_AES_GCM_128")]
    NifiArgon2AesGcm128,
    // Deprecated in v1 -> can be removed when 1.x.x is no longer supported
    #[strum(serialize = "NIFI_SCRYPT_AES_GCM_128")]
    NifiScryptAesGcm128,
    // Deprecated in v1 -> can be removed when 1.x.x is no longer supported
    #[strum(serialize = "NIFI_SCRYPT_AES_GCM_256")]
    NifiScryptAesGcm256,
}

impl NifiSensitiveKeyAlgorithm {
    /// Checks if the used encryption algorithm is supported or deprecated.
    /// Will warn for deprecation and error out for missing support.
    pub fn check_for_nifi_version(&self, product_version: &str) -> Result<(), Error> {
        let algorithm = self.to_string();

        match self {
            // Allowed and supported in NiFi 1.x.x and 2.x.x
            NifiSensitiveKeyAlgorithm::NifiPbkdf2AesGcm256
            | NifiSensitiveKeyAlgorithm::NifiArgon2AesGcm256 => {}
            // All others are deprecated in 1.x.x and removed in 2.x.x
            // see https://nifi.apache.org/docs/nifi-docs/html/administration-guide.html#property-encryption-algorithms
            _ => {
                if product_version.starts_with("1.") {
                    tracing::warn!(
                        "You are using a deprecated sensitive properties algorithm '{algorithm}'. Please update to '{pbkd}' or '{argon}'.",
                        pbkd = NifiSensitiveKeyAlgorithm::NifiPbkdf2AesGcm256,
                        argon = NifiSensitiveKeyAlgorithm::NifiArgon2AesGcm256
                    )
                } else {
                    return Err(Error::UnsupportedSensitivePropertiesAlgorithm { algorithm });
                }
            }
        }

        Ok(())
    }
}

impl Default for NifiSensitiveKeyAlgorithm {
    fn default() -> Self {
        Self::NifiArgon2AesGcm256
    }
}
