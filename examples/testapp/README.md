# testapp

Simple JWC example project with entities, controllers, and services.

## Run

```bash
jwc test
jwc migrate up
jwc run
```

Server default: http://0.0.0.0:8080

## Useful Commands

```bash
# create migration (two equivalent commands)
jwc migrate new init-db
jwc migrate add init-db

# apply pending migrations
jwc migrate up

# start server with request logs enabled
jwc run --request-logging
```

Request logging is disabled by default.
