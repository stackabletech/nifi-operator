//! The dereference step in the NifiCluster controller
//!
//! Fetches all Kubernetes objects referenced by the NifiCluster spec and returns
//! them in [`DereferencedObjects`].

use snafu::{ResultExt, Snafu};
use stackable_operator::{
    client::Client,
    commons::networking::DomainName,
    k8s_openapi::api::core::v1::Secret,
    v2::{
        controller_utils::{self, get_cluster_name, get_namespace},
        types::kubernetes::NamespaceName,
    },
};

use crate::{
    controller::build::resource::secret::build_oidc_admin_password_secret_name,
    crd::v1alpha1,
    security::{
        authentication::{self, DereferencedAuthenticationClasses},
        authorization::{self as authorization_mod, DereferencedAuthorization},
    },
};

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to get the namespace"))]
    GetNamespace { source: controller_utils::Error },

    #[snafu(display("failed to get the cluster name"))]
    GetClusterName { source: controller_utils::Error },

    #[snafu(display("failed to dereference NiFi authentication classes"))]
    DereferenceAuthenticationClasses { source: authentication::Error },

    #[snafu(display("failed to dereference NiFi authorization config"))]
    DereferenceAuthorization { source: authorization_mod::Error },

    #[snafu(display("failed to get the Secret {secret_name:?}"))]
    GetSecret {
        source: stackable_operator::client::Error,
        secret_name: String,
    },
}

type Result<T, E = Error> = std::result::Result<T, E>;

/// Kubernetes objects referenced from the [`v1alpha1::NifiCluster`] spec, already fetched.
pub struct DereferencedObjects {
    /// The namespace of the [`v1alpha1::NifiCluster`], parsed once here and reused everywhere.
    pub namespace: NamespaceName,
    /// The Kubernetes cluster domain, captured from the client here so the build step needs no
    /// client to assemble in-cluster DNS names.
    pub cluster_domain: DomainName,
    pub authentication_classes: DereferencedAuthenticationClasses,
    pub authorization: DereferencedAuthorization,
    /// The Secrets whose contents this operator generates, as currently stored in Kubernetes.
    pub existing_secrets: ExistingSecrets,
}

/// The Secrets whose contents this operator generates, as currently stored in Kubernetes.
///
/// Their contents are randomly generated at creation and can therefore not be rebuilt. They are
/// fetched here so that the build step can re-emit the existing contents unchanged, and only
/// generate fresh ones when a Secret is missing. See
/// [`build::resource::secret`](crate::controller::build::resource::secret).
#[derive(Clone, Debug, Default)]
pub struct ExistingSecrets {
    /// The sensitive-properties key Secret named by
    /// `spec.clusterConfig.sensitiveProperties.keySecret`, mounted by the NiFi Pods.
    pub sensitive_key: Option<Secret>,

    /// The admin password Secret for OIDC authentication, which this operator names itself.
    ///
    /// Fetched unconditionally because the authentication type is only resolved in the validate
    /// step; the build step emits it for OIDC authentication only.
    pub oidc_admin_password: Option<Secret>,
}

/// Fetches all Kubernetes objects referenced from the [`v1alpha1::NifiCluster`] spec.
pub async fn dereference(
    client: &Client,
    nifi: &v1alpha1::NifiCluster,
) -> Result<DereferencedObjects> {
    let namespace = get_namespace(nifi).context(GetNamespaceSnafu)?;

    let authentication_classes = DereferencedAuthenticationClasses::dereference(nifi, client)
        .await
        .context(DereferenceAuthenticationClassesSnafu)?;

    let authorization = DereferencedAuthorization::dereference(
        &nifi.spec.cluster_config.authorization,
        client,
        namespace.as_ref(),
    )
    .await
    .context(DereferenceAuthorizationSnafu)?;

    let cluster_name = get_cluster_name(nifi).context(GetClusterNameSnafu)?;
    let existing_secrets = ExistingSecrets {
        sensitive_key: get_secret_opt(
            client,
            &nifi.spec.cluster_config.sensitive_properties.key_secret,
            &namespace,
        )
        .await?,
        oidc_admin_password: get_secret_opt(
            client,
            &build_oidc_admin_password_secret_name(&cluster_name),
            &namespace,
        )
        .await?,
    };

    Ok(DereferencedObjects {
        namespace,
        cluster_domain: client.kubernetes_cluster_info.cluster_domain.clone(),
        authentication_classes,
        authorization,
        existing_secrets,
    })
}

/// Fetches the Secret with the given name, returning `None` if it does not exist.
async fn get_secret_opt(
    client: &Client,
    secret_name: &impl AsRef<str>,
    namespace: &NamespaceName,
) -> Result<Option<Secret>> {
    let secret_name = secret_name.as_ref();
    client
        .get_opt::<Secret>(secret_name, namespace.as_ref())
        .await
        .context(GetSecretSnafu { secret_name })
}
