# Secret-Service SQLite Lifecycle

This folder manages schema evolution and deterministic scenarios for `skills/examples/secret-service`.

## Structure

- `schema/`: versioned SQL migrations (`0001_*.sql`, `0002_*.sql`, ...)
- `migrations.toml`: migration order + checksums
- `seeds/`: deterministic scenario data
- `snapshots/`: generated sqlite files for fast eval startup

## Commands (run from repo root)

Migrate a DB:

```bash
uv run --project skills/examples/secret-service python skills/examples/secret-service/scripts/db_migrate.py \
  --db skills/examples/secret-service/db/snapshots/dev.sqlite
```

Apply Operation Nightfall seed:

```bash
uv run --project skills/examples/secret-service python skills/examples/secret-service/scripts/db_seed.py \
  --db skills/examples/secret-service/db/snapshots/dev.sqlite \
  --scenario operation_nightfall
```

Build a fresh scenario DB in one step:

```bash
uv run --project skills/examples/secret-service python skills/examples/secret-service/scripts/db_build_scenario.py \
  --scenario operation_nightfall \
  --out skills/examples/secret-service/db/snapshots/operation_nightfall.sqlite \
  --force
```

Validate a DB has required tables:

```bash
uv run --project skills/examples/secret-service python skills/examples/secret-service/scripts/db_validate.py \
  --db skills/examples/secret-service/db/snapshots/operation_nightfall.sqlite
```
