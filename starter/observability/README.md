# Observability stack

Grafana for a full_stack_engine app: every request and cron run as a trace,
request rate / error rate / latency per route, a list of recent failures that
opens straight into the failing trace, and an alert the moment anything errors.

It is a separate deployment from the app — its own compose project, on the
same server or another one.

## Deploy with Dokploy

1. **Create a Compose application** pointing at this repository, with the
   compose path set to this folder's `docker-compose.yml`.
2. **Environment** tab:
   ```bash
   OTLP_TOKEN=<openssl rand -hex 32>
   GRAFANA_ADMIN_PASSWORD=<a strong password>
   GRAFANA_ROOT_URL=https://grafana.example.com
   ```
3. **Domains** tab — two domains, HTTPS on:

   | Domain | Service | Port |
   |---|---|---|
   | `grafana.example.com` | `grafana` | 3000 |
   | `otel.example.com` | `collector` | 4318 |

4. **Deploy.** Then set on the **app**:
   ```bash
   OTEL_EXPORTER_OTLP_ENDPOINT=https://otel.example.com
   OTEL_EXPORTER_OTLP_HEADERS=authorization=Bearer%20<OTLP_TOKEN>   # %20 is the space
   ```
   The app needs the framework's `otel` feature (the starter has it). No other
   change, no SDK, no code.

Log in to Grafana as `admin`. The home page is the **App overview** dashboard.

Any other reverse proxy works the same way: route the two hostnames to those
two services. Nothing in the compose file publishes a port, on purpose — a
published port would be a second, unencrypted way in that skips the proxy's
TLS, and Docker-published ports also bypass a host firewall such as UFW.

## What you get

| | |
|---|---|
| **App overview** dashboard | Requests/s, share of 5xx, p95 latency and failure count; the same per route; cron job runs; a table of recent failures. |
| **Recent failures** table | Each row opens the trace: the route, the status, the full cause chain, and every log line written while the request ran. |
| **Errors in the app** alert | Fires when anything ends in an error — a 5xx, a failed cron run, a panic — including a route's first-ever failure. Expect it within about 3 minutes of the error. |
| **Explore → Tempo** | Search any trace, e.g. `{ span.request_id = "<x-request-id a user quoted>" }` or `{ span.url.path = "/checkout" }`. A trace becomes searchable about 30 seconds after the request. |

**One thing to set up by hand: where the alert goes.** Grafana → Alerting →
Contact points → edit the default one (email, Slack, Discord, a webhook, …).
Until then it fires visibly in Grafana but notifies no one.

## How it works

```
app ──HTTPS──► reverse proxy ──► collector ──► Tempo ──► stores traces
               (TLS)             (checks the      │
                                  token)          └─ derives metrics ──► Prometheus
                                                                            │
                   reverse proxy ◄── Grafana ◄── traces (Tempo) + metrics (Prometheus)
```

The app only exports traces. Request rate, error rate and latency are computed
**from** those traces by Tempo's metrics generator, so they cannot disagree
with them, and a point on a latency graph links to an example trace.

Errors are not a separate signal: a 5xx response, a failed cron run and a
panic each arrive as a span with status *Error*. A 4xx is the client's mistake
and is not counted.

Two networks keep the parts apart. Tempo and Prometheus sit only on
`internal`, which has no route out and which nothing outside the stack can
reach — neither has authentication of its own. The collector and Grafana are
also on `edge`, which the reverse proxy reaches and which lets Grafana send
alert notifications out.

## Operating it

- **Changing the Grafana password.** `GRAFANA_ADMIN_PASSWORD` is used on the
  first start only; after that the password lives in the `grafana` volume.
  Change it in Grafana (profile → change password), or:
  `docker compose exec grafana grafana cli admin reset-admin-password <new>`.
- **Retention**: 14 days of traces; metrics for 30 days or 5 GB, whichever
  comes first. `TRACE_RETENTION`, `METRICS_RETENTION`,
  `METRICS_RETENTION_SIZE`. Tempo has no size cap; its disk use grows with
  traffic, so check the `tempo` volume after the first few weeks.
- **Memory**: limits of 512 MB (collector), 2 GB (Tempo), 1 GB (Prometheus)
  and 512 MB (Grafana), adjustable with `*_MEMORY`. The collector refuses data
  rather than exceed its limit.
- **Backups**: the `grafana` volume holds users, contact points and any
  dashboards you made; `tempo` and `prometheus` hold data you can afford to
  lose. Back up `grafana` if nothing else.
- **Logs** are capped at 30 MB per container.
- **On the app, keep `TELEMETRY_SAMPLE_RATIO` at 1.0** unless the traffic
  forces otherwise. Sampling drops whole traces, errors included, and every
  number here is computed from what arrives.
- **Set `SERVICE_VERSION`** on the app to the deployed commit, so a new error
  can be tied to the deploy that introduced it.
- **Several apps** can share one stack: each reports under its own
  `SERVICE_NAME`, and the dashboard's *Service* selector switches between them.

## Upgrading

Images are pinned. Bump the tags in `docker-compose.yml` and redeploy. Tempo's
configuration changes between major versions (`tempo.yaml` is written for
3.x), so read its release notes before crossing one.

## Running it locally

```bash
cp .example.env .env    # OTLP_TOKEN, GRAFANA_ADMIN_PASSWORD, GRAFANA_ROOT_URL=http://localhost:3000
docker compose -f docker-compose.yml -f docker-compose.local.yml up -d
```

Grafana on http://localhost:3000; run the app with
`OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318` and the token header.
`docker-compose.local.yml` publishes the two ports on 127.0.0.1 and lets
Grafana's cookie work over plain HTTP — never use it on a server.
