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

    def test_incident_schema_omits_internal_declaration_actor(self):
        incident_ref = {"$ref": "#/components/schemas/IncidentPublic"}
        spec = {
            "paths": {
                path: (
                    {"get": {"responses": {"200": {"content": {"application/json": {"schema": incident_ref}}}}}}
                    if path == "/api/v1/incidents/{incident_id}"
                    else {}
                )
                for path in ALLOWED_PATHS
            },
            "components": {
                "schemas": {
                    "IncidentPublic": {
                        "properties": {
                            "id": {"type": "string"},
                            "declaration_actor_kind": {"$ref": "#/components/schemas/DeclarationActorKind"},
                        },
                        "required": ["id", "declaration_actor_kind"],
                    },
                    "DeclarationActorKind": {"enum": ["internal", "user"], "type": "string"},
                }
            },
        }

        result = prune(spec)

        schemas = result["components"]["schemas"]
        self.assertEqual(set(schemas), {"IncidentPublic"})
        self.assertEqual(schemas["IncidentPublic"]["required"], ["id"])


if __name__ == "__main__":
    unittest.main()
