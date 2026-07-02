use serde::{Deserialize, Serialize};
use stackable_operator::schemars::{self, JsonSchema};

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
    /// Learn more about the specifics of the algorithm parameters in the
    /// [NiFi documentation](https://nifi.apache.org/docs/nifi-docs/html/administration-guide.html#property-encryption-algorithms).
    pub algorithm: Option<NifiSensitiveKeyAlgorithm>,
}

#[derive(
    strum::Display, Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize,
)]
#[serde(rename_all = "camelCase")]
pub enum NifiSensitiveKeyAlgorithm {
    #[strum(serialize = "NIFI_PBKDF2_AES_GCM_256")]
    NifiPbkdf2AesGcm256,

    #[default]
    #[strum(serialize = "NIFI_ARGON2_AES_GCM_256")]
    NifiArgon2AesGcm256,
}
