import contextlib
import importlib.util
import io
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch
from urllib.parse import parse_qs, urlsplit


MODULE_PATH = Path(__file__).parents[1] / "docker" / "googlecloud-storage.py"
SPEC = importlib.util.spec_from_file_location("googlecloud_storage", MODULE_PATH)
storage = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(storage)


class GooglecloudStorageTest(unittest.TestCase):
    def setUp(self):
        self.responses = []
        self.requests = []
        self.original_request = storage.request
        storage.request = self.record_request

    def tearDown(self):
        storage.request = self.original_request

    def record_request(self, method, path, token):
        self.requests.append((method, path, token))
        return self.responses.pop(0)

    def test_rewrite_resumes_with_the_recorded_source_generation(self):
        self.responses = [
            {"done": False, "rewriteToken": "continue token"},
            {"done": True, "resource": {"generation": "456"}},
        ]

        result = storage.copy_object(
            "bucket",
            "folder*?[]#/source",
            "bucket",
            "folder*?[]#/target",
            source_generation="123",
            destination_generation="0",
            token="token",
        )

        self.assertEqual({"generation": "456"}, result)
        self.assertEqual(2, len(self.requests))
        for method, path, token in self.requests:
            self.assertEqual("POST", method)
            self.assertEqual("token", token)
            self.assertIn("/rewriteTo/", path)
            self.assertIn("folder%2A%3F%5B%5D%23%2F", path)

        first_query = parse_qs(urlsplit(self.requests[0][1]).query)
        self.assertEqual(
            {
                "sourceGeneration": ["123"],
                "ifSourceGenerationMatch": ["123"],
                "ifGenerationMatch": ["0"],
            },
            first_query,
        )
        second_query = parse_qs(urlsplit(self.requests[1][1]).query)
        self.assertEqual(["continue token"], second_query["rewriteToken"])

    def test_delete_selects_the_recorded_generation(self):
        self.responses = [{}]

        storage.delete_object("bucket", "folder*?[]#/target", "456", token="token")

        method, path, token = self.requests[0]
        self.assertEqual("DELETE", method)
        self.assertEqual("token", token)
        self.assertIn("folder%2A%3F%5B%5D%23%2F", path)
        self.assertEqual({"generation": ["456"]}, parse_qs(urlsplit(path).query))

    def test_list_preserves_special_names_across_pages(self):
        self.responses = [
            {"items": [{"name": "folder*?[]#/é.txt"}], "nextPageToken": "next"},
            {"items": [{"name": "plain.txt"}]},
        ]

        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            storage.list_objects("bucket", token="token")

        self.assertEqual("gs://bucket/folder*?[]#/é.txt\ngs://bucket/plain.txt\n", output.getvalue())
        self.assertEqual(2, len(self.requests))
        self.assertEqual({"maxResults": ["1000"]}, parse_qs(urlsplit(self.requests[0][1]).query))
        self.assertEqual(
            {"maxResults": ["1000"], "pageToken": ["next"]},
            parse_qs(urlsplit(self.requests[1][1]).query),
        )

    def test_list_treats_an_empty_bucket_as_successful(self):
        self.responses = [{}]

        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            storage.list_objects("bucket", token="token")

        self.assertEqual("", output.getvalue())
        self.assertEqual(1, len(self.requests))

    def test_list_propagates_missing_and_forbidden_bucket_errors(self):
        for status in (404, 403):
            with self.subTest(status=status):
                def raise_error(_method, _path, _token):
                    raise storage.StorageApiError(status, "listing failed")

                storage.request = raise_error
                with self.assertRaises(storage.StorageApiError) as context:
                    storage.list_objects("bucket", token="token")
                self.assertEqual(status, context.exception.status)

    def test_parse_args_accepts_a_leading_hyphen_in_an_object_name(self):
        argv = [
            "googlecloud-storage.py",
            "state",
            "--bucket",
            "bucket",
            "--object=-foo*",
        ]
        with patch.object(storage.sys, "argv", argv):
            args = storage.parse_args()

        self.assertEqual("-foo*", args.object)


class HttpsConnectionTimeoutTest(unittest.TestCase):
    def test_request_sets_a_socket_timeout(self):
        connection = Mock()
        connection.getresponse.return_value.status = 200
        connection.getresponse.return_value.read.return_value = b"{}"

        with patch.object(storage.http.client, "HTTPSConnection", return_value=connection) as constructor:
            storage.request("GET", "/path", "token")

        constructor.assert_called_once_with(storage.API_HOST, timeout=30)

    def test_upload_sets_a_socket_timeout(self):
        connection = Mock()
        connection.getresponse.return_value.status = 200
        connection.getresponse.return_value.read.return_value = b"{}"

        with tempfile.NamedTemporaryFile() as source:
            source.write(b"content")
            source.flush()
            with patch.object(storage.http.client, "HTTPSConnection", return_value=connection) as constructor:
                storage.upload_file(source.name, "bucket", "object", token="token")

        constructor.assert_called_once_with(storage.API_HOST, timeout=30)


if __name__ == "__main__":
    unittest.main()
