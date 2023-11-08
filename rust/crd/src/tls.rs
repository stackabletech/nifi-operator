use serde::{Deserialize, Serialize};
use stackable_operator::schemars::{self, JsonSchema};

const TLS_DEFAULT_SECRET_CLASS: &str = "tls";

#[derive(Clone, Deserialize, Debug, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NifiTls {
    /// The <https://docs.stackable.tech/secret-operator/stable/secretclass.html> to use for
    /// http communication.
    /// (mandatory). This setting controls:
    /// - Which cert the http servers should use to authenticate themselves towards users
    /// Defaults to `tls`
    #[serde(default = "http_tls_default")]
    pub http_secret_class: String,
}

/// Default TLS settings. Http communication default to "tls" secret class.
pub fn default_nifi_tls() -> NifiTls {
    NifiTls {
        http_secret_class: http_tls_default(),
    }
}

/// Helper method to provide defaults in the CRDs and tests
pub fn http_tls_default() -> String {
    TLS_DEFAULT_SECRET_CLASS.into()
}
