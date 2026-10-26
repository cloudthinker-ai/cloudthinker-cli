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

    def test_connection_paths_stay_read_only_and_drop_credential_fields(self):
        detail = "/api/v1/builtin-connection/{builtin_id}"
        retry = "/api/v1/builtin-connection/{builtin_id}/retry"
        mcp_list = "/api/v1/mcp-connection"
        conflict = {
            "409": {
                "content": {
                    "application/json": {
                        "schema": {"$ref": "#/components/schemas/ConnectionErrorResponse"}
                    }
                }
            }
        }

        def ok(name):
            return {
                "200": {
                    "content": {
                        "application/json": {"schema": {"$ref": f"#/components/schemas/{name}"}}
                    }
                },
                **conflict,
            }

        operations = {
            detail: {
                "get": {"responses": ok("BuiltinConnectionDetail")},
                "post": {"responses": {"202": {"description": "installed"}}},
                "delete": {"responses": {"204": {"description": "removed"}}},
            },
            retry: {"post": {"responses": ok("BuiltinConnectionDetail")}},
            mcp_list: {
                "get": {"responses": ok("MCPConnectionPublic")},
                "post": {"responses": {"201": {"description": "created"}}},
            },
        }
        spec = {
            "paths": {path: operations.get(path, {}) for path in ALLOWED_PATHS},
            "components": {
                "schemas": {
                    "BuiltinConnectionDetail": {
                        "properties": {
                            "id": {"type": "string"},
                            "env": {"type": "array"},
                            "tools": {"type": "array"},
                            "config": {"type": "object"},
                        },
                        "required": ["id", "env", "tools"],
                    },
                    "MCPConnectionPublic": {
                        "properties": {
                            "id": {"type": "string"},
                            "config": {"$ref": "#/components/schemas/ConnectionMCPConfigPublic"},
                        },
                        "required": ["id", "config"],
                    },
                    "ConnectionMCPConfigPublic": {"properties": {"headers": {"type": "object"}}},
                    "ConnectionErrorResponse": {"properties": {"error": {"type": "object"}}},
                }
            },
        }

        result = prune(spec)

        self.assertEqual(set(result["paths"][detail]), {"get"})
        self.assertEqual(set(result["paths"][mcp_list]), {"get"})
        self.assertEqual(set(result["paths"][retry]), {"post"})
        self.assertEqual(set(result["paths"][retry]["post"]["responses"]), {"200"})
        schemas = result["components"]["schemas"]
        self.assertEqual(set(schemas), {"BuiltinConnectionDetail", "MCPConnectionPublic"})
        self.assertEqual(set(schemas["BuiltinConnectionDetail"]["properties"]), {"id"})
        self.assertEqual(schemas["MCPConnectionPublic"]["required"], ["id"])

    def test_connection_enums_accept_values_this_cli_does_not_know(self):
        status_ref = {"$ref": "#/components/schemas/ConnectionTestStatus"}
        spec = {
            "paths": {
                path: (
                    {"get": {"responses": {"200": {"content": {"application/json": {"schema": status_ref}}}}}}
                    if path == "/api/v1/builtin-connection/{builtin_id}/test-status"
                    else {}
                )
                for path in ALLOWED_PATHS
            },
            "components": {
                "schemas": {
                    "ConnectionTestStatus": {
                        "properties": {"phase": {"$ref": "#/components/schemas/ConnectionTestPhase"}},
                        "required": ["phase"],
                    },
                    "ConnectionTestPhase": {
                        "enum": ["queued", "succeeded"],
                        "title": "ConnectionTestPhase",
                        "description": "Test phase.",
                        "type": "string",
                    },
                }
            },
        }

        result = prune(spec)

        self.assertEqual(
            result["components"]["schemas"]["ConnectionTestPhase"],
            {"title": "ConnectionTestPhase", "description": "Test phase.", "type": "string"},
        )


if __name__ == "__main__":
    unittest.main()
