#!/bin/sh
# Enables CORS on the TOS bucket so the browser-based viewer can read it directly.
# Scope: localhost viewer origins only; GET/HEAD (read) + PUT (phase-2 rrd write-back).
# The bucket currently has NO CORS config (verified 2026-07-24), so nothing is overwritten.
set -eu
cd "$(dirname "$0")"

AK=$(cat secrets/tos_access_key)
SK=$(cat secrets/tos_secret_key)
BUCKET_HOST="physical-ai-rerun-test.tos-s3-cn-beijing.volces.com"

cat > /tmp/tos-cors.xml <<'EOF'
<CORSConfiguration><CORSRule><AllowedOrigin>http://127.0.0.1:9091</AllowedOrigin><AllowedOrigin>http://localhost:9091</AllowedOrigin><AllowedMethod>GET</AllowedMethod><AllowedMethod>HEAD</AllowedMethod><AllowedMethod>PUT</AllowedMethod><AllowedHeader>*</AllowedHeader><ExposeHeader>ETag</ExposeHeader><ExposeHeader>Content-Range</ExposeHeader><ExposeHeader>Content-Length</ExposeHeader><MaxAgeSeconds>3600</MaxAgeSeconds></CORSRule></CORSConfiguration>
EOF

MD5=$(openssl md5 -binary /tmp/tos-cors.xml | base64)
curl -s -o /dev/null -w "PutBucketCors: HTTP %{http_code}\n" \
    --aws-sigv4 "aws:amz:cn-beijing:s3" --user "$AK:$SK" \
    -X PUT -H "Content-MD5: $MD5" -H "Content-Type: application/xml" \
    --data-binary @/tmp/tos-cors.xml \
    "https://$BUCKET_HOST/?cors"

echo "verify preflight:"
curl -s -o /dev/null -D - -X OPTIONS \
    -H "Origin: http://127.0.0.1:9091" \
    -H "Access-Control-Request-Method: GET" \
    -H "Access-Control-Request-Headers: authorization,x-amz-date,x-amz-content-sha256,range" \
    "https://$BUCKET_HOST/dataset-1/so101-pick-place/meta/info.json" | grep -i "^HTTP\|access-control" || true
