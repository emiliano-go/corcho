# Encrypted 48h relay cache

A read-only Corcho peer that keeps a rolling copy of an encrypted workspace for
48 hours. It receives the **cache ticket** (read-only, no workspace key), so
its document and blob stores contain ciphertext only.

## Build

From the repository root:

```sh
docker build -f packaging/iroh-cache/Dockerfile -t corcho-cache:local .
```

The image includes the Qt runtime, so the container needs no host libraries.

## Run

```sh
mkdir -p /srv/iroh-cache/data
cd /srv/iroh-cache
cp compose.yaml .
printf 'CORCHO_CACHE_TICKET=%s\n' '<cache-ticket>' > .env
chmod 600 .env
chown -R "$(id -u):$(id -g)" data
docker compose up -d
```

Set `CACHE_UID`/`CACHE_GID` in `.env` if the data directory belongs to another
user. The cache needs outbound network access to the relay; it publishes no
ports.

## Approve the cache on the relay

The relay's access service must approve the cache endpoint id:

```sh
docker run --rm -v /srv/iroh-cache/data:/data corcho-cache:local \
  --print-endpoint-id --data-dir /data
```

Then, in the relay deployment:

```sh
docker compose exec access python /app/access.py approve <cache-endpoint-id>
```

Restart the cache once after approval.

## Expiry

`--ttl-hours 48` closes the endpoint, deletes `data/iroh/docs` and
`data/iroh/blobs`, and exits. Docker restarts it, so the cache re-syncs from
whichever peers are online. The endpoint key and logs stay, so the relay
approval survives.
