use serde::{Deserialize, Serialize};
use stackable_operator::schemars::{self, JsonSchema};

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NifiTls {
    /// This only affects client connections and is used to
    /// control which certificate the servers should use to
    /// authenticate themselves against the client.
    #[serde(default = "NifiTls::default_server_secret_class")]
    pub server_secret_class: String,
}

impl Default for NifiTls {
    fn default() -> Self {
        Self {
            server_secret_class: Self::default_server_secret_class(),
        }
    }
}

impl NifiTls {
    fn default_server_secret_class() -> String {
        "tls".to_owned()
    }
}
