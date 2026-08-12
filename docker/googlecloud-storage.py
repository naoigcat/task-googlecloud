#!/usr/bin/env python3

import argparse
import http.client
import json
import os
import subprocess
import sys
from urllib.parse import quote, urlencode


API_HOST = "storage.googleapis.com"
API_ROOT = "/storage/v1"
UPLOAD_ROOT = "/upload/storage/v1"
CHUNK_SIZE = 1024 * 1024
HTTPS_TIMEOUT = 30


class StorageApiError(RuntimeError):
    def __init__(self, status, message):
        super().__init__(message)
        self.status = status


def access_token():
    return subprocess.check_output(
        ["gcloud", "auth", "print-access-token"], text=True
    ).strip()


def encoded_path(value):
    return quote(value, safe="")


def api_path(root, path_parts, query=None):
    path = root + "".join(f"/{encoded_path(part)}" for part in path_parts)
    return f"{path}?{urlencode(query)}" if query else path


def response_error(status, body):
    try:
        payload = json.loads(body)
        return payload.get("error", {}).get("message", body.decode(errors="replace"))
    except (json.JSONDecodeError, UnicodeDecodeError):
        return body.decode(errors="replace")


def request(method, path, token, headers=None, body=None):
    request_headers = {"Authorization": f"Bearer {token}", "Accept": "application/json"}
    request_headers.update(headers or {})
    connection = http.client.HTTPSConnection(API_HOST, timeout=HTTPS_TIMEOUT)
    try:
        connection.request(method, path, body=body, headers=request_headers)
        response = connection.getresponse()
        response_body = response.read()
    finally:
        connection.close()

    if not 200 <= response.status < 300:
        raise StorageApiError(response.status, response_error(response.status, response_body))

    return json.loads(response_body) if response_body else {}


def object_metadata(bucket, object_name, generation=None, token=None):
    token = token or access_token()
    query = {"generation": generation} if generation is not None else None
    path = api_path(API_ROOT, ["b", bucket, "o", object_name], query)
    return request("GET", path, token)


def created_message(bucket, object_name, metadata):
    print(f"Created: gs://{bucket}/{object_name}#{metadata['generation']}")


def copy_object(
    source_bucket,
    source_object,
    target_bucket,
    target_object,
    source_generation=None,
    destination_generation=None,
    token=None,
):
    token = token or access_token()
    query = {}
    if source_generation is not None:
        query["sourceGeneration"] = source_generation
        query["ifSourceGenerationMatch"] = source_generation
    if destination_generation is not None:
        query["ifGenerationMatch"] = destination_generation
    path_parts = [
        "b",
        source_bucket,
        "o",
        source_object,
        "rewriteTo",
        "b",
        target_bucket,
        "o",
        target_object,
    ]

    # Rewrite tokens let the service continue large transfers without restarting them.
    while True:
        response = request("POST", api_path(API_ROOT, path_parts, query), token)
        if response["done"]:
            return response["resource"]
        query["rewriteToken"] = response["rewriteToken"]


def delete_object(bucket, object_name, generation, token=None):
    token = token or access_token()
    path = api_path(
        API_ROOT,
        ["b", bucket, "o", object_name],
        {"generation": generation},
    )
    request("DELETE", path, token)


def upload_file(source, bucket, object_name, token=None):
    token = token or access_token()
    query = urlencode(
        {"uploadType": "media", "name": object_name, "ifGenerationMatch": "0"}
    )
    path = f"{UPLOAD_ROOT}/b/{encoded_path(bucket)}/o?{query}"
    size = os.path.getsize(source)
    connection = http.client.HTTPSConnection(API_HOST, timeout=HTTPS_TIMEOUT)
    try:
        connection.putrequest("POST", path)
        connection.putheader("Authorization", f"Bearer {token}")
        connection.putheader("Content-Type", "application/octet-stream")
        connection.putheader("Content-Length", str(size))
        connection.endheaders()
        with open(source, "rb") as file:
            while chunk := file.read(CHUNK_SIZE):
                connection.send(chunk)
        response = connection.getresponse()
        response_body = response.read()
    finally:
        connection.close()

    if not 200 <= response.status < 300:
        raise StorageApiError(response.status, response_error(response.status, response_body))

    return json.loads(response_body)


def list_objects(bucket, token=None):
    token = token or access_token()
    page_token = None
    while True:
        query = {"maxResults": "1000"}
        if page_token:
            query["pageToken"] = page_token
        path = api_path(API_ROOT, ["b", bucket, "o"], query)
        payload = request("GET", path, token)
        for item in payload.get("items", []):
            print(f"gs://{bucket}/{item['name']}")
        page_token = payload.get("nextPageToken")
        if not page_token:
            return


def parse_args():
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="operation", required=True)

    upload = subparsers.add_parser("upload")
    upload.add_argument("--file", required=True)
    upload.add_argument("--bucket", required=True)
    upload.add_argument("--object", required=True)

    copy = subparsers.add_parser("copy")
    copy.add_argument("--source-bucket", required=True)
    copy.add_argument("--source-object", required=True)
    copy.add_argument("--target-bucket", required=True)
    copy.add_argument("--target-object", required=True)
    copy.add_argument("--source-generation")
    copy.add_argument("--destination-generation")

    move = subparsers.add_parser("move")
    move.add_argument("--source-bucket", required=True)
    move.add_argument("--source-object", required=True)
    move.add_argument("--target-bucket", required=True)
    move.add_argument("--target-object", required=True)
    move.add_argument("--source-generation")

    for operation in ("stat", "state"):
        command = subparsers.add_parser(operation)
        command.add_argument("--bucket", required=True)
        command.add_argument("--object", required=True)
        command.add_argument("--generation")

    delete = subparsers.add_parser("delete")
    delete.add_argument("--bucket", required=True)
    delete.add_argument("--object", required=True)
    delete.add_argument("--generation", required=True)

    list_command = subparsers.add_parser("list")
    list_command.add_argument("--bucket", required=True)

    return parser.parse_args()


def run(args):
    token = access_token()
    if args.operation == "upload":
        created_message(args.bucket, args.object, upload_file(args.file, args.bucket, args.object, token))
    elif args.operation == "copy":
        metadata = copy_object(
            args.source_bucket,
            args.source_object,
            args.target_bucket,
            args.target_object,
            args.source_generation,
            args.destination_generation,
            token,
        )
        created_message(args.target_bucket, args.target_object, metadata)
    elif args.operation == "move":
        if args.source_generation is None:
            args.source_generation = object_metadata(args.source_bucket, args.source_object, token=token)["generation"]
        metadata = copy_object(
            args.source_bucket,
            args.source_object,
            args.target_bucket,
            args.target_object,
            args.source_generation,
            "0",
            token,
        )
        delete_object(args.source_bucket, args.source_object, args.source_generation, token)
        created_message(args.target_bucket, args.target_object, metadata)
    elif args.operation == "stat":
        print(f"Generation: {object_metadata(args.bucket, args.object, args.generation, token)['generation']}")
    elif args.operation == "state":
        try:
            object_metadata(args.bucket, args.object, args.generation, token)
        except StorageApiError as error:
            if error.status == 404:
                print("missing")
                return
            raise
        print("present")
    elif args.operation == "delete":
        delete_object(args.bucket, args.object, args.generation, token)
    elif args.operation == "list":
        list_objects(args.bucket, token)


def main():
    try:
        run(parse_args())
    except (OSError, subprocess.CalledProcessError, StorageApiError, KeyError) as error:
        print(f"Cloud Storage API request failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
