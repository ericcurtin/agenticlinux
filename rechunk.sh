#!/bin/bash
# Runs in the Dockerfile's rechunk stage with the finished image at /rootfs;
# writes it to /out as an OCI layout, repacked into one layer per group of
# packages. Docker's overlay2 store refuses images of over 125 layers, and
# Fedora's bootc images alone have 257.
set -euxo pipefail

rpm-ostree compose build-chunked-oci --bootc --format-version=2 \
  --max-layers=96 --rootfs=/rootfs \
  --label ostree.bootable=true --label "ostree.linux=$(ls /rootfs/usr/lib/modules)" \
  ${VERSION:+--label "org.opencontainers.image.version=$VERSION"} \
  --output oci-archive:/tmp/image.ociarchive
mkdir /out
tar -xf /tmp/image.ociarchive -C /out
rm /tmp/image.ociarchive

# rpm-ostree writes no command, environment or stop signal: restore the base
# images'. Also set the manifest's media type, which it leaves out, and the
# name docker load tags the image with.
case "$VARIANT" in
  centos-*) config='{"Cmd": ["/sbin/init"], "StopSignal": "SIGRTMIN+3", "Env":
    ["PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin", "container=oci"]}' ;;
  *) config='{"Cmd": ["/usr/bin/bash"]}' ;;
esac
CONFIG="$config" NAME="docker.io/library/agenticlinux:$VARIANT" python3 - <<'EOF'
import hashlib, json, os

def path(desc):
    return os.path.join("/out/blobs", *desc["digest"].split(":"))

def load(desc):
    with open(path(desc)) as f:
        obj = json.load(f)
    os.remove(path(desc))
    return obj

def store(desc, obj):
    data = json.dumps(obj, separators=(",", ":")).encode()
    desc["digest"] = "sha256:" + hashlib.sha256(data).hexdigest()
    desc["size"] = len(data)
    with open(path(desc), "wb") as f:
        f.write(data)

with open("/out/index.json") as f:
    index = json.load(f)
[desc] = index["manifests"]
manifest = load(desc)
config = load(manifest["config"])
config["config"].update(json.loads(os.environ["CONFIG"]))
store(manifest["config"], config)
store(desc, {"schemaVersion": 2, "mediaType": desc["mediaType"], **manifest})
desc["annotations"] = {**desc.get("annotations", {}), "io.containerd.image.name": os.environ["NAME"]}
with open("/out/index.json", "w") as f:
    json.dump(index, f)
print(f"{len(manifest['layers'])} layers")
EOF
