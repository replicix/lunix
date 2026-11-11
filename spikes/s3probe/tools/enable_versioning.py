#!/usr/bin/env python3
"""Enable bucket versioning on an S3 endpoint with a hand-signed (SigV4) PutBucketVersioning.

Used only by run-minio.sh against the throwaway local MinIO, to probe a *versioned* bucket
(ADR-2: a versioned authority bucket qualifies only if the concurrent-CAS probe passes).
Reads ENDPOINT, BUCKET, AK, SK, REGION from the environment; prints nothing secret.
"""
import datetime, hashlib, hmac, os, sys, urllib.request, urllib.parse

ep, bucket = os.environ["ENDPOINT"], os.environ["BUCKET"]
ak, sk, region = os.environ["AK"], os.environ["SK"], os.environ.get("REGION", "us-east-1")
body = b'<VersioningConfiguration xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><Status>Enabled</Status></VersioningConfiguration>'
host = urllib.parse.urlparse(ep).netloc
now = datetime.datetime.now(datetime.timezone.utc)
amz, day = now.strftime("%Y%m%dT%H%M%SZ"), now.strftime("%Y%m%d")
ph = hashlib.sha256(body).hexdigest()
hdrs = {"host": host, "x-amz-content-sha256": ph, "x-amz-date": amz}
signed = ";".join(sorted(hdrs))
canon = "\n".join(["PUT", "/" + bucket, "versioning=", "".join(f"{k}:{hdrs[k]}\n" for k in sorted(hdrs)), signed, ph])
scope = f"{day}/{region}/s3/aws4_request"
sts = "\n".join(["AWS4-HMAC-SHA256", amz, scope, hashlib.sha256(canon.encode()).hexdigest()])
h = lambda k, m: hmac.new(k, m.encode(), hashlib.sha256).digest()
key = h(h(h(h(("AWS4" + sk).encode(), day), region), "s3"), "aws4_request")
sig = hmac.new(key, sts.encode(), hashlib.sha256).hexdigest()
req = urllib.request.Request(f"{ep}/{bucket}?versioning", data=body, method="PUT")
for k, v in hdrs.items():
    if k != "host":
        req.add_header(k, v)
req.add_header("Authorization", f"AWS4-HMAC-SHA256 Credential={ak}/{scope}, SignedHeaders={signed}, Signature={sig}")
try:
    with urllib.request.urlopen(req, timeout=20) as r:
        print("versioning enabled, HTTP", r.status)
except urllib.error.HTTPError as e:
    print("PutBucketVersioning failed, HTTP", e.code)
    sys.exit(1)
