import unittest

from prune_spec import ALLOWED_PATHS, prune


class PruneSpecTests(unittest.TestCase):
    def test_context_read_allowlist_drops_multipart_upload_operation(self):
        context_path = "/api/v1/appsec/apps/{app_id}/context"
        spec = {
            "paths": {
                path: (
                    {
                        "get": {"responses": {"200": {"description": "ok"}}},
                        "post": {
                            "requestBody": {
                                "content": {"multipart/form-data": {"schema": {}}}
                            },
                            "responses": {"201": {"description": "created"}},
                        },
                    }
                    if path == context_path
                    else {}
                )
                for path in ALLOWED_PATHS
            },
            "components": {"schemas": {}},
        }

        result = prune(spec)

        self.assertEqual(set(result["paths"][context_path]), {"get"})


if __name__ == "__main__":
    unittest.main()
