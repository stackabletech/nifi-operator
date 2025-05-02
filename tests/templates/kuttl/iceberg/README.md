The file `60_nifi-flow.json` was exported from the NiFi UI.

*However*, we need to update some stuff, such as adding S3 credentials and templating the namespace of MinIO.

TIP: I used `JSON: Sort Document` in VScode to somewhat have consistent formatting, which makes reading and diffs easier.

Notable the following diff has been made:

```diff
diff --git a/tests/templates/kuttl/iceberg/60_nifi-flow.json b/tests/templates/kuttl/iceberg/60_nifi-flow.json
index 09783fa..23c679f 100644
--- a/tests/templates/kuttl/iceberg/60_nifi-flow.json
+++ b/tests/templates/kuttl/iceberg/60_nifi-flow.json
@@ -160,7 +160,9 @@
                     "custom-signer-module-location": null,
                     "default-credentials": "false",
                     "profile-name": null,
-                    "Session Time": "3600"
+                    "Session Time": "3600",
+                    "Access Key": "admin",
+                    "Secret Key": "adminadmin"
                 },
                 "propertyDescriptors": {
                     "Access Key": {
@@ -483,7 +485,7 @@
                 "properties": {
                     "AWS Credentials Provider service": "d9e8d00a-c387-3064-add2-c6060f158ae7",
                     "hive-metastore-uri": "thrift://hive:9083",
-                    "s3-endpoint": "https://minio.kuttl-test-patient-tarpon.svc.cluster.local:9000",
+                    "s3-endpoint": "https://minio.${NAMESPACE}.svc.cluster.local:9000",
                     "s3-path-style-access": "true",
                     "warehouse-location": "s3a://demo/lakehouse"
                 },
```
