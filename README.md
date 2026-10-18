# lastmile

A self-hosted list of the packages a household is expecting: who is
delivering each one, where it is, and the code to give the courier or the
locker. A proof of concept for a homelab; see [docs/PLAN.md](docs/PLAN.md) for
where it is going.

![Packages](tests/screenshots/packages.png)

| On a phone | Dark mode |
|---|---|
| ![Phone](tests/screenshots/packages-phone.png) | ![Dark](tests/screenshots/packages-dark-violet.png) |

## Status

**M0 – Skeleton.** The stack shared with
[dominium](https://github.com/PedDavid/dominium) and
[clavium](https://github.com/PedDavid/clavium) (Rust, axum, askama, Tailwind
v4, Basecoat, htmx, OIDC), the SQLite database and its migrations, the
packages list, the Helm chart and CI. Packages can't be added from the UI yet.

## Running it

```sh
make demo   # sample packages in an in-memory database, no login
```

`--demo` loads made-up packages into a temporary in-memory database and, unless
OIDC is configured, lets every visitor in as an admin.

For real use, the app needs a writable directory for its database and an OIDC
provider:

```sh
lastmile \
  --database /data/lastmile.db \
  --public-url https://parcels.example.com \
  --oidc-issuer https://auth.example.com/application/o/lastmile/ \
  --oidc-client-id lastmile \
  --cookie-key-file /etc/lastmile/cookie-key
```

Every flag can also be set with a `LASTMILE_*` environment variable
(`lastmile --help` lists them). The UI listens on `:8080`; `/metrics`,
`/healthz` and `/readyz` on `:9090`, without authentication, so that port
should stay inside the cluster.

## Kubernetes

The Helm chart in `deploy/helm/lastmile` runs one replica (strategy
`Recreate`) with the database on a `ReadWriteOnce` PVC mounted at `/data`. The
PVC is kept on `helm uninstall`; back it up with the rest of your volumes.

```sh
helm install lastmile deploy/helm/lastmile \
  --set publicUrl=https://parcels.example.com \
  --set oidc.issuer=https://auth.example.com/application/o/lastmile/
```

## Development

```sh
make check        # fmt, clippy, tests (what CI runs)
make css          # rebuild assets/dist/app.css (Tailwind standalone CLI)
make helm-lint    # needs helm and yq
make visual-docker         # screenshot tests, in CI's container
make visual-docker-update  # rewrite the baselines in tests/screenshots/
```

**Screenshots** (`tests/visual.rs`,
[playwright-rs](https://github.com/padamson/playwright-rust)) drive Chromium
against the `--demo` router in-process, with the clock pinned
(`demo_app` in `tests/common/mod.rs`), and compare with
`tests/screenshots/*.png`. Fonts and antialiasing differ between machines, so
they run in the same container image locally (`make visual-docker`) and on CI
(the `visual` job): `mcr.microsoft.com/playwright`, at the Playwright version
playwright-rs drives, with its browsers and fonts. Baselines rendered on any
machine with Docker match CI. On a failure, each `-actual.png` is kept next to its
baseline and CI uploads them as the `screenshots` artifact.
