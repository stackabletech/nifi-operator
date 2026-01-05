use serde::{Deserialize, Serialize};
use stackable_operator::{
    commons::{cache::UserInformationCache, opa::OpaConfig},
    schemars::{self, JsonSchema},
};

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NifiAuthorization {
    Opa {
        #[serde(flatten)]
        opa: NifiOpaConfig,
    },
    SingleUser {},
    Standard {
        access_policy_provider: NifiAccessPolicyProvider,
    },
}

impl Default for NifiAuthorization {
    fn default() -> Self {
        Self::SingleUser {}
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NifiOpaConfig {
    #[serde(flatten)]
    pub opa: OpaConfig,
    #[serde(default)]
    pub cache: UserInformationCache,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NifiAccessPolicyProvider {
    FileBased { initia_admin_user: String },
}
