# Governo Sombra transcripts

This service publishes searchable transcripts for the Governo Sombra podcast. It downloads one unprocessed episode each day, transcribes the audio with Whisper, and stores transcripts and book mentions in S3 compatible storage.

## Required environment variables

The web server and processing job use these variables:

- `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY` provide access to the transcript bucket.
- `AWS_REGION` sets the S3 region.
- `CLOUDFLARE_ENDPOINT` sets the S3 compatible endpoint.
- `OPENROUTER_API_KEY` is used for book extraction with `openai/gpt-5.6-luna`.
- `SENTRY_DSN` is optional. Error reporting is disabled when it is missing.
- `PROCESS_TOKEN` is optional. When set, it authorizes manual processing requests.

## Health checks

Use `GET /healthz` for Docker, Coolify, and uptime checks. The endpoint returns immediately and does not call the podcast feed or S3.

The home page loads the podcast feed through a ten minute cache. Feed requests have a timeout, and the server uses stale cached data when a refresh fails.

## Processing

The service schedules processing each day at 08:00 with a fixed UTC plus one hour offset. A failed or panicked run is logged without stopping the scheduler. Only one run can execute at a time.

You can start a manual run when `PROCESS_TOKEN` is configured:

```sh
curl -X POST \
  -H "Authorization: Bearer $PROCESS_TOKEN" \
  https://governosombra.duarteocarmo.com/process
```

The endpoint returns `409 Conflict` when another run is active.

## Coolify deployment

Set the Coolify health check path to `/healthz`. The Dockerfile also defines this health check, but a health check configured in Coolify can override the Dockerfile setting.

Enable Docker init support when the deployment configuration allows it. The included Compose file uses `init: true`, which reaps finished child processes from health checks and FFmpeg.
