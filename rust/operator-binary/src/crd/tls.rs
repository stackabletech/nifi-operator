use std::str::FromStr;

use serde::{Deserialize, Serialize};
use stackable_operator::{
    constant,
    schemars::{self, JsonSchema},
    v2::types::kubernetes::SecretClassName,
};

constant!(DEFAULT_SERVER_SECRET_CLASS: SecretClassName = "tls");

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NifiTls {
    /// This only affects client connections and is used to
    /// control which certificate the servers should use to
    /// authenticate themselves against the client.
    #[serde(default = "NifiTls::default_server_secret_class")]
    pub server_secret_class: SecretClassName,
}

impl Default for NifiTls {
    fn default() -> Self {
        Self {
            server_secret_class: Self::default_server_secret_class(),
        }
    }
}

impl NifiTls {
    /// Serde default for `serverSecretClass`. Kept as a function because
    /// `#[serde(default = "...")]` requires a function path.
    fn default_server_secret_class() -> SecretClassName {
        DEFAULT_SERVER_SECRET_CLASS.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constants() {
        // Test that dereferencing the constants does not panic.
        let _ = *DEFAULT_SERVER_SECRET_CLASS;
    }
}
