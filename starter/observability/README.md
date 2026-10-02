# Observability stack

Grafana for a full_stack_engine app: every request and cron run as a trace,
request rate / error rate / latency per route, a list of recent failures that
opens straight into the failing trace, and an alert the moment anything errors.

It is a separate deployment from the app. Run it on any host the app can
reach — the same machine, or a small box of its own.

```bash
cp .example.env .env    # set OTLP_TOKEN and GRAFANA_ADMIN_PASSWORD
docker compose up -d
```

Then, on the **app**:

```bash
OTEL_EXPORTER_OTLP_ENDPOINT=https://otel.example.com          # where this stack's port 4318 is reachable
OTEL_EXPORTER_OTLP_HEADERS=authorization=Bearer%20<OTLP_TOKEN> # %20 is the space; keep it encoded
```

The app needs the framework's `otel` feature (the starter has it). Nothing else
changes in the app — no SDK, no code.

Open Grafana on port 3000 and log in as `admin`. The home page is the **App
overview** dashboard.

## What you get

| | |
|---|---|
| **App overview** dashboard | Requests/s, share of 5xx, p95 latency and failure count; the same per route; cron job runs; a table of recent failures. |
| **Recent failures** table | Each row opens the trace: the route, the status, the full cause chain, and every log line written while the request ran. |
| **Errors in the app** alert | Fires when anything ends in an error — a 5xx, a failed cron run, a panic — including a route's first-ever failure. Expect it within about 3 minutes of the error (export batching, metric generation, then a 1-minute evaluation). |
| **Explore → Tempo** | Search any trace, e.g. `{ span.request_id = "<x-request-id a user quoted>" }` or `{ span.url.path = "/checkout" }`. A trace becomes searchable about 30 seconds after the request. |

The one thing left to set up is **where the alert goes**: Grafana →
Alerting → Contact points → edit the default one (email, Slack, Discord, a
webhook, …). Until then it fires visibly in Grafana but notifies no one.

## How it works

```
app ──OTLP/HTTP + bearer token──► collector ──► Tempo ──► stores traces
                                  (checks the                │
                                   token)                    └─ derives metrics ──► Prometheus
                                                                                       │
                                     Grafana ◄──── traces (Tempo) + metrics (Prometheus)
```

The app only exports traces. The request-rate, error-rate and latency series
are computed **from** those traces by Tempo's metrics generator, so they cannot
disagree with them, and every point on a latency graph can link to an example
trace.

Errors are not a separate signal. A request answered with a 5xx, a cron run
that fails, and a panic each arrive as a span with status *Error* (see the
framework's `docs/observability.md`). A 4xx is the client's mistake and is not
counted.

Only the collector (4318) and Grafana (3000) are published. Tempo and
Prometheus have no authentication of their own and stay on the internal
network.

## In production

- **Put TLS in front of both published ports.** The bearer token and the
  Grafana login travel in every request; over plain HTTP across the internet
  they can be read. If a reverse proxy (Caddy, Traefik, nginx) on the same host
  terminates TLS, set `OTLP_BIND=127.0.0.1` and `GRAFANA_BIND=127.0.0.1` so the
  plain ports are not reachable from outside.
- **Keep `TELEMETRY_SAMPLE_RATIO` at 1.0 on the app** unless the traffic forces
  otherwise. Sampling drops whole traces, errors included, and the metrics are
  computed from what arrives — a ratio of 0.1 makes every number here a tenth of
  the truth.
- **Set `SERVICE_VERSION`** on the app to the deployed commit; every trace
  carries it, so a new error can be tied to the deploy that introduced it.
- **Retention** is 14 days of traces and 30 days of metrics. Change it with
  `TRACE_RETENTION` and `METRICS_RETENTION`. Data lives in Docker volumes and
  survives restarts and upgrades.
- **Several apps** can share one stack: each reports under its own
  `SERVICE_NAME`, and the dashboard's *Service* selector switches between them.

## Upgrading

Images are pinned. Bump the tags in `docker-compose.yml`, then
`docker compose pull && docker compose up -d`. Tempo's configuration changes
between major versions (`tempo.yaml` is written for 3.x), so read its release
notes before crossing one.

## Locally

The same compose file works on a laptop: start it, then run the app with
`OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318` and the token header.
