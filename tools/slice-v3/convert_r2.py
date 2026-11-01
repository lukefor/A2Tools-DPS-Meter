"""Converts this shard's stored slices in R2 to slice v3 (zstd), in place.

Run by .github/workflows/slice-v3-convert.yml. Every slice key is listed,
sorted, cut to LIMIT if set, and this shard takes the keys that hash to it. Each
batch is downloaded, converted and verified by the `slice-v3` binary, and only
a verified slice is put back: under the same key, as application/zstd, after a
server-side copy of the original to backup/slice-v2/<key>.

The repo is public, so the job log shows counts only, never log ids: some logs
are private. The id,sha256,bytes CSV of replaced slices and the failures go to
R2 under migrations/slice-v3/<RUN>/, for the site to apply to D1.
"""

import collections
import concurrent.futures as cf
import hashlib
import os
import shutil
import subprocess
import sys
import tempfile

import boto3
from botocore.config import Config

BUCKET = "aion2-dps-meter"
ENDPOINT = "https://267ccf7a7e2dd817fe78e4f85a47756e.r2.cloudflarestorage.com"
SUFFIX = "/slice.a2es.gz"
BATCH = 200

SHARD = int(os.environ["SHARD"])
SHARDS = int(os.environ["SHARDS"])
LIMIT = int(os.environ.get("LIMIT") or 0)
DRY_RUN = os.environ.get("DRY_RUN") == "true"
RUN = os.environ["RUN"]
BIN = os.environ["BIN"]

s3 = boto3.client(
    "s3", endpoint_url=ENDPOINT, region_name="auto",
    aws_access_key_id=os.environ["R2_ACCESS_KEY_ID"],
    aws_secret_access_key=os.environ["R2_SECRET_ACCESS_KEY"],
    config=Config(max_pool_connections=32, retries={"max_attempts": 8, "mode": "adaptive"},
                  request_checksum_calculation="when_required",
                  response_checksum_validation="when_required"))


def slice_keys():
    keys = []
    for page in s3.get_paginator("list_objects_v2").paginate(Bucket=BUCKET, Prefix="logs/"):
        keys += [o["Key"] for o in page.get("Contents", []) if o["Key"].endswith(SUFFIX)]
    keys.sort()
    if LIMIT:
        keys = keys[:LIMIT]
    # By the key itself, not its place in the list: each runner lists the
    # bucket on its own, and a log uploaded meanwhile moved every later place,
    # so shards overlapped and missed slices (run 38873218544).
    return [k for k in keys if int(hashlib.sha256(k.encode()).hexdigest(), 16) % SHARDS == SHARD]


def log_id(key):
    return key[len("logs/"):-len(SUFFIX)]


def download(key, folder):
    obj = s3.get_object(Bucket=BUCKET, Key=key)
    with open(os.path.join(folder, log_id(key)), "wb") as f:
        f.write(obj["Body"].read())
    return obj["ETag"]


def replace(key, path):
    s3.copy_object(Bucket=BUCKET, Key="backup/slice-v2/" + key,
                   CopySource={"Bucket": BUCKET, "Key": key})
    with open(path, "rb") as f:
        s3.put_object(Bucket=BUCKET, Key=key, Body=f.read(), ContentType="application/zstd")


def main():
    keys = slice_keys()
    print(f"shard {SHARD}/{SHARDS}: {len(keys)} slices{' (dry run)' if DRY_RUN else ''}")
    counts = collections.Counter()
    reasons = collections.Counter()
    bytes_in = bytes_out = 0
    csv_rows, failed = [], []
    pool = cf.ThreadPoolExecutor(24)

    for start in range(0, len(keys), BATCH):
        batch = keys[start:start + BATCH]
        work = tempfile.mkdtemp()
        src, dst = os.path.join(work, "in"), os.path.join(work, "out")
        os.makedirs(src)
        got = {}
        for key, fut in [(k, pool.submit(download, k, src)) for k in batch]:
            try:
                fut.result()
                got[log_id(key)] = key
            except Exception as e:
                counts["failed"] += 1
                reasons["download"] += 1
                failed.append(f"{log_id(key)} download {type(e).__name__}")

        out = subprocess.run([BIN, src, dst], capture_output=True, text=True, check=True).stdout
        to_put = []
        for line in out.splitlines():
            name, status, *rest = line.split(" ")
            if status == "converted":
                to_put.append((name, int(rest[0]), int(rest[1]), rest[2]))
            elif status == "skipped":
                counts["skipped"] += 1
            else:
                counts["failed"] += 1
                reasons[rest[0] if rest else "?"] += 1
                failed.append(line)

        if DRY_RUN:
            for name, n_in, n_out, sha in to_put:
                counts["would convert"] += 1
                bytes_in += n_in
                bytes_out += n_out
        else:
            futs = [(row, pool.submit(replace, got[row[0]], os.path.join(dst, row[0]))) for row in to_put]
            for (name, n_in, n_out, sha), fut in futs:
                try:
                    fut.result()
                    counts["converted"] += 1
                    bytes_in += n_in
                    bytes_out += n_out
                    csv_rows.append(f"{name},{sha},{n_out}")
                except Exception as e:
                    counts["failed"] += 1
                    reasons["put"] += 1
                    failed.append(f"{name} put {type(e).__name__}")
        shutil.rmtree(work)
        done = min(start + BATCH, len(keys))
        print(f"  {done}/{len(keys)} {dict(counts)}", flush=True)

    prefix = f"migrations/slice-v3/{RUN}/"
    if csv_rows:
        s3.put_object(Bucket=BUCKET, Key=f"{prefix}shard-{SHARD:02}.csv",
                      Body=("id,sha256,bytes\n" + "\n".join(csv_rows) + "\n").encode(),
                      ContentType="text/csv")
    if failed:
        s3.put_object(Bucket=BUCKET, Key=f"{prefix}failed-{SHARD:02}.txt",
                      Body=("\n".join(failed) + "\n").encode(), ContentType="text/plain")
    pct = f" ({100 * bytes_out / bytes_in:.1f}% of before)" if bytes_in else ""
    print(f"shard {SHARD}: {dict(counts)}; {bytes_in >> 20} MB -> {bytes_out >> 20} MB{pct}")
    if reasons:
        print(f"failure reasons: {dict(reasons)}")
    if counts["failed"]:
        sys.exit(1)


if __name__ == "__main__":
    main()
