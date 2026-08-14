# Manager of Google Cloud Storage

Manage Google Cloud Storage object names and uploads with a Rust CLI.

## Requirements

-   Docker
-   mise
-   Google Cloud account

## Usage

### Normalize filenames to NFC

Run:

```sh
mise run normalize [project] [bucket]
```

### Upload files

Move files to `uploads/[bucket]` and run:

```sh
mise run upload [project]
```

### Authentication lifecycle

Authentication is stored only in the temporary `googlecloud` container. The
`mise run normalize` and `mise run upload` tasks remove that container when they
finish, so credentials are not reused by later runs and no logout command is
provided. A development shell does not tear the container down on its own, so
run `docker compose down` after using it.

## Development

The application container contains the pinned Rust toolchain and the compiled
`task-googlecloud` binary. Start a development shell with:

```sh
docker compose build app
docker compose run --rm app /bin/bash
```

`--rm` removes only the application container. Discard the `googlecloud`
container, and with it any credentials obtained from the shell, with:

```sh
docker compose down
```

Run the local verification tasks:

```sh
mise run fmt-check
mise run clippy
mise run audit
mise run deny
mise run test
mise run markdownlint
```

Integration tests are split by responsibility under `tests/`.

The CLI uses the Google Cloud Storage JSON API directly. Authentication is
performed with the `gcloud` CLI in the dedicated Cloud SDK container, and the
application connects to it over an ephemeral, host-verified SSH channel.

## Recovery after an interrupted or failed run

The normalize and upload workflows handle Ctrl-C (`SIGINT`) and termination
signals and attempt to roll back the transaction. A forced kill (`SIGKILL`),
container or host termination, or an interruption during rollback cannot be
handled. These events can leave temporary Cloud Storage objects or locally
normalized filenames behind. The workflows also stop for manual recovery when
a remote failure leaves an object whose ownership cannot be established safely.

Before retrying a stopped run:

1.  Stop other runs for the same bucket and inspect Cloud Storage for the
    `.task-googlecloud-lock` object, normalize objects ending in
    `.task-googlecloud-<token>`, and upload objects below
    `.task-googlecloud-staging/<token>/`. Every writer touching the bucket must
    honor the lock object; after a forced termination, remove the lock only
    after confirming that no run is active and recording its current
    generation.
2.  For normalize, inspect both the temporary object and its normalized final
    path. Verify the source, destination, object generation, and contents
    before moving a temporary object back or keeping/removing a final object.
    Do not overwrite an object created by another run.
3.  For upload, inspect both the final object and the abandoned staging prefix.
    Verify their generation and contents before removing staging objects, and
    do not delete data belonging to another run.
4.  Restore any locally normalized filename in `uploads/` to its pre-run name,
    using the operation's input list or a backup to confirm the original name.
5.  After all temporary objects, final objects, and local names have been
    reconciled, rerun the original `mise run normalize` or `mise run upload`
    command.

## Author

naoigcat

## License

MIT
