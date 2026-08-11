# Manager of Google Cloud storage

Manage files of Google Cloud storage.

## Requirements

-   Docker
-   mise
-   Google Cloud Account

## Usage

### Normalize filenames to NFC

Run below command.

```sh
mise run normalize [project] [bucket]
```

### Upload files

Move files to `uploads/[bucket]` directory and run below command.

```sh
mise run upload [project]
```

### Recovery after a forced termination

The normalize and upload workflows catch Ctrl-C (`Interrupt`) and termination signals and attempt to roll back the transaction. A forced kill (`SIGKILL`),
container or host termination, or a second interruption during rollback cannot be caught. These events can leave temporary Cloud Storage objects or locally
normalized filenames behind.

Before retrying a stopped run:

1.  Stop other runs for the same bucket and inspect Cloud Storage for normalize objects ending in `.task-googlecloud-<token>` and upload objects below
   `.task-googlecloud-staging/<token>/`.
2.  For normalize, inspect both the temporary object and its normalized final path. Verify the source, destination, object generation, and contents before
   moving a temporary object back or keeping/removing a final object. Do not overwrite an object created by another run.
3.  For upload, inspect both the final object and the abandoned staging prefix. Verify their generation and contents before removing staging objects, and
   do not delete data belonging to another run.
4.  Restore any locally normalized filename in `uploads/` to its pre-run name, using the operation's input list or a backup to confirm the original name.
5.  After all temporary objects, final objects, and local names have been reconciled, rerun the original `mise run normalize` or `mise run upload` command.

## Development

1.  Run command to start a container.

    ```sh
    docker compose build
    docker compose run --rm app /bin/bash
    ```

2.  Edit files.

3.  Run command to stop the container.

    ```sh
    docker compose down
    ```

## Author

naoigcat

## License

MIT
