The file `60_nifi-flow.json` was exported from the NiFi UI.

*However*, we need to update some stuff, such as adding S3 credentials and templating the namespace of MinIO.

TIP: I used `JSON: Sort Document` in VScode to somewhat have consistent formatting, which makes reading and diffs easier.

Notable the following diff has been made (may not be up to date!):

```diff
diff --git a/tests/templates/kuttl/iceberg-rest/60_nifi-flow.json b/tests/templates/kuttl/iceberg-rest/60_nifi-flow.json
index eb64241..6ead26e 100644
--- a/tests/templates/kuttl/iceberg-rest/60_nifi-flow.json
+++ b/tests/templates/kuttl/iceberg-rest/60_nifi-flow.json
@@ -331,7 +331,9 @@
                 },
                 "properties": {
                     "Authentication Strategy": "BASIC_CREDENTIALS",
-                    "Endpoint URL": "https://minio.kuttl-test-dear-bug.svc.cluster.local:9000",
+                    "Access Key ID": "admin",
+                    "Secret Access Key": "adminadmin",
+                    "Endpoint URL": "https://minio.${NAMESPACE}.svc.cluster.local:9000",
                     "Path Style Access": "true",
                     "Client Region": "us-east1"
                 },
```
