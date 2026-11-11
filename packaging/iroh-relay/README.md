# Private Iroh relay

A generic `iroh-relay` behind an existing Traefik instance, with access
requests approved by the operator. The relay stores nothing and sees only
ciphertext.

## Prerequisites

- Docker Compose.
- A Traefik `websecure` entrypoint with a `le` certificate resolver and the
  external `traefik_net` network.
- A DNS-only (grey cloud) A record for the relay hostname pointing at the
  server, so Let's Encrypt HTTP-01 can complete.

## Deploy

```sh
mkdir -p /srv/iroh-relay/state
cd /srv/iroh-relay
cp access.py compose.yaml config.toml .env.example .
cp .env.example .env          # set RELAY_DOMAIN and the operator UID/GID
chown -R "$(id -u):$(id -g)" state
printf '[]\n' > state/approved.json
printf '[]\n' > state/pending.json
docker compose up -d
```

`ACCESS_UID`/`ACCESS_GID` must own `state/`; the access container runs as that
user. The relay container runs as `nobody` with a read-only filesystem and no
capabilities. No ports are published on the host.

## Verify

```sh
curl -fsS https://relay.example.com/ping -o /dev/null -w '%{http_code}\n'
curl -fsS https://relay.example.com/generate_204 -o /dev/null -w '%{http_code}\n'
```

Both should return success. The relay rejects every client that is not in
`state/approved.json`.

## Access requests

Corcho devices request access from **Settings → Peer sync → Set up…**. The
request lands in `state/pending.json`; nothing is allowed until the operator
approves it.

```sh
docker compose exec access python /app/access.py pending
docker compose exec access python /app/access.py approve <endpoint-id>
docker compose exec access python /app/access.py list
docker compose exec access python /app/access.py revoke <endpoint-id>
```

Approval takes effect on the next connection; no restart is needed. The
operator can also add ids directly to `state/approved.json` while the service
is stopped.

The relay calls `POST http://access:8090/authorize` with
`X-Iroh-Endpoint-Id`; the access service is not exposed through Traefik except
for `/access/request` and `/access/status`.
