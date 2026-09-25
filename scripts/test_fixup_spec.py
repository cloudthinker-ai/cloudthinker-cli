import unittest

from fixup_spec import fixup


class FixupSpecTests(unittest.TestCase):
    def test_distinct_inline_bounds_do_not_share_generated_type(self):
        schemas = {
            "Large": {"type": "object", "properties": {"content": {"type": "string", "title": "Content", "maxLength": 200}}},
            "Small": {"type": "object", "properties": {"content": {"type": "string", "title": "Content", "maxLength": 20}}},
        }
        paths = {f"/{name}": {"get": {"responses": {"200": {"content": {"application/json": {"schema": {"$ref": f"#/components/schemas/{name}"}}}}}}} for name in schemas}
        result = fixup({"paths": paths, "components": {"schemas": schemas}})
        schemas = result["components"]["schemas"]
        self.assertEqual(schemas["Large"]["properties"]["content"]["title"], "Large_content")
        self.assertEqual(schemas["Small"]["properties"]["content"]["title"], "Small_content")
        self.assertEqual(schemas["Large"]["properties"]["content"]["maxLength"], 200)
        self.assertEqual(schemas["Small"]["properties"]["content"]["maxLength"], 20)
        self.assertEqual(fixup(result), result)


if __name__ == "__main__":
    unittest.main()
