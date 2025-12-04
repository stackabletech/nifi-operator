# Examples

## Overview

This note provides some explanatory information when running the EntraID example.

## Cluster

Create a new local cluster (e.g. with [Kind](https://kind.sigs.k8s.io/docs/user/quick-start/) and the [stackablectl tool](https://github.com/stackabletech/stackablectl)).
This creates a cluster named `stackable-data-platform`.
Install the operators required by the example.

```text
kind create cluster --name stackable-data-platform
stackablectl operator install commons secret listener nifi
```

## Prerequisites

This example assumes that an EntraID backend is available and that an Application (in this example, Nifi-Entra-Test) has been configured with a web redirect URI.

Create a dedicated namespace in which to run the example:

```text
kubectl create namespace nifi
```

Apply a secret containing the following fields necessary for EntraID connectivity:

```yaml
---
apiVersion: v1
kind: Secret
metadata:
  name: oidc-secret
stringData:
  auth.endpoint: https://login.microsoftonline.com
  directory.id: <DIRECTORY-ID>
  client.id: <CLIENT-ID>
  client.secret: <CLIENT-SECRET>
  filter.prefix: <FILTER-PREFIX> # e.g. Nifi-Entra
  initial.admin: <INIT-ADMIN> # an existing EntraID user
  discovery.url: https://login.microsoftonline.com/<DIRECTORY-ID>/v2.0/.well-known/openid-configuration
```

Apply the Nifi cluster resource:

```text
kubectl apply -f examples/entra_nifi.yaml -n nifi
```

## Usage

Once the cluster is running, you will need to make a note of the listener endpoint.
This can be found by inspecting the listener class:

```text
kubectl get listeners/test-nifi-node -n nifi -o yaml | yq '[.status][0] | ("https://" + .ingressAddresses[0].address + ":" + .nodePorts.https)'
```

which yields e.g.

```text
https://172.19.0.3:31131
```

The web endpoint for app running against Entra needs to be updated with this endpoint as the prefix i.e.

![EntraID Web URI](entra-redirect-uri.png)

Paste this endpoint into the browser and you will be directed to the Azure portal login portal (to enter the credentials for the user designated as the intiial admin) and then redirected back to the Nifi UI.
The UI opens up on a writable canvas, in this case with the UUID `ea060c65-019a-1000-766b-0854b414d37e`:

![Nifi canvas](canvas.png)

The initial admin has immediate access as the static `authorizations.xml` file provided via the ConfigMap defined this:

```xml
<policy identifier="c8d5a9ba-0199-1000-0000-00003d66cc46" resource="/data/process-groups/root" action="W">
    <user identifier="6996a00e-bf7f-4ae2-8dc1-b668161e8ae0"/>
</policy>
```

and the `root` part of this has been updated with the actual root process group:

```xml
<policy identifier="c8d5a9ba-0199-1000-0000-00003d66cc46" resource="/data/process-groups/ea060c65-019a-1000-766b-0854b414d37e" action="W">
    <user identifier="6996a00e-bf7f-4ae2-8dc1-b668161e8ae0"/>
</policy>
```

This requires that the following be set:

```yaml
configOverrides:
    nifi.properties:
    ...
    nifi.process.group.root.placeholder: "root"
```

so that it is clear which placeholder - if any - should be patched.
